// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! Draws a few windows on a running xsrv.
//! Usage: `cargo run -p xsrv-server --example demo -- /tmp/xsrv/X1`

use std::os::unix::net::UnixStream;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    ChangeGCAux, ConnectionExt, CoordMode, CreateGCAux, CreateWindowAux, EventMask, Point,
    Rectangle, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::{DefaultStream, RustConnection};

const PALETTE: [u32; 4] = [0xE63946, 0x2A9D8F, 0xE9C46A, 0x264653];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: demo <socket path>")?;
    let (stream, _) = DefaultStream::from_unix_stream(UnixStream::connect(path)?)?;
    let conn = RustConnection::connect_to_stream(stream, 0)?;
    let root = conn.setup().roots[0].root;

    let window = conn.generate_id()?;
    conn.create_window(
        24,
        window,
        root,
        40,
        40,
        360,
        240,
        4,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .background_pixel(0xF1FAEE)
            .border_pixel(0x1D3557)
            .event_mask(EventMask::EXPOSURE),
    )?;
    conn.map_window(window)?;
    conn.flush()?;

    let font = conn.generate_id()?;
    conn.open_font(font, b"fixed")?;
    let gc = conn.generate_id()?;
    conn.create_gc(gc, window, &CreateGCAux::new().font(font))?;
    loop {
        if let Event::Expose(_) = conn.wait_for_event()? {
            for (index, color) in PALETTE.iter().enumerate() {
                conn.change_gc(gc, &ChangeGCAux::new().foreground(*color))?;
                let x = 20 + index as i16 * 80;
                conn.poly_fill_rectangle(
                    window,
                    gc,
                    &[Rectangle {
                        x,
                        y: 30 + index as i16 * 20,
                        width: 70,
                        height: 120,
                    }],
                )?;
            }
            conn.change_gc(
                gc,
                &ChangeGCAux::new().foreground(0x1D3557).background(0xF1FAEE),
            )?;
            conn.image_text8(window, gc, 20, 190, b"Hello from xsrv")?;
            conn.poly_line(
                CoordMode::ORIGIN,
                window,
                gc,
                &[
                    Point { x: 20, y: 200 },
                    Point { x: 340, y: 200 },
                    Point { x: 340, y: 225 },
                ],
            )?;
            conn.flush()?;
        }
    }
}
