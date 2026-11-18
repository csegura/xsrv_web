// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! Measures what the page gets for typical X11 workloads: messages, bytes after compression, and
//! the time from the first request of an update to the reply of the round trip that ends it. A
//! request with a reply flushes the screen first, so that time includes compose and compression.
//!
//! Run with `make bench`.
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use tokio::runtime::Builder;
use tokio::sync::broadcast::Receiver;
use tokio::task::LocalSet;
use x11rb::connection::Connection;
use x11rb::protocol::render::{
    self, Color, ConnectionExt as _, CreatePictureAux, Glyphinfo, PictOp, Pictformat,
};
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xproto::{
    ConfigureWindowAux, ConnectionExt, CreateGCAux, CreateWindowAux, ImageFormat, Rectangle,
    WindowClass,
};
use x11rb::rust_connection::{DefaultStream, RustConnection};
use x11rb::wrapper::ConnectionExt as _;
use xsrv_server::display::{Display, TileMessage};
use xsrv_server::x11;

const SCREEN: (u16, u16) = (1280, 800);
const TERMINAL: (u16, u16) = (1280, 800);
const LINE_HEIGHT: i16 = 16;
const GLYPH_WIDTH: i16 = 8;
const BACKGROUND: u32 = 0x00FF_FFFF;
const FOREGROUND: u32 = 0x0000_0000;

type Conn = RustConnection<DefaultStream>;
/// A workload. It gets the path of the X11 socket of its own server.
type Scenario = fn(&Path) -> Result;

struct Terminal {
    window: u32,
    gc: u32,
    clear_gc: u32,
}

struct Result {
    name: &'static str,
    updates: usize,
    messages: usize,
    bytes: usize,
    times: Vec<Duration>,
}

