// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! Measures the time from an X11 request to the screen message at a page, with a real server
//! process and a WebSocket over TCP. It shows the delays that `make bench` cannot see: the wait
//! for the end of a burst, and the network settings of the page connection.
//!
//! Usage: `make bench-latency`, or `latency <path of the xsrv-web program>`.
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, CreateGCAux, CreateWindowAux, WindowClass};
use x11rb::rust_connection::{DefaultStream, RustConnection};
use x11rb::wrapper::ConnectionExt as _;

type Conn = RustConnection<DefaultStream>;
type Failure = Box<dyn std::error::Error>;

const ROUNDS: usize = 100;
const PAUSE: Duration = Duration::from_millis(30);
const QUIET: Duration = Duration::from_millis(300);
const TIME_LIMIT: Duration = Duration::from_secs(2);
const OPCODE_BINARY: u8 = 2;

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn main() -> Result<(), Failure> {
    let program = std::env::args()
        .nth(1)
        .ok_or("usage: latency <path of the xsrv-web program>")?;
    let dir = std::env::temp_dir().join(format!("xsrv-latency-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let outcome = run(&program, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    outcome
}

fn run(program: &str, dir: &Path) -> Result<(), Failure> {
    let socket = dir.join("X1");
    let port = free_port()?;
    let _server = Server(
        Command::new(program)
            .env("XSRV_ADDR", format!("127.0.0.1:{port}"))
            .env("XSRV_X11_SOCKET", &socket)
            .env("XSRV_WM", "0")
            .env_remove("XSRV_TOKEN")
            .env_remove("XSRV_VIDEO")
            .stdout(Stdio::null())
            .spawn()?,
    );
    wait_for(|| socket.exists() && TcpStream::connect(("127.0.0.1", port)).is_ok())?;

    let (stream, _) = DefaultStream::from_unix_stream(UnixStream::connect(&socket)?)?;
    let conn = RustConnection::connect_to_stream(stream, 0)?;
    let (window, gc) = open_window(&conn)?;
    let tiles = open_page(port)?;
    while tiles.recv_timeout(QUIET).is_ok() {}

    println!(
        "{:<28} {:>8} {:>8} {:>8}",
        "case", "mean ms", "p95 ms", "max ms"
    );
    let text = |round: usize| [b'a' + (round % 2) as u8];
    report(
        "one tile, with a reply",
        measure(&tiles, 1, |round| {
            conn.image_text8(window, gc, 40, 100, &text(round))?;
            conn.sync()?;
            Ok(())
        })?,
    );
    report(
        "one tile, no reply",
        measure(&tiles, 1, |round| {
            conn.image_text8(window, gc, 40, 100, &text(round))?;
            conn.flush()?;
            Ok(())
        })?,
    );
    report(
        "two tiles, no reply",
        measure(&tiles, 2, |round| {
            conn.image_text8(window, gc, 40, 100, &text(round))?;
            conn.image_text8(window, gc, 40, 300, &text(round))?;
            conn.flush()?;
            Ok(())
        })?,
    );
    Ok(())
}

fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}

fn wait_for(ready: impl Fn() -> bool) -> Result<(), Failure> {
    let start = Instant::now();
    while !ready() {
        if start.elapsed() > Duration::from_secs(10) {
            return Err("the server did not start".into());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn open_window(conn: &Conn) -> Result<(u32, u32), Failure> {
    let root = conn.setup().roots[0].root;
    let window = conn.generate_id()?;
    let aux = CreateWindowAux::new().background_pixel(0x00FF_FFFF);
    conn.create_window(
        24,
        window,
        root,
        0,
        0,
        640,
        400,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &aux,
    )?;
    conn.map_window(window)?;
    let font = conn.generate_id()?;
    conn.open_font(font, b"fixed")?;
    let gc = conn.generate_id()?;
    let aux = CreateGCAux::new()
        .foreground(0)
        .background(0x00FF_FFFF)
        .font(font);
    conn.create_gc(gc, window, &aux)?;
    conn.sync()?;
    Ok((window, gc))
}

/// Opens the page connection. A thread reads it and gives the arrival time of each tile.
fn open_page(port: u16) -> Result<Receiver<Instant>, Failure> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_nodelay(true)?;
    write!(
        stream,
        "GET /ws HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\n\
         Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n"
    )?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte)?;
        head.push(byte[0]);
    }
    if !head.starts_with(b"HTTP/1.1 101") {
        return Err(format!("no WebSocket: {}", String::from_utf8_lossy(&head)).into());
    }
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        while let Ok((opcode, payload)) = read_frame(&mut stream) {
            let is_tile = xsrv_server::display::decode_tile(&payload).is_some();
            if opcode == OPCODE_BINARY && is_tile && sender.send(Instant::now()).is_err() {
                return;
            }
        }
    });
    Ok(receiver)
}

/// One frame from the server: the opcode and the payload. Server frames have no mask.
fn read_frame(stream: &mut TcpStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 2];
    stream.read_exact(&mut head)?;
    let len = match head[1] & 0x7F {
        126 => {
            let mut bytes = [0u8; 2];
            stream.read_exact(&mut bytes)?;
            u64::from(u16::from_be_bytes(bytes))
        }
        127 => {
            let mut bytes = [0u8; 8];
            stream.read_exact(&mut bytes)?;
            u64::from_be_bytes(bytes)
        }
        short => u64::from(short),
    };
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload)?;
    Ok((head[0] & 0x0F, payload))
}

/// Runs `update` and waits for `tiles_wanted` tiles. The time is from the start of `update` to
/// the last of them.
fn measure(
    tiles: &Receiver<Instant>,
    tiles_wanted: usize,
    update: impl Fn(usize) -> Result<(), Failure>,
) -> Result<Vec<Duration>, Failure> {
    let mut times = Vec::with_capacity(ROUNDS);
    for round in 0..ROUNDS {
        thread::sleep(PAUSE);
        while tiles.try_recv().is_ok() {}
        let start = Instant::now();
        update(round)?;
        let mut last = start;
        for _ in 0..tiles_wanted {
            last = tiles
                .recv_timeout(TIME_LIMIT)
                .map_err(|_| "no tile arrived in time")?;
        }
        times.push(last.duration_since(start));
    }
    Ok(times)
}

fn report(name: &str, mut times: Vec<Duration>) {
    times.sort();
    let ms = |time: Duration| time.as_secs_f64() * 1000.0;
    let mean = times.iter().sum::<Duration>() / times.len() as u32;
    let p95 = times[times.len() * 95 / 100];
    println!(
        "{name:<28} {:>8.2} {:>8.2} {:>8.2}",
        ms(mean),
        ms(p95),
        ms(times[times.len() - 1])
    );
}
