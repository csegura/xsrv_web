// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! Opens a window and prints the input events that it gets.
//! Usage: `cargo run -p xsrv-server --example input_log -- /tmp/xsrv/X1`

use std::os::unix::net::UnixStream;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt, CreateWindowAux, EventMask, InputFocus, WindowClass};
use x11rb::protocol::Event;
use x11rb::rust_connection::{DefaultStream, RustConnection};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: input_log <socket path>")?;
    let (stream, _) = DefaultStream::from_unix_stream(UnixStream::connect(path)?)?;
    let conn = RustConnection::connect_to_stream(stream, 0)?;
    let root = conn.setup().roots[0].root;
    let window = conn.generate_id()?;
    let mask = EventMask::POINTER_MOTION
        | EventMask::BUTTON_PRESS
        | EventMask::BUTTON_RELEASE
        | EventMask::KEY_PRESS
        | EventMask::KEY_RELEASE
        | EventMask::ENTER_WINDOW
        | EventMask::LEAVE_WINDOW;
    conn.create_window(
        24,
        window,
        root,
        100,
        100,
        200,
        150,
        2,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .background_pixel(0xFFFFFF)
            .event_mask(mask),
    )?;
    conn.map_window(window)?;
    conn.set_input_focus(InputFocus::POINTER_ROOT, window, x11rb::CURRENT_TIME)?;
    conn.flush()?;
    println!("ready");
    loop {
        match conn.wait_for_event()? {
            Event::MotionNotify(e) => println!(
                "motion {} {} state={:#x}",
                e.event_x,
                e.event_y,
                u16::from(e.state)
            ),
            Event::ButtonPress(e) => {
                println!("button-press {} at {} {}", e.detail, e.event_x, e.event_y)
            }
            Event::ButtonRelease(e) => println!("button-release {}", e.detail),
            Event::KeyPress(e) => {
                println!("key-press {} state={:#x}", e.detail, u16::from(e.state))
            }
            Event::KeyRelease(e) => println!("key-release {}", e.detail),
            Event::EnterNotify(_) => println!("enter"),
            Event::LeaveNotify(_) => println!("leave"),
            _ => {}
        }
    }
}