fn main() {
    let dir = std::env::temp_dir().join(format!("xsrv-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("bench socket directory");
    let scenarios: [(&str, Scenario); 9] = [
        ("typing", typing),
        ("scroll", scroll),
        ("redraw", redraw),
        ("move", window_move),
        ("putimage", put_image),
        ("shm", shm_put_image),
        ("photo", photo),
        ("rtext", render_text),
        ("rblend", render_blend),
    ];
    // `XSRV_BENCH=rtext,shm make bench` runs only those, for a profile of one workload.
    let wanted = std::env::var("XSRV_BENCH").unwrap_or_default();
    let results: Vec<Result> = scenarios
        .into_iter()
        .filter(|(name, _)| wanted.is_empty() || wanted.split(',').any(|want| want == *name))
        .map(|(name, run)| run(&dir.join(name)))
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    print_table(&results);
}

/// One character for each update, like an echo in a terminal.
fn typing(path: &Path) -> Result {
    let (conn, mut page) = start(path);
    let term = open_terminal(&conn);
    fill_screen_text(&conn, &term);
    let mut result = Result::new("typing", &conn, &mut page);
    for index in 0..400_i16 {
        let x = (index % 150) * GLYPH_WIDTH;
        let y = LINE_HEIGHT * (index / 150 + 10);
        result.measure(&conn, &mut page, || {
            conn.image_text8(term.window, term.gc, x, y, b"k").unwrap();
        });
    }
    result
}

/// The whole terminal moves up one line, and one new line is drawn at the bottom.
fn scroll(path: &Path) -> Result {
    let (conn, mut page) = start(path);
    let term = open_terminal(&conn);
    fill_screen_text(&conn, &term);
    let (width, height) = TERMINAL;
    let line = LINE_HEIGHT as u16;
    let mut result = Result::new("scroll", &conn, &mut page);
    for index in 0..200 {
        result.measure(&conn, &mut page, || {
            conn.copy_area(
                term.window,
                term.window,
                term.gc,
                0,
                LINE_HEIGHT,
                0,
                0,
                width,
                height - line,
            )
            .unwrap();
            let bottom = (height - line) as i16;
            let rect = Rectangle {
                x: 0,
                y: bottom,
                width,
                height: line,
            };
            conn.poly_fill_rectangle(term.window, term.clear_gc, &[rect])
                .unwrap();
            let text = text_line(index);
            conn.image_text8(term.window, term.gc, 0, bottom + 12, &text)
                .unwrap();
        });
    }
    result
}

/// The client draws the same full screen of text again, as after an Expose.
fn redraw(path: &Path) -> Result {
    let (conn, mut page) = start(path);
    let term = open_terminal(&conn);
    fill_screen_text(&conn, &term);
    let mut result = Result::new("redraw", &conn, &mut page);
    for _ in 0..50 {
        result.measure(&conn, &mut page, || fill_screen_text(&conn, &term));
    }
    result
}

/// A half-screen window with text moves by a few pixels each update.
fn window_move(path: &Path) -> Result {
    let (conn, mut page) = start(path);
    let term = open_terminal(&conn);
    fill_screen_text(&conn, &term);
    let small = conn.generate_id().unwrap();
    let root = conn.setup().roots[0].root;
    let aux = CreateWindowAux::new().background_pixel(0x00DD_EEFF);
    conn.create_window(
        24,
        small,
        root,
        0,
        0,
        640,
        400,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &aux,
    )
    .unwrap();
    conn.map_window(small).unwrap();
    for row in 0..24 {
        conn.image_text8(
            small,
            term.gc,
            4,
            14 + row * LINE_HEIGHT,
            &text_line(row as usize),
        )
        .unwrap();
    }
    conn.sync().unwrap();
    let mut result = Result::new("move", &conn, &mut page);
    for step in 0..100 {
        let aux = ConfigureWindowAux::new().x(step * 4).y(step * 3);
        result.measure(&conn, &mut page, || {
            conn.configure_window(small, &aux).unwrap();
        });
    }
    result
}

const PICTURE: (u16, u16) = (800, 600);
const PHOTO: (u16, u16) = (400, 300);

/// A picture like a toolkit window: flat areas, rows of short dark marks, and one block that
/// moves with `step`. Most pixels stay the same from one step to the next.
fn toolkit_picture(step: usize) -> Vec<u8> {
    let (width, height) = (PICTURE.0 as usize, PICTURE.1 as usize);
    let mut pixels = vec![0u8; width * height * 4];
    let (block_x, block_y) = (20 + step * 7 % 500, 30 + step * 5 % 380);
    for (index, pixel) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (index % width, index / width);
        let in_block =
            (block_x..block_x + 200).contains(&x) && (block_y..block_y + 150).contains(&y);
        let mark = y % 20 > 12 && (x * 7 + y / 20 * 13) % 11 < 4;
        *pixel = if in_block {
            [0xA4, 0x65, 0x34, 0]
        } else if y < 40 {
            [0xD0, 0xD0, 0xD0, 0]
        } else if mark {
            [0x30, 0x30, 0x30, 0]
        } else {
            [0xF6, 0xF5, 0xF4, 0]
        };
    }
    pixels
}

/// Pixels that zstd cannot make smaller, different for each `step`.
fn noise_picture(step: usize) -> Vec<u8> {
    let mut state = 0x9E37_79B9_u32 ^ step as u32;
    let mut pixels = vec![0u8; PHOTO.0 as usize * PHOTO.1 as usize * 4];
    for pixel in pixels.as_chunks_mut::<4>().0 {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *pixel = [
            (state >> 24) as u8,
            (state >> 16) as u8,
            (state >> 8) as u8,
            0,
        ];
    }
    pixels
}

/// A toolkit without MIT-SHM sends its window as one PutImage.
fn put_image(path: &Path) -> Result {
    let (conn, mut page) = start(path);
    let term = open_terminal(&conn);
    let frames: Vec<Vec<u8>> = (0..8).map(toolkit_picture).collect();
    let mut result = Result::new("putimage", &conn, &mut page);
    for step in 0..100 {
        result.measure(&conn, &mut page, || {
            conn.put_image(
                ImageFormat::Z_PIXMAP,
                term.window,
                term.gc,
                PICTURE.0,
                PICTURE.1,
                0,
                0,
                0,
                24,
                &frames[step % frames.len()],
            )
            .unwrap();
        });
    }
    result
}

/// GTK and Chromium send their windows with MIT-SHM.
fn shm_put_image(path: &Path) -> Result {
    let (conn, mut page) = start(path);
    let term = open_terminal(&conn);
    let len = PICTURE.0 as usize * PICTURE.1 as usize * 4;
    // SAFETY: the segment is made here with `len` bytes, and only this function uses it.
    let (shm_id, pointer) = unsafe {
        let id = libc::shmget(libc::IPC_PRIVATE, len, libc::IPC_CREAT | 0o600);
        assert!(id >= 0, "shmget failed");
        let pointer = libc::shmat(id, std::ptr::null(), 0).cast::<u8>();
        assert_ne!(pointer as isize, -1, "shmat failed");
        (id, pointer)
    };
    let seg = conn.generate_id().unwrap();
    conn.shm_attach(seg, shm_id as u32, true)
        .unwrap()
        .check()
        .unwrap();
    let frames: Vec<Vec<u8>> = (0..8).map(toolkit_picture).collect();
    let mut result = Result::new("shm", &conn, &mut page);
    for step in 0..100 {
        let frame = &frames[step % frames.len()];
        result.measure(&conn, &mut page, || {
            // SAFETY: a frame has exactly `len` bytes, the size of the segment.
            unsafe { std::ptr::copy_nonoverlapping(frame.as_ptr(), pointer, len) };
            conn.shm_put_image(
                term.window,
                term.gc,
                PICTURE.0,
                PICTURE.1,
                0,
                0,
                PICTURE.0,
                PICTURE.1,
                0,
                0,
                24,
                ImageFormat::Z_PIXMAP.into(),
                false,
                seg,
                0,
            )
            .unwrap();
        });
    }
    conn.shm_detach(seg).unwrap();
    conn.sync().unwrap();
    // SAFETY: the segment is the one made above, and nothing uses the pointer after this.
    unsafe {
        libc::shmdt(pointer.cast());
        libc::shmctl(shm_id, libc::IPC_RMID, std::ptr::null_mut());
    }
    result
}

/// A picture or a video frame: every pixel changes and compression does not help.
fn photo(path: &Path) -> Result {
    let (conn, mut page) = start(path);
    let term = open_terminal(&conn);
    let frames: Vec<Vec<u8>> = (0..8).map(noise_picture).collect();
    let mut result = Result::new("photo", &conn, &mut page);
    for step in 0..100 {
        result.measure(&conn, &mut page, || {
            conn.put_image(
                ImageFormat::Z_PIXMAP,
                term.window,
                term.gc,
                PHOTO.0,
                PHOTO.1,
                100,
                100,
                0,
                24,
                &frames[step % frames.len()],
            )
            .unwrap();
        });
    }
    result
}

fn format_of(conn: &Conn, depth: u8) -> Pictformat {
    let formats = conn.render_query_pict_formats().unwrap().reply().unwrap();
    formats
        .formats
        .iter()
        .find(|format| format.depth == depth)
        .map(|format| format.id)
        .unwrap_or_else(|| panic!("no picture format of depth {depth}"))
}

fn window_picture(conn: &Conn, window: u32) -> u32 {
    let picture = conn.generate_id().unwrap();
    conn.render_create_picture(
        picture,
        window,
        format_of(conn, 24),
        &CreatePictureAux::new(),
    )
    .unwrap();
    picture
}

/// A 1x1 picture that repeats: the way cairo gives a plain colour to RENDER.
fn solid_picture(conn: &Conn, colour: Color) -> u32 {
    let root = conn.setup().roots[0].root;
    let pixmap = conn.generate_id().unwrap();
    conn.create_pixmap(32, pixmap, root, 1, 1).unwrap();
    let picture = conn.generate_id().unwrap();
    let aux = CreatePictureAux::new().repeat(render::Repeat::NORMAL);
    conn.render_create_picture(picture, pixmap, format_of(conn, 32), &aux)
        .unwrap();
    let all = Rectangle {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    conn.render_fill_rectangles(PictOp::SRC, picture, colour, &[all])
        .unwrap();
    conn.free_pixmap(pixmap).unwrap();
    picture
}

const WHITE: Color = Color {
    red: 0xFFFF,
    green: 0xFFFF,
    blue: 0xFFFF,
    alpha: 0xFFFF,
};
const BLACK: Color = Color {
    red: 0,
    green: 0,
    blue: 0,
    alpha: 0xFFFF,
};

/// A glyph set with an 8x16 glyph of 8-bit coverage for each printable ASCII code.
fn glyph_set(conn: &Conn) -> u32 {
    let set = conn.generate_id().unwrap();
    conn.render_create_glyph_set(set, format_of(conn, 8))
        .unwrap();
    let info = Glyphinfo {
        width: GLYPH_WIDTH as u16,
        height: LINE_HEIGHT as u16,
        x: 0,
        y: 12,
        x_off: GLYPH_WIDTH,
        y_off: 0,
    };
    for code in 32..127_u32 {
        conn.render_add_glyphs(set, &[code], &[info], &glyph_coverage(code))
            .unwrap();
    }
    set
}

/// The 8-bit coverage of a glyph that looks like a letter to the blend: strokes two pixels
/// wide that are full, with a half-covered pixel at each side, and empty pixels elsewhere.
fn glyph_coverage(code: u32) -> Vec<u8> {
    let (width, height) = (GLYPH_WIDTH as usize, LINE_HEIGHT as usize);
    let shape = code.wrapping_mul(0x9E37_79B9);
    // A 4x6 grid of cells, each on or off, in the middle of the glyph.
    let full =
        |x: usize, y: usize| (2..14).contains(&y) && shape >> ((y - 2) / 2 * 4 + x / 2) & 1 == 1;
    (0..width * height)
        .map(|at| {
            let (x, y) = (at % width, at / width);
            let beside = (x > 0 && full(x - 1, y)) || (x + 1 < width && full(x + 1, y));
            match (full(x, y), beside) {
                (true, _) => 0xFF,
                (false, true) => 0x80,
                (false, false) => 0,
            }
        })
        .collect()
}

/// One CompositeGlyphs8 element: the glyphs of `text` with the pen at (`x`, `y`).
fn glyph_item(x: i16, y: i16, text: &[u8]) -> Vec<u8> {
    let mut item = vec![text.len() as u8, 0, 0, 0];
    item.extend_from_slice(&x.to_le_bytes());
    item.extend_from_slice(&y.to_le_bytes());
    item.extend_from_slice(text);
    item.resize(item.len().next_multiple_of(4), 0);
    item
}

/// A toolkit draws its window again: the background, then every line of text as glyphs.
fn render_text(path: &Path) -> Result {
    let (conn, mut page) = start(path);
    let term = open_terminal(&conn);
    let target = window_picture(&conn, term.window);
    let ink = solid_picture(&conn, BLACK);
    let set = glyph_set(&conn);
    let all = Rectangle {
        x: 0,
        y: 0,
        width: TERMINAL.0,
        height: TERMINAL.1,
    };
    let rows = TERMINAL.1 as usize / LINE_HEIGHT as usize;
    let mut result = Result::new("rtext", &conn, &mut page);
    for step in 0..50 {
        result.measure(&conn, &mut page, || {
            conn.render_fill_rectangles(PictOp::SRC, target, WHITE, &[all])
                .unwrap();
            for row in 0..rows {
                let y = row as i16 * LINE_HEIGHT + 12;
                let item = glyph_item(0, y, &text_line(row + step));
                conn.render_composite_glyphs8(PictOp::OVER, ink, target, 0, set, 0, 0, &item)
                    .unwrap();
            }
        });
    }
    result
}

/// A picture with alpha is blended over the window at a place that moves, as a toolkit does for
/// shadows, icons and menus.
fn render_blend(path: &Path) -> Result {
    let (conn, mut page) = start(path);
    let term = open_terminal(&conn);
    fill_screen_text(&conn, &term);
    let target = window_picture(&conn, term.window);
    let root = conn.setup().roots[0].root;
    let pixmap = conn.generate_id().unwrap();
    conn.create_pixmap(32, pixmap, root, PHOTO.0, PHOTO.1)
        .unwrap();
    let source = conn.generate_id().unwrap();
    conn.render_create_picture(
        source,
        pixmap,
        format_of(&conn, 32),
        &CreatePictureAux::new(),
    )
    .unwrap();
    let half = Color {
        red: 0x2000,
        green: 0x4000,
        blue: 0x8000,
        alpha: 0x8000,
    };
    let all = Rectangle {
        x: 0,
        y: 0,
        width: PHOTO.0,
        height: PHOTO.1,
    };
    conn.render_fill_rectangles(PictOp::SRC, source, half, &[all])
        .unwrap();
    let mut result = Result::new("rblend", &conn, &mut page);
    for step in 0..100_i16 {
        result.measure(&conn, &mut page, || {
            conn.render_composite(
                PictOp::OVER,
                source,
                x11rb::NONE,
                target,
                0,
                0,
                0,
                0,
                step * 6,
                step * 4,
                PHOTO.0,
                PHOTO.1,
            )
            .unwrap();
        });
    }
    result
}

fn start(path: &Path) -> (Conn, Receiver<TileMessage>) {
    let display = Display::new(SCREEN.0, SCREEN.1);
    let page = display.subscribe();
    let (ready_tx, ready_rx) = mpsc::channel();
    let server_path: PathBuf = path.to_owned();
    thread::spawn(move || {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        LocalSet::new().block_on(&runtime, async move {
            let listener = x11::bind(&server_path).unwrap();
            ready_tx.send(()).unwrap();
            let _ = x11::serve_with(listener, display, x11::Options::default()).await;
        });
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let stream = UnixStream::connect(path).unwrap();
    let (stream, _) = DefaultStream::from_unix_stream(stream).unwrap();
    (RustConnection::connect_to_stream(stream, 0).unwrap(), page)
}

fn open_terminal(conn: &Conn) -> Terminal {
    let root = conn.setup().roots[0].root;
    let window = conn.generate_id().unwrap();
    let aux = CreateWindowAux::new().background_pixel(BACKGROUND);
    conn.create_window(
        24,
        window,
        root,
        0,
        0,
        TERMINAL.0,
        TERMINAL.1,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &aux,
    )
    .unwrap();
    conn.map_window(window).unwrap();
    let font = conn.generate_id().unwrap();
    conn.open_font(font, b"fixed").unwrap();
    let gc = conn.generate_id().unwrap();
    let aux = CreateGCAux::new()
        .foreground(FOREGROUND)
        .background(BACKGROUND)
        .font(font)
        .graphics_exposures(0);
    conn.create_gc(gc, window, &aux).unwrap();
    let clear_gc = conn.generate_id().unwrap();
    let aux = CreateGCAux::new().foreground(BACKGROUND);
    conn.create_gc(clear_gc, window, &aux).unwrap();
    conn.sync().unwrap();
    Terminal {
        window,
        gc,
        clear_gc,
    }
}

fn fill_screen_text(conn: &Conn, term: &Terminal) {
    let rows = TERMINAL.1 as usize / LINE_HEIGHT as usize;
    for row in 0..rows {
        let y = row as i16 * LINE_HEIGHT + 12;
        conn.image_text8(term.window, term.gc, 0, y, &text_line(row))
            .unwrap();
    }
}

/// 160 characters of text that look like a log line, different for each `index`.
fn text_line(index: usize) -> Vec<u8> {
    let line = format!(
        "{index:06} xsrv[{}]: request {:>5} took {:>3} us; window 0x{:08x} mapped at {}x{} ",
        1000 + index % 97,
        index * 7 % 9973,
        index * 13 % 997,
        0x0040_0000 + index * 31,
        index % 1280,
        index % 800
    );
    line.bytes().cycle().take(160).collect()
}

impl Result {
    fn new(name: &'static str, conn: &Conn, page: &mut Receiver<TileMessage>) -> Self {
        conn.sync().unwrap();
        while page.try_recv().is_ok() {}
        Self {
            name,
            updates: 0,
            messages: 0,
            bytes: 0,
            times: Vec::new(),
        }
    }

    fn measure(&mut self, conn: &Conn, page: &mut Receiver<TileMessage>, update: impl FnOnce()) {
        let start = Instant::now();
        update();
        conn.sync().unwrap();
        self.times.push(start.elapsed());
        self.updates += 1;
        loop {
            match page.try_recv() {
                Ok(message) => {
                    self.messages += 1;
                    self.bytes += message.len();
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(lost)) => {
                    panic!("{}: the page lost {lost} messages", self.name)
                }
                Err(_) => break,
            }
        }
    }
}

fn print_table(results: &[Result]) {
    println!(
        "{:<8} {:>7} {:>9} {:>12} {:>12} {:>10} {:>10}",
        "scenario", "updates", "messages", "bytes", "bytes/upd", "mean ms", "p95 ms"
    );
    for result in results {
        let mut times = result.times.clone();
        times.sort();
        let mean = times.iter().sum::<Duration>() / times.len().max(1) as u32;
        let p95 = times[(times.len() * 95 / 100).min(times.len() - 1)];
        println!(
            "{:<8} {:>7} {:>9} {:>12} {:>12} {:>10.3} {:>10.3}",
            result.name,
            result.updates,
            result.messages,
            result.bytes,
            result.bytes / result.updates.max(1),
            mean.as_secs_f64() * 1000.0,
            p95.as_secs_f64() * 1000.0
        );
    }
}
