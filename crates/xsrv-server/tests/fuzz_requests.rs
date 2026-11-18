// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! Sends random and mutated requests to the server. The server must stay alive and keep serving
//! a well-behaved client. The generator is seeded, so a failure can be repeated.
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tokio::runtime::Builder;
use tokio::task::LocalSet;
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::rust_connection::{DefaultStream, RustConnection};
use xsrv_server::display::Display;
use xsrv_server::x11;

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: u64) -> u64 {
        self.next() % limit
    }
}

fn start_server(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("xsrv-fuzz-{}-{name}", std::process::id()));
    let display = Display::new(x11::SCREEN_WIDTH, x11::SCREEN_HEIGHT);
    let (ready_tx, ready_rx) = mpsc::channel();
    let server_path = path.clone();
    thread::spawn(move || {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        LocalSet::new().block_on(&runtime, async move {
            let listener = x11::bind(&server_path).unwrap();
            ready_tx.send(()).unwrap();
            let _ = x11::serve(listener, display).await;
        });
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    path
}

fn raw_connection(path: &PathBuf, little_endian: bool) -> UnixStream {
    let mut stream = UnixStream::connect(path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    let mut setup = vec![if little_endian { b'l' } else { b'B' }, 0];
    let (major, minor) = if little_endian {
        (11u16.to_le_bytes(), 0u16.to_le_bytes())
    } else {
        (11u16.to_be_bytes(), 0u16.to_be_bytes())
    };
    setup.extend_from_slice(&major);
    setup.extend_from_slice(&minor);
    setup.extend_from_slice(&[0; 6]);
    stream.write_all(&setup).unwrap();
    let mut head = [0u8; 8];
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.read_exact(&mut head).unwrap();
    let extra = if little_endian {
        u16::from_le_bytes([head[6], head[7]])
    } else {
        u16::from_be_bytes([head[6], head[7]])
    };
    let mut body = vec![0u8; usize::from(extra) * 4];
    stream.read_exact(&mut body).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(1)))
        .unwrap();
    stream
}

/// One request: a real opcode with a random body, and a length field that is right, too small
/// or too large.
fn random_request(random: &mut Random, little_endian: bool) -> Vec<u8> {
    let opcode = if random.below(4) == 0 {
        128 + random.below(16) as u8
    } else {
        1 + random.below(127) as u8
    };
    let words = random.below(12) as usize;
    let mut bytes = vec![0u8; 4 + words * 4];
    for byte in bytes.iter_mut().skip(1) {
        *byte = match random.below(4) {
            0 => 0,
            1 => 0xFF,
            _ => random.next() as u8,
        };
    }
    bytes[0] = opcode;
    let length = match random.below(8) {
        0 => (words as u16 + 1).wrapping_sub(random.below(3) as u16),
        1 => words as u16 + 1 + random.below(3) as u16,
        _ => words as u16 + 1,
    };
    let encoded = if little_endian {
        length.to_le_bytes()
    } else {
        length.to_be_bytes()
    };
    bytes[2..4].copy_from_slice(&encoded);
    bytes
}

fn assert_still_serving(path: &PathBuf, round: u32) {
    let stream = UnixStream::connect(path).unwrap();
    let (stream, _) = DefaultStream::from_unix_stream(stream).unwrap();
    let conn = RustConnection::connect_to_stream(stream, 0).unwrap_or_else(|error| {
        panic!("round {round}: the server does not accept clients: {error}")
    });
    conn.get_input_focus()
        .unwrap()
        .reply()
        .unwrap_or_else(|error| panic!("round {round}: the server does not answer: {error}"));
}

#[test]
fn random_requests_do_not_stop_the_server() {
    let path = start_server("random");
    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    for round in 0..120 {
        let little_endian = random.below(5) != 0;
        let mut stream = raw_connection(&path, little_endian);
        for _ in 0..(1 + random.below(40)) {
            let request = random_request(&mut random, little_endian);
            if stream.write_all(&request).is_err() {
                break;
            }
            let mut sink = [0u8; 4096];
            let _ = stream.read(&mut sink);
        }
        drop(stream);
        if round % 20 == 19 {
            assert_still_serving(&path, round);
        }
    }
    assert_still_serving(&path, 120);
}

#[test]
fn garbage_bytes_do_not_stop_the_server() {
    let path = start_server("garbage");
    let mut random = Random(0xDEAD_BEEF_CAFE_F00D);
    for round in 0..50 {
        let mut stream = raw_connection(&path, true);
        let noise: Vec<u8> = (0..random.below(600))
            .map(|_| random.next() as u8)
            .collect();
        let _ = stream.write_all(&noise);
        let mut sink = [0u8; 4096];
        let _ = stream.read(&mut sink);
        drop(stream);
        if round % 25 == 24 {
            assert_still_serving(&path, round);
        }
    }
    assert_still_serving(&path, 100);
}

#[test]
fn bad_connection_setups_do_not_stop_the_server() {
    let path = start_server("setup");
    let mut random = Random(0x1234_5678_9ABC_DEF1);
    for round in 0..100 {
        let mut stream = UnixStream::connect(&path).unwrap();
        let noise: Vec<u8> = (0..random.below(40)).map(|_| random.next() as u8).collect();
        let _ = stream.write_all(&noise);
        drop(stream);
        if round % 25 == 24 {
            assert_still_serving(&path, round);
        }
    }
    assert_still_serving(&path, 100);
}
