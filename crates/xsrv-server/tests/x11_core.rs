// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tokio::runtime::Builder;
use tokio::task::LocalSet;
use wire::Input;
use x11rb::connection::Connection;
use x11rb::connection::RequestConnection as _;
use x11rb::errors::ReplyError;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeGCAux, CloseDown, ConfigureWindowAux, ConnectionExt, CreateGCAux,
    CreateWindowAux, EventMask, PropMode, Rectangle, WindowClass, GX,
};
use x11rb::protocol::xproto::{
    ChangeWindowAttributesAux, Char2b, ClientMessageEvent, ClipOrdering, CoordMode, FillStyle,
    GrabMode, GrabStatus, Gravity, ImageFormat, InputFocus, ModMask, Point, Segment, StackMode,
};
use x11rb::protocol::{ErrorKind, Event};
use x11rb::rust_connection::{DefaultStream, RustConnection};
use x11rb::wrapper::ConnectionExt as _;
use xsrv_server::clipboard_set::ClipboardData;
use xsrv_server::display::Display;
use xsrv_server::x11;

/// One directory for the sockets of all tests in this run. It is removed when the process exits.
fn socket_dir() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        extern "C" fn remove_dir() {
            if let Some(dir) = DIR.get() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
        let dir = std::env::temp_dir().join(format!("xsrv-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: `remove_dir` only removes a directory and does not touch other state.
        unsafe { libc::atexit(remove_dir) };
        dir
    })
}

fn start_server(name: &str) -> PathBuf {
    start_server_with_display(name).0
}

fn start_server_with_display(name: &str) -> (PathBuf, Display) {
    start_server_with_options(name, x11::Options::default())
}

fn start_server_wm(name: &str) -> (PathBuf, Display) {
    start_server_with_options(
        name,
        x11::Options {
            window_manager: true,
            ..x11::Options::default()
        },
    )
}

fn start_server_with_options(name: &str, options: x11::Options) -> (PathBuf, Display) {
    let path = socket_dir().join(name);
    let display = Display::new(x11::SCREEN_WIDTH, x11::SCREEN_HEIGHT);
    let (ready_tx, ready_rx) = mpsc::channel();
    let (server_path, server_display) = (path.clone(), display.clone());
    thread::spawn(move || {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        LocalSet::new().block_on(&runtime, async move {
            let listener = x11::bind(&server_path).unwrap();
            ready_tx.send(()).unwrap();
            let _ = x11::serve_with(listener, server_display, options).await;
        });
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    (path, display)
}

fn connect(path: &PathBuf) -> RustConnection<DefaultStream> {
    let stream = UnixStream::connect(path).unwrap();
    let (stream, _) = DefaultStream::from_unix_stream(stream).unwrap();
    RustConnection::connect_to_stream(stream, 0).unwrap()
}

#[test]
fn window_gets_map_notify_and_expose() {
    let conn = connect(&start_server("expose"));
    let screen = &conn.setup().roots[0];
    let window = conn.generate_id().unwrap();
    let mask = EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY;
    conn.create_window(
        24,
        window,
        screen.root,
        10,
        20,
        100,
        50,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new().event_mask(mask),
    )
    .unwrap();
    conn.map_window(window).unwrap();
    conn.flush().unwrap();

    let mut saw_map = false;
    loop {
        match next_event(&conn) {
            Event::MapNotify(e) => {
                assert_eq!(e.window, window);
                saw_map = true;
            }
            Event::Expose(e) => {
                assert_eq!((e.window, e.width, e.height), (window, 100, 50));
                break;
            }
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert!(saw_map);
}

#[test]
fn geometry_tree_properties_and_destroy() {
    let conn = connect(&start_server("core"));
    let root = conn.setup().roots[0].root;
    let window = conn.generate_id().unwrap();
    conn.create_window(
        24,
        window,
        root,
        1,
        2,
        30,
        40,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .event_mask(EventMask::STRUCTURE_NOTIFY | EventMask::PROPERTY_CHANGE),
    )
    .unwrap();

    let geometry = conn.get_geometry(window).unwrap().reply().unwrap();
    assert_eq!(
        (geometry.x, geometry.y, geometry.width, geometry.height),
        (1, 2, 30, 40)
    );
    assert!(conn
        .query_tree(root)
        .unwrap()
        .reply()
        .unwrap()
        .children
        .contains(&window));

    let atom = conn
        .intern_atom(false, b"XSRV_TEST")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let name = conn.get_atom_name(atom).unwrap().reply().unwrap().name;
    assert_eq!(name, b"XSRV_TEST");
    conn.change_property8(PropMode::REPLACE, window, atom, AtomEnum::STRING, b"hello")
        .unwrap();
    let property = conn
        .get_property(false, window, atom, AtomEnum::STRING, 0, 16)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(property.value, b"hello");

    conn.configure_window(window, &ConfigureWindowAux::new().width(200))
        .unwrap();
    conn.destroy_window(window).unwrap();
    conn.flush().unwrap();

    let mut seen = Vec::new();
    while seen.len() < 3 {
        match next_event(&conn) {
            Event::PropertyNotify(_) => seen.push("property"),
            Event::ConfigureNotify(e) => {
                assert_eq!(e.width, 200);
                seen.push("configure");
            }
            Event::DestroyNotify(e) => {
                assert_eq!(e.window, window);
                seen.push("destroy");
            }
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert_eq!(seen, ["property", "configure", "destroy"]);
    assert_eq!(
        error_kind(conn.get_geometry(window).unwrap().reply()),
        ErrorKind::Drawable
    );
}

#[test]
fn unknown_extension_is_reported_absent() {
    let conn = connect(&start_server("ext"));
    let reply = conn.query_extension(b"NOPE").unwrap().reply().unwrap();
    assert!(!reply.present);
}

#[test]
fn bad_window_id_returns_an_error_and_keeps_the_connection() {
    let conn = connect(&start_server("badid"));
    assert_eq!(
        error_kind(conn.map_window(0x1234_5678).unwrap().check()),
        ErrorKind::Window
    );
    assert!(conn.get_input_focus().unwrap().reply().is_ok());
}

#[test]
fn old_protocol_version_is_refused() {
    let mut stream = UnixStream::connect(start_server("version")).unwrap();
    stream
        .write_all(&[b'l', 0, 12, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        .unwrap();
    let mut status = [0u8; 1];
    stream.read_exact(&mut status).unwrap();
    assert_eq!(status[0], 0);
}

#[test]
fn zero_length_request_closes_the_connection() {
    let mut stream = UnixStream::connect(start_server("zero")).unwrap();
    stream
        .write_all(&[b'l', 0, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        .unwrap();
    let mut setup = [0u8; 8];
    stream.read_exact(&mut setup).unwrap();
    assert_eq!(setup[0], 1);
    let mut rest = vec![0u8; u16::from_le_bytes([setup[6], setup[7]]) as usize * 4];
    stream.read_exact(&mut rest).unwrap();
    stream.write_all(&[127, 0, 0, 0]).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut sink = [0u8; 1];
    assert_eq!(stream.read(&mut sink).unwrap(), 0);
}

fn error_kind<T: std::fmt::Debug>(result: Result<T, ReplyError>) -> ErrorKind {
    match result {
        Err(ReplyError::X11Error(error)) => error.error_kind,
        other => panic!("expected an X11 error, got {other:?}"),
    }
}

fn new_window(conn: &RustConnection<DefaultStream>, parent: u32, aux: &CreateWindowAux) -> u32 {
    let window = conn.generate_id().unwrap();
    conn.create_window(
        24,
        window,
        parent,
        0,
        0,
        50,
        50,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        aux,
    )
    .unwrap();
    window
}

fn drain_events(conn: &RustConnection<DefaultStream>) -> Vec<Event> {
    conn.get_input_focus().unwrap().reply().unwrap();
    let mut events = Vec::new();
    while let Some(event) = conn.poll_for_event().unwrap() {
        events.push(event);
    }
    events
}

#[test]
fn disconnect_removes_event_selections() {
    let path = start_server("listeners");
    let first = connect(&path);
    let root = first.setup().roots[0].root;
    first
        .change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_NOTIFY),
        )
        .unwrap()
        .check()
        .unwrap();
    drop(first);

    let second = connect(&path);
    let mut cleaned = false;
    for _ in 0..100 {
        let attributes = second.get_window_attributes(root).unwrap().reply().unwrap();
        if attributes.all_event_masks.bits() == 0 {
            cleaned = true;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(cleaned, "event selections of a closed client are still set");
}

#[test]
fn second_window_manager_gets_bad_access() {
    let path = start_server("redirect");
    let first = connect(&path);
    let second = connect(&path);
    let root = first.setup().roots[0].root;
    let redirect = ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_REDIRECT);
    first
        .change_window_attributes(root, &redirect)
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        error_kind(
            second
                .change_window_attributes(root, &redirect)
                .unwrap()
                .check()
        ),
        ErrorKind::Access
    );
}

#[test]
fn attribute_values_are_validated_and_kept() {
    let conn = connect(&start_server("attributes"));
    let root = conn.setup().roots[0].root;
    let window = new_window(&conn, root, &CreateWindowAux::new());
    let set = |aux: &ChangeWindowAttributesAux| {
        conn.change_window_attributes(window, aux).unwrap().check()
    };
    assert_eq!(
        error_kind(set(&ChangeWindowAttributesAux::new().colormap(0xDEAD))),
        ErrorKind::Colormap
    );
    assert_eq!(
        error_kind(set(
            &ChangeWindowAttributesAux::new().event_mask(EventMask::from(0xFF00_0000u32))
        )),
        ErrorKind::Value
    );
    set(&ChangeWindowAttributesAux::new().win_gravity(Gravity::STATIC)).unwrap();
    let attributes = conn.get_window_attributes(window).unwrap().reply().unwrap();
    assert_eq!(attributes.win_gravity, Gravity::STATIC);
}

#[test]
fn input_only_window_needs_zero_border() {
    let conn = connect(&start_server("inputonly"));
    let root = conn.setup().roots[0].root;
    let window = conn.generate_id().unwrap();
    let result = conn
        .create_window(
            0,
            window,
            root,
            0,
            0,
            10,
            10,
            5,
            WindowClass::INPUT_ONLY,
            0,
            &CreateWindowAux::new(),
        )
        .unwrap()
        .check();
    assert_eq!(error_kind(result), ErrorKind::Match);
}

#[test]
fn destroying_a_parent_unmaps_only_that_window() {
    let conn = connect(&start_server("destroy"));
    let root = conn.setup().roots[0].root;
    let parent = new_window(&conn, root, &CreateWindowAux::new());
    let child = new_window(
        &conn,
        parent,
        &CreateWindowAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
    );
    conn.map_window(parent).unwrap();
    conn.map_window(child).unwrap();
    drain_events(&conn);
    conn.destroy_window(parent).unwrap();
    let events = drain_events(&conn);
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(matches!(&events[0], Event::DestroyNotify(e) if e.window == child));
}

#[test]
fn stacking_with_siblings_and_opposite() {
    let conn = connect(&start_server("stacking"));
    let root = conn.setup().roots[0].root;
    let lower = new_window(&conn, root, &CreateWindowAux::new());
    let upper = new_window(&conn, root, &CreateWindowAux::new());
    conn.map_window(lower).unwrap();
    conn.map_window(upper).unwrap();
    let order = || conn.query_tree(root).unwrap().reply().unwrap().children;
    assert_eq!(order(), [lower, upper]);

    conn.configure_window(
        lower,
        &ConfigureWindowAux::new().stack_mode(StackMode::OPPOSITE),
    )
    .unwrap();
    assert_eq!(order(), [upper, lower]);

    conn.configure_window(
        lower,
        &ConfigureWindowAux::new().stack_mode(StackMode::BELOW),
    )
    .unwrap();
    assert_eq!(order(), [lower, upper]);

    conn.configure_window(
        lower,
        &ConfigureWindowAux::new()
            .sibling(upper)
            .stack_mode(StackMode::ABOVE),
    )
    .unwrap();
    assert_eq!(order(), [upper, lower]);
}

#[test]
fn property_modes_and_partial_reads() {
    let conn = connect(&start_server("modes"));
    let root = conn.setup().roots[0].root;
    let window = new_window(&conn, root, &CreateWindowAux::new());
    let atom = conn
        .intern_atom(false, b"XSRV_MODES")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let string = AtomEnum::STRING;
    conn.change_property8(PropMode::REPLACE, window, atom, string, b"bcd")
        .unwrap();
    conn.change_property8(PropMode::PREPEND, window, atom, string, b"a")
        .unwrap();
    conn.change_property8(PropMode::APPEND, window, atom, string, b"e")
        .unwrap();
    let all = conn
        .get_property(false, window, atom, string, 0, 16)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(all.value, b"abcde");

    let part = conn
        .get_property(false, window, atom, string, 0, 1)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!((part.value.as_slice(), part.bytes_after), (&b"abcd"[..], 1));

    let wrong_type = conn
        .get_property(false, window, atom, AtomEnum::CARDINAL, 0, 16)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!((wrong_type.value.len(), wrong_type.bytes_after), (0, 5));

    let mismatch = conn
        .change_property32(PropMode::APPEND, window, atom, AtomEnum::CARDINAL, &[1])
        .unwrap()
        .check();
    assert_eq!(error_kind(mismatch), ErrorKind::Match);

    conn.get_property(true, window, atom, string, 0, 16)
        .unwrap()
        .reply()
        .unwrap();
    let gone = conn
        .get_property(false, window, atom, string, 0, 16)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(gone.type_, 0);
}

/// The pixels of a tile message, whether the server sent them raw or compressed.
fn tile_pixels(message: &[u8]) -> Vec<u8> {
    tile(message).expect("a tile message").1
}

/// The rect and the pixels of a tile message of any kind. `None` for another message.
fn tile(message: &[u8]) -> Option<(wire::Rect, Vec<u8>)> {
    xsrv_server::display::decode_tile(message)
}

fn screen_pixel(display: &Display, x: usize, y: usize) -> [u8; 3] {
    let (_, full, _) = display.subscribe_with_snapshot().1;
    let pixels = tile_pixels(&full);
    let width = usize::from(display.size().0);
    let offset = (y * width + x) * 4;
    [pixels[offset], pixels[offset + 1], pixels[offset + 2]]
}

fn sync(conn: &RustConnection<DefaultStream>) {
    conn.get_input_focus().unwrap().reply().unwrap();
}

const BLUE_GREEN_RED: fn(u32) -> [u8; 3] = |rgb| [rgb as u8, (rgb >> 8) as u8, (rgb >> 16) as u8];

#[test]
fn mapped_window_background_and_fill_reach_the_screen() {
    let (path, display) = start_server_with_display("pixels");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = conn.generate_id().unwrap();
    conn.create_window(
        24,
        window,
        root,
        10,
        20,
        30,
        20,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new().background_pixel(0x112233),
    )
    .unwrap();
    let gc = conn.generate_id().unwrap();
    conn.create_gc(gc, window, &CreateGCAux::new().foreground(0xFF0000))
        .unwrap();
    sync(&conn);
    assert_eq!(
        screen_pixel(&display, 10, 20),
        [0, 0, 0],
        "unmapped window is invisible"
    );

    conn.map_window(window).unwrap();
    conn.poly_fill_rectangle(
        window,
        gc,
        &[Rectangle {
            x: 5,
            y: 5,
            width: 10,
            height: 10,
        }],
    )
    .unwrap();
    sync(&conn);
    assert_eq!(screen_pixel(&display, 10, 20), BLUE_GREEN_RED(0x112233));
    assert_eq!(screen_pixel(&display, 15, 25), BLUE_GREEN_RED(0xFF0000));
    assert_eq!(screen_pixel(&display, 24, 34), BLUE_GREEN_RED(0xFF0000));
    assert_eq!(screen_pixel(&display, 25, 35), BLUE_GREEN_RED(0x112233));
    assert_eq!(
        screen_pixel(&display, 40, 20),
        [0, 0, 0],
        "outside the window"
    );

    conn.unmap_window(window).unwrap();
    sync(&conn);
    assert_eq!(
        screen_pixel(&display, 15, 25),
        [0, 0, 0],
        "unmapped window is gone"
    );
}

#[test]
fn window_background_pixmap_is_tiled_and_copied() {
    let (path, display) = start_server_with_display("bgpixmap");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let tile = conn.generate_id().unwrap();
    conn.create_pixmap(24, tile, root, 2, 1).unwrap();
    let gc = conn.generate_id().unwrap();
    conn.create_gc(gc, tile, &CreateGCAux::new()).unwrap();
    let row = [1, 2, 3, 0, 4, 5, 6, 0];
    conn.put_image(ImageFormat::Z_PIXMAP, tile, gc, 2, 1, 0, 0, 0, 24, &row)
        .unwrap();
    let parent = new_window(&conn, root, &CreateWindowAux::new().background_pixmap(tile));
    conn.free_pixmap(tile).unwrap();
    let child = conn.generate_id().unwrap();
    conn.create_window(
        24,
        child,
        parent,
        1,
        0,
        4,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new().background_pixmap(1u32),
    )
    .unwrap();
    conn.configure_window(
        parent,
        &ConfigureWindowAux::new().x(10).y(20).width(8).height(2),
    )
    .unwrap();
    conn.map_window(parent).unwrap();
    conn.map_window(child).unwrap();
    sync(&conn);
    let at = |x: usize, y: usize| screen_pixel(&display, x, y);
    assert_eq!(at(10, 20), [1, 2, 3]);
    assert_eq!(at(10, 21), [1, 2, 3], "the tile is one row high");
    assert_eq!(at(12, 20), [1, 2, 3]);
    assert_eq!(at(17, 20), [4, 5, 6]);
    assert_eq!(
        at(11, 20),
        [4, 5, 6],
        "a ParentRelative child keeps the tile of its parent"
    );

    let bitmap = conn.generate_id().unwrap();
    conn.create_pixmap(1, bitmap, root, 2, 2).unwrap();
    let bad = ChangeWindowAttributesAux::new().background_pixmap(bitmap);
    assert_eq!(
        error_kind(conn.change_window_attributes(parent, &bad).unwrap().check()),
        ErrorKind::Match
    );
    let from_parent = conn.generate_id().unwrap();
    let copy_depth = conn
        .create_window(
            0,
            from_parent,
            root,
            0,
            0,
            2,
            2,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().background_pixmap(bitmap),
        )
        .unwrap()
        .check();
    assert_eq!(
        error_kind(copy_depth),
        ErrorKind::Match,
        "depth CopyFromParent"
    );
    let border_one = ChangeWindowAttributesAux::new().border_pixmap(1u32);
    assert_eq!(
        error_kind(
            conn.change_window_attributes(parent, &border_one)
                .unwrap()
                .check()
        ),
        ErrorKind::Pixmap
    );
    let missing = ChangeWindowAttributesAux::new().background_pixmap(0x1234_5678u32);
    assert_eq!(
        error_kind(
            conn.change_window_attributes(parent, &missing)
                .unwrap()
                .check()
        ),
        ErrorKind::Pixmap
    );
}

#[test]
fn border_child_clipping_and_stacking() {
    let (path, display) = start_server_with_display("compose");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let parent = conn.generate_id().unwrap();
    conn.create_window(
        24,
        parent,
        root,
        10,
        20,
        20,
        20,
        2,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .background_pixel(0x0000FF)
            .border_pixel(0x00FF00),
    )
    .unwrap();
    let child = conn.generate_id().unwrap();
    conn.create_window(
        24,
        child,
        parent,
        4,
        4,
        100,
        100,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new().background_pixel(0xFFFFFF),
    )
    .unwrap();
    conn.map_window(parent).unwrap();
    conn.map_window(child).unwrap();
    sync(&conn);

    let green = BLUE_GREEN_RED(0x00FF00);
    assert_eq!(screen_pixel(&display, 10, 20), green, "border corner");
    assert_eq!(
        screen_pixel(&display, 12, 22),
        BLUE_GREEN_RED(0x0000FF),
        "parent inside"
    );
    assert_eq!(
        screen_pixel(&display, 16, 26),
        BLUE_GREEN_RED(0xFFFFFF),
        "child origin"
    );
    assert_eq!(
        screen_pixel(&display, 31, 41),
        BLUE_GREEN_RED(0xFFFFFF),
        "child inner corner"
    );
    assert_eq!(
        screen_pixel(&display, 32, 41),
        green,
        "child is clipped by the parent"
    );
    assert_eq!(
        screen_pixel(&display, 33, 50),
        [0, 0, 0],
        "nothing past the parent"
    );

    conn.configure_window(parent, &ConfigureWindowAux::new().x(100).y(100))
        .unwrap();
    sync(&conn);
    assert_eq!(
        screen_pixel(&display, 10, 20),
        [0, 0, 0],
        "old place is repainted"
    );
    assert_eq!(screen_pixel(&display, 100, 100), green, "new place");
}

fn pixmap_exists(conn: &RustConnection<DefaultStream>, pixmap: u32) -> bool {
    conn.get_geometry(pixmap).unwrap().reply().is_ok()
}

#[test]
fn retained_pixmap_outlives_its_client_until_kill_client() {
    let path = start_server("retained");
    let owner = connect(&path);
    let root = owner.setup().roots[0].root;
    let kept = owner.generate_id().unwrap();
    owner.create_pixmap(24, kept, root, 4, 4).unwrap();
    owner
        .set_close_down_mode(CloseDown::RETAIN_PERMANENT)
        .unwrap()
        .check()
        .unwrap();
    drop(owner);

    let plain = connect(&path);
    let lost = plain.generate_id().unwrap();
    plain.create_pixmap(24, lost, root, 4, 4).unwrap();
    sync(&plain);
    drop(plain);

    let other = connect(&path);
    let mut lost_gone = false;
    for _ in 0..100 {
        if !pixmap_exists(&other, lost) {
            lost_gone = true;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(lost_gone, "a pixmap of a closed client is still there");
    assert!(
        pixmap_exists(&other, kept),
        "the retained pixmap was destroyed"
    );
    assert_eq!(
        error_kind(
            other
                .set_close_down_mode(CloseDown::from(3u8))
                .unwrap()
                .check()
        ),
        ErrorKind::Value
    );
    assert_eq!(
        error_kind(other.kill_client(root).unwrap().check()),
        ErrorKind::Value
    );

    assert_eq!(
        error_kind(other.kill_client(kept + 1).unwrap().check()),
        ErrorKind::Value,
        "an id that is not a resource of the retained client"
    );
    assert!(pixmap_exists(&other, kept));
    other.kill_client(kept).unwrap().check().unwrap();
    assert!(!pixmap_exists(&other, kept), "KillClient left the pixmap");
}

#[test]
fn pixmaps_and_gc_errors() {
    let conn = connect(&start_server("pixmaps"));
    let root = conn.setup().roots[0].root;
    let pixmap = conn.generate_id().unwrap();
    conn.create_pixmap(24, pixmap, root, 16, 8).unwrap();
    let geometry = conn.get_geometry(pixmap).unwrap().reply().unwrap();
    assert_eq!(
        (geometry.width, geometry.height, geometry.depth),
        (16, 8, 24)
    );

    let gc = conn.generate_id().unwrap();
    conn.create_gc(gc, pixmap, &CreateGCAux::new().foreground(0xABCDEF))
        .unwrap();
    conn.poly_fill_rectangle(
        pixmap,
        gc,
        &[Rectangle {
            x: -5,
            y: -5,
            width: 100,
            height: 100,
        }],
    )
    .unwrap()
    .check()
    .unwrap();

    let missing_gc = conn
        .poly_fill_rectangle(pixmap, 0x1234, &[])
        .unwrap()
        .check();
    assert_eq!(error_kind(missing_gc), ErrorKind::GContext);
    let missing_drawable = conn.poly_fill_rectangle(0x1234, gc, &[]).unwrap().check();
    assert_eq!(error_kind(missing_drawable), ErrorKind::Drawable);
    let bad_function = conn
        .change_gc(gc, &ChangeGCAux::new().function(GX::from(99u32)))
        .unwrap()
        .check();
    assert_eq!(error_kind(bad_function), ErrorKind::Value);
    let bad_depth = conn
        .create_pixmap(4, conn.generate_id().unwrap(), root, 4, 4)
        .unwrap()
        .check();
    assert_eq!(error_kind(bad_depth), ErrorKind::Value);

    conn.free_pixmap(pixmap).unwrap();
    assert_eq!(
        error_kind(conn.free_pixmap(pixmap).unwrap().check()),
        ErrorKind::Pixmap
    );
    conn.free_gc(gc).unwrap();
    assert_eq!(
        error_kind(conn.free_gc(gc).unwrap().check()),
        ErrorKind::GContext
    );
}

const WHITE: [u8; 3] = [0xFF, 0xFF, 0xFF];
const BLACK: [u8; 3] = [0, 0, 0];

fn mapped_window(conn: &RustConnection<DefaultStream>, x: i16, y: i16, size: u16) -> (u32, u32) {
    let root = conn.setup().roots[0].root;
    let window = conn.generate_id().unwrap();
    conn.create_window(
        24,
        window,
        root,
        x,
        y,
        size,
        size,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new().background_pixel(0),
    )
    .unwrap();
    conn.map_window(window).unwrap();
    let gc = conn.generate_id().unwrap();
    conn.create_gc(gc, window, &CreateGCAux::new().foreground(0xFFFFFF))
        .unwrap();
    (window, gc)
}

#[test]
fn lines_points_segments_and_outlines() {
    let (path, display) = start_server_with_display("lines");
    let conn = connect(&path);
    let (window, gc) = mapped_window(&conn, 100, 100, 60);
    let point = |x, y| Point { x, y };
    conn.poly_line(
        CoordMode::ORIGIN,
        window,
        gc,
        &[point(0, 0), point(10, 0), point(10, 10)],
    )
    .unwrap();
    conn.poly_line(
        CoordMode::PREVIOUS,
        window,
        gc,
        &[point(20, 20), point(5, 0), point(0, 5)],
    )
    .unwrap();
    conn.poly_point(CoordMode::ORIGIN, window, gc, &[point(40, 5), point(42, 5)])
        .unwrap();
    conn.poly_segment(
        window,
        gc,
        &[Segment {
            x1: 30,
            y1: 40,
            x2: 30,
            y2: 50,
        }],
    )
    .unwrap();
    conn.poly_rectangle(
        window,
        gc,
        &[Rectangle {
            x: 30,
            y: 10,
            width: 10,
            height: 10,
        }],
    )
    .unwrap();
    sync(&conn);
    let at = |x: usize, y: usize| screen_pixel(&display, 100 + x, 100 + y);
    assert_eq!(at(5, 0), WHITE, "first segment");
    assert_eq!(at(10, 5), WHITE, "second segment");
    assert_eq!(at(5, 5), BLACK);
    assert_eq!(at(25, 20), WHITE, "relative coordinates");
    assert_eq!(at(25, 25), WHITE);
    assert_eq!(
        (at(40, 5), at(41, 5), at(42, 5)),
        (WHITE, BLACK, WHITE),
        "points"
    );
    assert_eq!(
        (at(30, 40), at(30, 50), at(30, 51)),
        (WHITE, WHITE, BLACK),
        "segment"
    );
    assert_eq!(
        (at(30, 10), at(40, 20), at(35, 15)),
        (WHITE, WHITE, BLACK),
        "outline"
    );
}

#[test]
fn copy_area_between_drawables_and_with_overlap() {
    let (path, display) = start_server_with_display("copy");
    let conn = connect(&path);
    let (window, gc) = mapped_window(&conn, 200, 50, 40);
    let root = conn.setup().roots[0].root;
    let pixmap = conn.generate_id().unwrap();
    conn.create_pixmap(24, pixmap, root, 4, 4).unwrap();
    let red = conn.generate_id().unwrap();
    conn.create_gc(red, pixmap, &CreateGCAux::new().foreground(0xFF0000))
        .unwrap();
    conn.poly_fill_rectangle(
        pixmap,
        red,
        &[Rectangle {
            x: 0,
            y: 0,
            width: 4,
            height: 4,
        }],
    )
    .unwrap();
    conn.copy_area(pixmap, window, gc, 1, 1, 10, 10, 100, 100)
        .unwrap();
    sync(&conn);
    let at = |x: usize, y: usize| screen_pixel(&display, 200 + x, 50 + y);
    assert_eq!(at(10, 10), BLUE_GREEN_RED(0xFF0000));
    assert_eq!(at(12, 12), BLUE_GREEN_RED(0xFF0000));
    assert_eq!(at(13, 13), BLACK, "only the 3x3 visible part is copied");

    let row = [1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0, 10, 11, 12, 0];
    conn.put_image(ImageFormat::Z_PIXMAP, window, gc, 4, 1, 0, 30, 0, 24, &row)
        .unwrap();
    conn.copy_area(window, window, gc, 0, 30, 2, 30, 4, 1)
        .unwrap();
    sync(&conn);
    let bgr = |x: usize| screen_pixel(&display, 200 + x, 80);
    assert_eq!(bgr(0), [1, 2, 3]);
    assert_eq!(bgr(1), [4, 5, 6]);
    assert_eq!(bgr(2), [1, 2, 3], "overlapping copy reads the source first");
    assert_eq!(bgr(5), [10, 11, 12]);
}

#[test]
fn drawing_requests_report_bad_input() {
    let conn = connect(&start_server("drawerrors"));
    let (window, gc) = mapped_window(&conn, 0, 0, 20);
    let short = [0u8; 12];
    let too_short = conn
        .put_image(
            ImageFormat::Z_PIXMAP,
            window,
            gc,
            4,
            1,
            0,
            0,
            0,
            24,
            &short[..8],
        )
        .unwrap()
        .check();
    assert_eq!(error_kind(too_short), ErrorKind::Length);
    let bitmap = conn
        .put_image(
            ImageFormat::XY_PIXMAP,
            window,
            gc,
            8,
            1,
            0,
            0,
            0,
            1,
            &[0u8; 4],
        )
        .unwrap()
        .check();
    // A depth 1 XYPixmap does not fit a depth 24 window: only XYBitmap may differ in depth.
    assert_eq!(error_kind(bitmap), ErrorKind::Match);
    let bad_depth = conn
        .put_image(
            ImageFormat::Z_PIXMAP,
            window,
            gc,
            1,
            1,
            0,
            0,
            0,
            8,
            &[0u8; 4],
        )
        .unwrap()
        .check();
    assert_eq!(error_kind(bad_depth), ErrorKind::Match);
    let bad_mode = conn
        .poly_line(CoordMode::from(7u8), window, gc, &[Point { x: 0, y: 0 }])
        .unwrap()
        .check();
    assert_eq!(error_kind(bad_mode), ErrorKind::Value);
    let missing_source = conn
        .copy_area(0x4321, window, gc, 0, 0, 0, 0, 1, 1)
        .unwrap()
        .check();
    assert_eq!(error_kind(missing_source), ErrorKind::Drawable);
    assert!(conn.get_input_focus().unwrap().reply().is_ok());
}

#[test]
fn fonts_open_query_list_and_close() {
    let conn = connect(&start_server("fonts"));
    let font = conn.generate_id().unwrap();
    conn.open_font(font, b"fixed").unwrap().check().unwrap();
    let missing = conn.generate_id().unwrap();
    assert_eq!(
        error_kind(conn.open_font(missing, b"nosuchfont").unwrap().check()),
        ErrorKind::Name
    );

    let info = conn.query_font(font).unwrap().reply().unwrap();
    assert_eq!((info.font_ascent, info.font_descent), (13, 3));
    assert_eq!(info.char_infos.len(), 256);
    assert_eq!(info.char_infos[usize::from(b'A')].character_width, 8);
    assert_eq!(info.max_bounds.character_width, 8);
    // urxvt takes the charset from FONT and stops without it.
    let property = |name: &[u8]| {
        let atom = conn.intern_atom(true, name).unwrap().reply().unwrap().atom;
        info.properties
            .iter()
            .find(|p| p.name == atom)
            .map(|p| p.value)
    };
    let atom_name = |atom: u32| conn.get_atom_name(atom).unwrap().reply().unwrap().name;
    assert_eq!(
        atom_name(property(b"FONT").unwrap()),
        b"-xsrv-fixed-medium-r-normal--16-160-75-75-c-80-iso8859-1"
    );
    assert_eq!(
        atom_name(property(b"CHARSET_REGISTRY").unwrap()),
        b"iso8859"
    );
    assert_eq!(atom_name(property(b"CHARSET_ENCODING").unwrap()), b"1");
    assert_eq!(property(b"PIXEL_SIZE"), Some(16));
    assert_eq!(
        property(b"ADD_STYLE_NAME"),
        None,
        "an empty field is left out"
    );

    let listed = conn.list_fonts(10, b"fix*").unwrap().reply().unwrap();
    let names: Vec<_> = listed.names.iter().map(|s| s.name.clone()).collect();
    assert_eq!(names, [b"fixed".to_vec()]);

    let root = conn.setup().roots[0].root;
    let gc = conn.generate_id().unwrap();
    conn.create_gc(gc, root, &CreateGCAux::new().font(font))
        .unwrap()
        .check()
        .unwrap();
    let bad_gc = conn.generate_id().unwrap();
    assert_eq!(
        error_kind(
            conn.create_gc(bad_gc, root, &CreateGCAux::new().font(0x999))
                .unwrap()
                .check()
        ),
        ErrorKind::Font
    );

    conn.close_font(font).unwrap().check().unwrap();
    assert_eq!(
        error_kind(conn.close_font(font).unwrap().check()),
        ErrorKind::Font
    );
}

#[test]
fn image_text_and_poly_text_reach_the_screen() {
    let (path, display) = start_server_with_display("text");
    let conn = connect(&path);
    let (window, gc) = mapped_window(&conn, 100, 100, 80);
    conn.change_gc(gc, &ChangeGCAux::new().background(0x0000FF))
        .unwrap();
    conn.image_text8(window, gc, 10, 30, b"A").unwrap();
    conn.poly_text8(window, gc, 40, 30, &[1, 0, b'A', 0])
        .unwrap();
    sync(&conn);
    let at = |x: usize, y: usize| screen_pixel(&display, 100 + x, 100 + y);
    let blue = BLUE_GREEN_RED(0x0000FF);

    assert_eq!(at(10, 17), blue, "background of the first cell");
    assert_eq!(
        (at(12, 17), at(13, 17)),
        (WHITE, WHITE),
        "apex of the letter A"
    );
    assert_eq!(at(14, 17), blue);
    assert_eq!(at(10, 32), blue, "descent rows belong to the cell");
    assert_eq!(at(10, 33), BLACK, "below the cell");
    assert_eq!(at(18, 20), BLACK, "right of the cell");

    assert_eq!(at(40, 17), BLACK, "poly text keeps the background");
    assert_eq!(at(42, 17), WHITE);
}

#[test]
fn poly_text_rejects_an_unknown_font_shift() {
    let (path, _) = start_server_with_display("shift");
    let conn = connect(&path);
    let (window, gc) = mapped_window(&conn, 0, 0, 40);
    let items = [255, 0, 0, 0x12, 0x34, 1, 0, b'A'];
    let result = conn.poly_text8(window, gc, 0, 20, &items).unwrap().check();
    assert_eq!(error_kind(result), ErrorKind::Font);
    assert!(conn.get_input_focus().unwrap().reply().is_ok());
}

#[test]
fn colors_are_allocated_looked_up_and_queried() {
    let conn = connect(&start_server("colors"));
    let colormap = conn.setup().roots[0].default_colormap;

    let reply = conn
        .alloc_color(colormap, 0x1200, 0x3400, 0x5600)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(reply.pixel, 0x12_34_56);
    assert_eq!(
        (reply.red, reply.green, reply.blue),
        (0x1212, 0x3434, 0x5656)
    );

    let named = conn
        .alloc_named_color(colormap, b"Red")
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(
        (named.pixel, named.exact_red, named.visual_red),
        (0xFF0000, 0xFFFF, 0xFFFF)
    );
    let looked_up = conn
        .lookup_color(colormap, b"rgb:00/ff/00")
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!((looked_up.exact_red, looked_up.exact_green), (0, 0xFFFF));
    assert_eq!(
        error_kind(
            conn.alloc_named_color(colormap, b"nosuchcolor")
                .unwrap()
                .reply()
        ),
        ErrorKind::Name
    );
    assert_eq!(
        error_kind(conn.alloc_color(0x777, 0, 0, 0).unwrap().reply()),
        ErrorKind::Colormap
    );

    let colors = conn
        .query_colors(colormap, &[0x0000FF, 0x808080])
        .unwrap()
        .reply()
        .unwrap()
        .colors;
    assert_eq!((colors[0].blue, colors[0].red), (0xFFFF, 0));
    assert_eq!(colors[1].green, 0x8080);
    conn.free_colors(colormap, 0, &[0x0000FF])
        .unwrap()
        .check()
        .unwrap();
}

#[test]
fn best_size_and_cursors() {
    let conn = connect(&start_server("cursors"));
    let root = conn.setup().roots[0].root;
    let best = conn
        .query_best_size(
            x11rb::protocol::xproto::QueryShapeOf::LARGEST_CURSOR,
            root,
            16,
            16,
        )
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!((best.width, best.height), (16, 16));

    let font = conn.generate_id().unwrap();
    conn.open_font(font, b"cursor").unwrap().check().unwrap();
    let cursor = conn.generate_id().unwrap();
    conn.create_glyph_cursor(cursor, font, font, 68, 69, 0, 0, 0, 0xFFFF, 0xFFFF, 0xFFFF)
        .unwrap()
        .check()
        .unwrap();
    let window = new_window(&conn, root, &CreateWindowAux::new().cursor(cursor));
    let unknown = conn
        .change_window_attributes(window, &ChangeWindowAttributesAux::new().cursor(0x4242))
        .unwrap()
        .check();
    assert_eq!(error_kind(unknown), ErrorKind::Cursor);
    conn.free_cursor(cursor).unwrap().check().unwrap();
    assert_eq!(
        error_kind(conn.free_cursor(cursor).unwrap().check()),
        ErrorKind::Cursor
    );
}

/// The source and the text of a clipboard message for the page (a set with a text part).
fn clipboard_text(message: &[u8]) -> Option<(wire::ClipboardSource, String)> {
    let Ok(wire::Message::ClipboardSet { source, parts }) = wire::decode(message) else {
        return None;
    };
    parts
        .filter(|(format, _)| *format == wire::ClipboardFormat::Text)
        .map(|(_, bytes)| (source, String::from_utf8_lossy(bytes).into_owned()))
        .next()
}

/// Waits for an event that the predicate accepts and skips the others.
fn wait_for_event(conn: &RustConnection<DefaultStream>, wanted: impl Fn(&Event) -> bool) -> Event {
    loop {
        let event = next_event(conn);
        if wanted(&event) {
            return event;
        }
    }
}

fn next_event(conn: &RustConnection<DefaultStream>) -> Event {
    conn.flush().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(event) = conn.poll_for_event().unwrap() {
            return event;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no event within 5 seconds"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn an_id_cannot_be_used_for_two_kinds_of_resource() {
    let conn = connect(&start_server("sameid"));
    let root = conn.setup().roots[0].root;
    let id = conn.generate_id().unwrap();
    conn.create_pixmap(24, id, root, 4, 4).unwrap();
    let window = conn
        .create_window(
            24,
            id,
            root,
            0,
            0,
            10,
            10,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new(),
        )
        .unwrap()
        .check();
    assert_eq!(error_kind(window), ErrorKind::IDChoice);
}

#[test]
fn clear_area_sends_expose_only_when_asked() {
    let conn = connect(&start_server("cleararea"));
    let root = conn.setup().roots[0].root;
    let window = conn.generate_id().unwrap();
    conn.create_window(
        24,
        window,
        root,
        0,
        0,
        50,
        50,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .background_pixel(0x123456)
            .event_mask(EventMask::EXPOSURE),
    )
    .unwrap();
    conn.map_window(window).unwrap();
    assert!(matches!(next_event(&conn), Event::Expose(_)));

    conn.clear_area(false, window, 5, 6, 7, 8).unwrap();
    assert!(drain_events(&conn).is_empty());

    conn.clear_area(true, window, 5, 6, 7, 8).unwrap();
    let events = drain_events(&conn);
    let [Event::Expose(e)] = events.as_slice() else {
        panic!("expected one Expose, got {events:?}");
    };
    assert_eq!((e.x, e.y, e.width, e.height), (5, 6, 7, 8));
}

#[test]
fn pixel_budget_is_enforced_and_released() {
    let conn = connect(&start_server("budget"));
    let root = conn.setup().roots[0].root;
    let mut pixmaps = Vec::new();
    let failure = loop {
        let id = conn.generate_id().unwrap();
        match conn
            .create_pixmap(24, id, root, 4096, 4096)
            .unwrap()
            .check()
        {
            Ok(()) => pixmaps.push(id),
            Err(error) => break error,
        }
        assert!(
            pixmaps.len() < 10,
            "the budget did not stop the allocations"
        );
    };
    assert_eq!(error_kind::<()>(Err(failure)), ErrorKind::Alloc);
    assert_eq!(
        pixmaps.len(),
        2,
        "one client may hold half of the 256 MiB budget"
    );

    conn.free_pixmap(pixmaps[0]).unwrap().check().unwrap();
    let id = conn.generate_id().unwrap();
    conn.create_pixmap(24, id, root, 4096, 4096)
        .unwrap()
        .check()
        .unwrap();
}

#[test]
fn resizing_a_mapped_window_repaints_old_and_new_area() {
    let (path, display) = start_server_with_display("resize");
    let conn = connect(&path);
    let (window, _) = mapped_window(&conn, 50, 50, 20);
    conn.change_window_attributes(
        window,
        &ChangeWindowAttributesAux::new().background_pixel(0xFFFFFF),
    )
    .unwrap();
    conn.configure_window(window, &ConfigureWindowAux::new().width(40).height(30))
        .unwrap();
    sync(&conn);
    assert_eq!(screen_pixel(&display, 85, 70), WHITE, "grown area");
    assert_eq!(screen_pixel(&display, 90, 70), BLACK, "outside");

    conn.configure_window(window, &ConfigureWindowAux::new().width(10).height(10))
        .unwrap();
    sync(&conn);
    assert_eq!(
        screen_pixel(&display, 85, 70),
        BLACK,
        "shrunk area is repainted"
    );
    assert_eq!(screen_pixel(&display, 55, 55), WHITE);
}

#[test]
fn text16_and_poly_text_delta() {
    let (path, display) = start_server_with_display("text16");
    let conn = connect(&path);
    let (window, gc) = mapped_window(&conn, 100, 100, 80);
    conn.image_text16(
        window,
        gc,
        0,
        20,
        &[Char2b {
            byte1: 0,
            byte2: b'A',
        }],
    )
    .unwrap();
    conn.poly_text16(window, gc, 20, 20, &[1, 4, 0, b'A'])
        .unwrap();
    sync(&conn);
    let at = |x: usize, y: usize| screen_pixel(&display, 100 + x, 100 + y);
    assert_eq!(at(2, 7), WHITE, "ImageText16 draws the same glyph");
    assert_eq!(at(20 + 4 + 2, 7), WHITE, "the delta moves the text right");
    assert_eq!(at(20 + 2, 7), BLACK, "nothing at the undelayed place");
}

#[test]
fn failed_configure_changes_nothing() {
    let conn = connect(&start_server("atomic"));
    let root = conn.setup().roots[0].root;
    let lower = new_window(&conn, root, &CreateWindowAux::new());
    let upper = new_window(&conn, root, &CreateWindowAux::new());
    let aux = ConfigureWindowAux::new()
        .width(60000)
        .height(60000)
        .stack_mode(StackMode::BELOW);
    let result = conn.configure_window(lower, &aux).unwrap().check();
    assert_eq!(error_kind(result), ErrorKind::Alloc);
    let order = conn.query_tree(root).unwrap().reply().unwrap().children;
    assert_eq!(order, [lower, upper]);
    let geometry = conn.get_geometry(lower).unwrap().reply().unwrap();
    assert_eq!((geometry.width, geometry.height), (50, 50));
}

#[test]
fn copy_area_reports_no_expose_or_graphics_expose() {
    let conn = connect(&start_server("copyevents"));
    let (window, gc) = mapped_window(&conn, 0, 0, 40);
    sync(&conn);
    drain_events(&conn);

    conn.copy_area(window, window, gc, 0, 0, 10, 10, 20, 20)
        .unwrap();
    let events = drain_events(&conn);
    assert!(
        matches!(events.as_slice(), [Event::NoExposure(e)] if e.drawable == window),
        "{events:?}"
    );

    conn.copy_area(window, window, gc, 30, 30, 0, 0, 20, 20)
        .unwrap();
    let events = drain_events(&conn);
    let exposed: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::GraphicsExposure(e) => Some((e.x, e.y, e.width, e.height)),
            _ => None,
        })
        .collect();
    assert_eq!(exposed.len(), events.len(), "{events:?}");
    let area: u32 = exposed
        .iter()
        .map(|&(_, _, w, h)| u32::from(w) * u32::from(h))
        .sum();
    assert_eq!(area, 20 * 20 - 10 * 10, "the part outside the source");

    let quiet = conn.generate_id().unwrap();
    conn.create_gc(quiet, window, &CreateGCAux::new().graphics_exposures(0))
        .unwrap();
    conn.copy_area(window, window, quiet, 0, 0, 10, 10, 5, 5)
        .unwrap();
    assert!(drain_events(&conn).is_empty());
}

#[test]
fn query_font_reports_which_glyphs_exist() {
    let conn = connect(&start_server("glyphs"));
    let font = conn.generate_id().unwrap();
    conn.open_font(font, b"fixed").unwrap().check().unwrap();
    let info = conn.query_font(font).unwrap().reply().unwrap();
    assert!(!info.all_chars_exist);
    assert_eq!(info.char_infos[usize::from(b'A')].character_width, 8);
    assert_eq!(info.char_infos[0xE9].character_width, 0);
    assert_eq!(info.default_char, u16::from(b' '));
}

#[test]
fn a_client_that_never_reads_is_disconnected() {
    let conn = connect(&start_server("overflow"));
    let root = conn.setup().roots[0].root;
    let window = new_window(&conn, root, &CreateWindowAux::new());
    let atom = conn
        .intern_atom(false, b"BIG")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let chunk = vec![7u8; 200_000];
    for _ in 0..5 {
        conn.change_property8(PropMode::APPEND, window, atom, AtomEnum::STRING, &chunk)
            .unwrap();
    }
    sync(&conn);
    for _ in 0..30 {
        conn.get_property(false, window, atom, AtomEnum::STRING, 0, 250_000)
            .unwrap();
    }
    conn.flush().unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let closed = loop {
        match conn.poll_for_event() {
            Err(_) => break true,
            Ok(_) if std::time::Instant::now() > deadline => break false,
            Ok(_) => thread::sleep(Duration::from_millis(10)),
        }
    };
    assert!(closed, "the server kept a client that does not read");
}

fn pointer_events() -> EventMask {
    EventMask::POINTER_MOTION
        | EventMask::BUTTON_PRESS
        | EventMask::BUTTON_RELEASE
        | EventMask::ENTER_WINDOW
        | EventMask::LEAVE_WINDOW
}

fn input_window(
    conn: &RustConnection<DefaultStream>,
    parent: u32,
    (x, y, size): (i16, i16, u16),
    mask: EventMask,
) -> u32 {
    let window = conn.generate_id().unwrap();
    conn.create_window(
        24,
        window,
        parent,
        x,
        y,
        size,
        size,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new().event_mask(mask),
    )
    .unwrap();
    conn.map_window(window).unwrap();
    window
}

/// The page input reaches the server through its own channel, so a request on the X socket
/// can overtake it. This waits until the server has seen the key.
fn wait_for_key_state(conn: &RustConnection<DefaultStream>, keycode: u8, down: bool) {
    for _ in 0..500 {
        let keys = conn.query_keymap().unwrap().reply().unwrap().keys;
        if (keys[usize::from(keycode / 8)] >> (keycode % 8) & 1 == 1) == down {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("the server never saw key {keycode} change");
}

fn pointer_to(display: &Display, x: u16, y: u16) {
    display.send_input(Input::Pointer { x, y });
}

fn button(display: &Display, button: u8, pressed: bool) {
    display.send_input(Input::Button { button, pressed });
}

fn key(display: &Display, keycode: u8, pressed: bool) {
    display.send_input(Input::Key { keycode, pressed });
}

#[test]
fn pointer_enters_a_window_moves_and_clicks() {
    let (path, display) = start_server_with_display("pointer");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(&conn, root, (100, 100, 50), pointer_events());
    sync(&conn);

    pointer_to(&display, 120, 130);
    let Event::EnterNotify(enter) = next_event(&conn) else {
        panic!("expected EnterNotify first");
    };
    assert_eq!(
        (enter.event, enter.event_x, enter.event_y),
        (window, 20, 30)
    );
    let Event::MotionNotify(motion) = next_event(&conn) else {
        panic!("expected MotionNotify");
    };
    assert_eq!(
        (motion.event, motion.root_x, motion.root_y),
        (window, 120, 130)
    );
    assert_eq!((motion.event_x, motion.event_y), (20, 30));

    button(&display, 1, true);
    let Event::ButtonPress(press) = next_event(&conn) else {
        panic!("expected ButtonPress");
    };
    assert_eq!(
        (press.detail, press.state.bits()),
        (1, 0),
        "state is before the press"
    );
    button(&display, 1, false);
    let Event::ButtonRelease(release) = next_event(&conn) else {
        panic!("expected ButtonRelease");
    };
    assert_eq!(
        (release.detail, release.state.bits()),
        (1, 0x100),
        "state includes the button"
    );

    pointer_to(&display, 10, 10);
    let Event::LeaveNotify(leave) = next_event(&conn) else {
        panic!("expected LeaveNotify");
    };
    assert_eq!(leave.event, window);
}

#[test]
fn button_events_propagate_to_the_parent_with_the_child_set() {
    let (path, display) = start_server_with_display("propagate");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let parent = input_window(&conn, root, (50, 50, 100), EventMask::BUTTON_PRESS);
    let child = input_window(&conn, parent, (10, 10, 30), EventMask::NO_EVENT);
    sync(&conn);

    pointer_to(&display, 70, 75);
    button(&display, 1, true);
    let Event::ButtonPress(press) = next_event(&conn) else {
        panic!("expected ButtonPress");
    };
    assert_eq!((press.event, press.child), (parent, child));
    assert_eq!(
        (press.event_x, press.event_y),
        (20, 25),
        "relative to the parent"
    );
}

#[test]
fn a_pressed_button_grabs_the_pointer_until_release() {
    let (path, display) = start_server_with_display("implicit");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(
        &conn,
        root,
        (100, 100, 50),
        EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::BUTTON1_MOTION,
    );
    let other = input_window(&conn, root, (300, 300, 50), EventMask::BUTTON_PRESS);
    sync(&conn);

    pointer_to(&display, 110, 110);
    button(&display, 1, true);
    assert!(matches!(next_event(&conn), Event::ButtonPress(_)));
    pointer_to(&display, 20, 30);
    let Event::MotionNotify(motion) = next_event(&conn) else {
        panic!("the grab must report motion outside the window");
    };
    assert_eq!(
        (motion.event, motion.event_x, motion.event_y),
        (window, -80, -70)
    );
    button(&display, 1, false);
    assert!(matches!(next_event(&conn), Event::ButtonRelease(_)));

    pointer_to(&display, 310, 310);
    button(&display, 3, true);
    let Event::ButtonPress(press) = next_event(&conn) else {
        panic!("expected the next press");
    };
    assert_eq!(
        (press.detail, press.event),
        (3, other),
        "the grab has ended, so the press goes to the window under the pointer"
    );
}

#[test]
fn keys_follow_the_pointer_and_report_modifiers() {
    let (path, display) = start_server_with_display("keys");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(
        &conn,
        root,
        (100, 100, 50),
        EventMask::KEY_PRESS | EventMask::KEY_RELEASE,
    );
    sync(&conn);

    pointer_to(&display, 110, 110);
    key(&display, 50, true);
    key(&display, 38, true);
    key(&display, 38, false);
    let Event::KeyPress(shift) = next_event(&conn) else {
        panic!("expected the Shift press");
    };
    assert_eq!((shift.detail, shift.event), (50, window));
    let Event::KeyPress(letter) = next_event(&conn) else {
        panic!("expected the letter press");
    };
    assert_eq!((letter.detail, letter.state.bits()), (38, 0x01));
    assert!(matches!(next_event(&conn), Event::KeyRelease(e) if e.detail == 38));

    pointer_to(&display, 400, 400);
    key(&display, 38, true);
    pointer_to(&display, 110, 110);
    key(&display, 39, true);
    let Event::KeyPress(after) = next_event(&conn) else {
        panic!("a key outside every window must not reach the client");
    };
    assert_eq!(after.detail, 39);
}

#[test]
fn input_focus_directs_keys_and_sends_focus_events() {
    let (path, display) = start_server_with_display("focus");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let mask = EventMask::KEY_PRESS | EventMask::FOCUS_CHANGE;
    let first = input_window(&conn, root, (0, 0, 50), mask);
    let second = input_window(&conn, root, (200, 200, 50), mask);
    sync(&conn);

    let initial = conn.get_input_focus().unwrap().reply().unwrap();
    assert_eq!(initial.focus, 1, "PointerRoot");

    conn.set_input_focus(InputFocus::PARENT, second, x11rb::CURRENT_TIME)
        .unwrap();
    let Event::FocusIn(focus) = next_event(&conn) else {
        panic!("expected FocusIn");
    };
    assert_eq!(focus.event, second);
    assert_eq!(
        conn.get_input_focus().unwrap().reply().unwrap().focus,
        second
    );

    pointer_to(&display, 10, 10);
    key(&display, 38, true);
    let Event::KeyPress(press) = next_event(&conn) else {
        panic!("expected KeyPress");
    };
    assert_eq!(
        press.event, second,
        "the focus window gets the key, not the pointer window"
    );
    let _ = first;

    conn.set_input_focus(InputFocus::NONE, 0u32, x11rb::CURRENT_TIME)
        .unwrap();
    assert!(matches!(next_event(&conn), Event::FocusOut(e) if e.event == second));
    key(&display, 39, true);
    wait_for_key_state(&conn, 39, true);
    conn.set_input_focus(InputFocus::POINTER_ROOT, 1u32, x11rb::CURRENT_TIME)
        .unwrap();
    sync(&conn);
    key(&display, 41, true);
    let Event::KeyPress(last) = next_event(&conn) else {
        panic!("expected KeyPress");
    };
    assert_eq!(last.detail, 41, "keys sent with no focus were dropped");
}

#[test]
fn destroying_the_focus_window_gives_the_focus_back() {
    let (path, _) = start_server_with_display("focusgone");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(&conn, root, (0, 0, 50), EventMask::NO_EVENT);
    conn.set_input_focus(InputFocus::POINTER_ROOT, window, x11rb::CURRENT_TIME)
        .unwrap();
    conn.destroy_window(window).unwrap();
    let focus = conn.get_input_focus().unwrap().reply().unwrap();
    assert_eq!(focus.focus, 1);
}

#[test]
fn keyboard_and_pointer_tables() {
    let (path, display) = start_server_with_display("tables");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;

    let map = conn.get_keyboard_mapping(38, 2).unwrap().reply().unwrap();
    assert_eq!(map.keysyms_per_keycode, 2);
    assert_eq!(map.keysyms, [0x61, 0x41, 0x73, 0x53]);
    assert_eq!(
        error_kind(conn.get_keyboard_mapping(7, 1).unwrap().reply()),
        ErrorKind::Value
    );
    assert_eq!(
        error_kind(conn.get_keyboard_mapping(200, 100).unwrap().reply()),
        ErrorKind::Value
    );

    let modifiers = conn.get_modifier_mapping().unwrap().reply().unwrap();
    assert_eq!(modifiers.keycodes_per_modifier(), 2);
    assert_eq!(&modifiers.keycodes[..2], [50, 62]);

    let window = input_window(&conn, root, (100, 100, 50), EventMask::NO_EVENT);
    sync(&conn);
    pointer_to(&display, 130, 140);
    button(&display, 2, true);
    key(&display, 50, true);
    let mut reply = None;
    for _ in 0..200 {
        let current = conn.query_pointer(window).unwrap().reply().unwrap();
        if current.mask.bits() == 0x201 {
            reply = Some(current);
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    let reply = reply.expect("the pointer state did not arrive");
    assert_eq!(
        (reply.root_x, reply.root_y, reply.win_x, reply.win_y),
        (130, 140, 30, 40)
    );
    assert_eq!(reply.child, 0);
    let from_root = conn.query_pointer(root).unwrap().reply().unwrap();
    assert_eq!(from_root.child, window);

    let translated = conn
        .translate_coordinates(window, root, 5, 6)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!((translated.dst_x, translated.dst_y), (105, 106));
    let keys = conn.query_keymap().unwrap().reply().unwrap();
    assert_eq!(keys.keys[50 / 8] >> (50 % 8) & 1, 1);
    assert_eq!(
        conn.get_pointer_mapping()
            .unwrap()
            .reply()
            .unwrap()
            .map
            .len(),
        7
    );
}

#[test]
fn active_pointer_grab_takes_the_events_and_is_exclusive() {
    let (path, display) = start_server_with_display("grabptr");
    let manager = connect(&path);
    let other = connect(&path);
    let root = manager.setup().roots[0].root;
    let window = input_window(&other, root, (100, 100, 50), EventMask::BUTTON_PRESS);
    sync(&other);

    let status = manager
        .grab_pointer(
            false,
            root,
            EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            0u32,
            0u32,
            x11rb::CURRENT_TIME,
        )
        .unwrap()
        .reply()
        .unwrap()
        .status;
    assert_eq!(status, GrabStatus::SUCCESS);
    let second = other
        .grab_pointer(
            false,
            window,
            EventMask::BUTTON_PRESS,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            0u32,
            0u32,
            x11rb::CURRENT_TIME,
        )
        .unwrap()
        .reply()
        .unwrap()
        .status;
    assert_eq!(second, GrabStatus::ALREADY_GRABBED);

    pointer_to(&display, 110, 110);
    button(&display, 1, true);
    let Event::ButtonPress(press) = next_event(&manager) else {
        panic!("the grabbing client must get the press");
    };
    assert_eq!(press.event, root);
    assert!(
        drain_events(&other).is_empty(),
        "the window owner gets nothing"
    );

    manager.ungrab_pointer(x11rb::CURRENT_TIME).unwrap();
    sync(&manager);
    button(&display, 1, false);
    pointer_to(&display, 112, 112);
    button(&display, 3, true);
    assert!(matches!(next_event(&other), Event::ButtonPress(e) if e.detail == 3));
}

#[test]
fn passive_key_and_button_grabs_start_on_a_matching_event() {
    let (path, display) = start_server_with_display("passive");
    let manager = connect(&path);
    let app = connect(&path);
    let root = manager.setup().roots[0].root;
    let window = input_window(
        &app,
        root,
        (100, 100, 50),
        EventMask::KEY_PRESS | EventMask::BUTTON_PRESS,
    );
    sync(&app);

    manager
        .grab_key(
            true,
            root,
            ModMask::CONTROL,
            38,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .unwrap()
        .check()
        .unwrap();
    manager
        .grab_button(
            false,
            root,
            EventMask::BUTTON_PRESS,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            0u32,
            0u32,
            1u8.into(),
            ModMask::CONTROL,
        )
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        error_kind(
            app.grab_key(
                true,
                root,
                ModMask::CONTROL,
                38,
                GrabMode::ASYNC,
                GrabMode::ASYNC
            )
            .unwrap()
            .check()
        ),
        ErrorKind::Access
    );

    pointer_to(&display, 110, 110);
    key(&display, 38, true);
    let Event::KeyPress(plain) = next_event(&app) else {
        panic!("without Control the key goes to the window");
    };
    assert_eq!(plain.event, window);
    key(&display, 38, false);

    key(&display, 37, true);
    key(&display, 38, true);
    let Event::KeyPress(grabbed) = next_event(&manager) else {
        panic!("the grabbing client must get Control+key");
    };
    assert_eq!((grabbed.detail, grabbed.state.bits()), (38, 0x04));
    button(&display, 1, true);
    let Event::ButtonPress(click) = next_event(&manager) else {
        panic!("the passive button grab must start");
    };
    assert_eq!(click.event, root);
}

#[test]
fn the_whole_keycode_range_can_be_read() {
    let conn = connect(&start_server("fullmap"));
    let setup = conn.setup();
    let (first, last) = (setup.min_keycode, setup.max_keycode);
    let count = last - first + 1;
    let map = conn
        .get_keyboard_mapping(first, count)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(map.keysyms.len(), usize::from(count) * 2);
    let at = |keycode: u8| &map.keysyms[usize::from(keycode - first) * 2..][..2];
    assert_eq!(at(38), [0x61, 0x41]);
    assert_eq!(at(255), [0, 0], "the last keycode has no symbol");
    let single = conn.get_keyboard_mapping(255, 1).unwrap().reply().unwrap();
    assert_eq!(single.keysyms, [0, 0]);
    assert!(
        conn.get_input_focus().unwrap().reply().is_ok(),
        "the server is still alive"
    );
}

#[test]
fn a_grab_with_owner_events_follows_the_window_under_the_pointer() {
    let (path, display) = start_server_with_display("owner");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let first = input_window(&conn, root, (100, 100, 50), EventMask::NO_EVENT);
    let second = input_window(&conn, root, (200, 200, 50), EventMask::POINTER_MOTION);
    let status = conn
        .grab_pointer(
            true,
            first,
            EventMask::POINTER_MOTION,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            0u32,
            0u32,
            x11rb::CURRENT_TIME,
        )
        .unwrap()
        .reply()
        .unwrap()
        .status;
    assert_eq!(status, GrabStatus::SUCCESS);

    pointer_to(&display, 210, 215);
    let Event::MotionNotify(motion) = next_event(&conn) else {
        panic!("expected MotionNotify");
    };
    assert_eq!(
        (motion.event, motion.event_x, motion.event_y),
        (second, 10, 15)
    );

    pointer_to(&display, 400, 400);
    let Event::MotionNotify(outside) = next_event(&conn) else {
        panic!("expected MotionNotify");
    };
    assert_eq!(
        outside.event, first,
        "outside every window of the client: the grab window"
    );
}

#[test]
fn destroying_the_focus_window_reverts_to_its_parent() {
    let (path, _) = start_server_with_display("revertparent");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let top = input_window(&conn, root, (0, 0, 100), EventMask::NO_EVENT);
    let child = input_window(&conn, top, (10, 10, 20), EventMask::NO_EVENT);
    conn.set_input_focus(InputFocus::PARENT, child, x11rb::CURRENT_TIME)
        .unwrap();
    conn.destroy_window(child).unwrap();
    let focus = conn.get_input_focus().unwrap().reply().unwrap();
    assert_eq!(focus.focus, top);
    assert_eq!(
        focus.revert_to,
        InputFocus::NONE,
        "a revert sets revert-to to None"
    );
}

#[test]
fn unmapping_the_grab_window_ends_the_grab() {
    let (path, display) = start_server_with_display("grabunmap");
    let manager = connect(&path);
    let app = connect(&path);
    let root = manager.setup().roots[0].root;
    let popup = input_window(&manager, root, (100, 100, 50), EventMask::NO_EVENT);
    let target = input_window(&app, root, (300, 300, 50), EventMask::KEY_PRESS);
    sync(&app);
    let status = manager
        .grab_keyboard(
            false,
            popup,
            x11rb::CURRENT_TIME,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .unwrap()
        .reply()
        .unwrap()
        .status;
    assert_eq!(status, GrabStatus::SUCCESS);

    manager.unmap_window(popup).unwrap();
    sync(&manager);
    pointer_to(&display, 310, 310);
    key(&display, 38, true);
    let Event::KeyPress(press) = next_event(&app) else {
        panic!("the key must reach the window under the pointer");
    };
    assert_eq!(press.event, target);
    assert!(drain_events(&manager).is_empty());
}

#[test]
fn release_all_lets_go_of_keys_and_buttons() {
    let (path, display) = start_server_with_display("releaseall");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    input_window(
        &conn,
        root,
        (100, 100, 50),
        EventMask::KEY_PRESS
            | EventMask::KEY_RELEASE
            | EventMask::BUTTON_PRESS
            | EventMask::BUTTON_RELEASE,
    );
    sync(&conn);
    pointer_to(&display, 110, 110);
    key(&display, 38, true);
    button(&display, 1, true);
    assert!(matches!(next_event(&conn), Event::KeyPress(_)));
    assert!(matches!(next_event(&conn), Event::ButtonPress(_)));

    display.send_input(Input::ReleaseAll);
    assert!(matches!(next_event(&conn), Event::ButtonRelease(e) if e.detail == 1));
    assert!(matches!(next_event(&conn), Event::KeyRelease(e) if e.detail == 38));
    wait_for_key_state(&conn, 38, false);
}

#[test]
fn lock_state_comes_from_the_page() {
    let (path, display) = start_server_with_display("locks");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    input_window(&conn, root, (100, 100, 50), EventMask::KEY_PRESS);
    sync(&conn);
    pointer_to(&display, 110, 110);

    display.send_input(Input::Locks {
        caps: true,
        num: true,
    });
    key(&display, 38, true);
    let Event::KeyPress(press) = next_event(&conn) else {
        panic!("expected KeyPress");
    };
    assert_eq!(press.state.bits(), 0x02 | 0x10, "Lock and Mod2");

    display.send_input(Input::Locks {
        caps: false,
        num: false,
    });
    key(&display, 39, true);
    let Event::KeyPress(press) = next_event(&conn) else {
        panic!("expected KeyPress");
    };
    assert_eq!(press.state.bits(), 0);
}

#[test]
fn crossing_events_say_whether_the_window_has_the_focus() {
    let (path, display) = start_server_with_display("crossfocus");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    input_window(
        &conn,
        root,
        (100, 100, 50),
        EventMask::ENTER_WINDOW | EventMask::LEAVE_WINDOW,
    );
    let elsewhere = input_window(&conn, root, (300, 300, 50), EventMask::NO_EVENT);
    sync(&conn);

    pointer_to(&display, 110, 110);
    let Event::EnterNotify(enter) = next_event(&conn) else {
        panic!("expected EnterNotify");
    };
    assert_eq!(
        enter.same_screen_focus & 1,
        1,
        "PointerRoot: every window has the focus"
    );

    pointer_to(&display, 10, 10);
    assert!(matches!(next_event(&conn), Event::LeaveNotify(_)));
    conn.set_input_focus(InputFocus::PARENT, elsewhere, x11rb::CURRENT_TIME)
        .unwrap();
    sync(&conn);
    pointer_to(&display, 110, 110);
    let Event::EnterNotify(enter) = next_event(&conn) else {
        panic!("expected EnterNotify");
    };
    assert_eq!(
        enter.same_screen_focus & 1,
        0,
        "the focus is in another window"
    );
    assert_eq!(enter.same_screen_focus & 2, 2, "same screen");
}

#[test]
fn the_pointer_leaving_the_page_sends_leave_and_coming_back_sends_enter() {
    let (path, display) = start_server_with_display("pointerleft");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    input_window(
        &conn,
        root,
        (100, 100, 50),
        EventMask::ENTER_WINDOW | EventMask::LEAVE_WINDOW,
    );
    sync(&conn);
    pointer_to(&display, 110, 110);
    assert!(matches!(next_event(&conn), Event::EnterNotify(_)));
    display.send_input(Input::PointerLeft);
    assert!(matches!(next_event(&conn), Event::LeaveNotify(_)));
    pointer_to(&display, 110, 110);
    assert!(matches!(next_event(&conn), Event::EnterNotify(_)));
}

#[test]
fn grab_modifier_masks_are_checked() {
    let conn = connect(&start_server("modmask"));
    let root = conn.setup().roots[0].root;
    let bad = conn
        .grab_key(
            true,
            root,
            ModMask::from(0x0100u16),
            38,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .unwrap()
        .check();
    assert_eq!(error_kind(bad), ErrorKind::Value);
    conn.grab_key(
        true,
        root,
        ModMask::ANY,
        38,
        GrabMode::ASYNC,
        GrabMode::ASYNC,
    )
    .unwrap()
    .check()
    .unwrap();
}

#[test]
fn bitmaps_are_drawn_copied_and_checked() {
    let (path, display) = start_server_with_display("bitmaps");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let (window, gc) = mapped_window(&conn, 100, 100, 50);
    conn.change_gc(gc, &ChangeGCAux::new().background(0x0000FF))
        .unwrap();
    let at = |x: usize, y: usize| screen_pixel(&display, 100 + x, 100 + y);
    let blue = BLUE_GREEN_RED(0x0000FF);

    conn.put_image(
        ImageFormat::XY_BITMAP,
        window,
        gc,
        8,
        1,
        0,
        0,
        0,
        1,
        &[0b0000_0101, 0, 0, 0],
    )
    .unwrap();
    sync(&conn);
    assert_eq!(
        (at(0, 0), at(1, 0), at(2, 0), at(3, 0)),
        (WHITE, blue, WHITE, blue)
    );

    let bitmap = conn.generate_id().unwrap();
    conn.create_pixmap(1, bitmap, root, 8, 1).unwrap();
    assert_eq!(conn.get_geometry(bitmap).unwrap().reply().unwrap().depth, 1);
    let ink = conn.generate_id().unwrap();
    conn.create_gc(ink, bitmap, &CreateGCAux::new().foreground(1).background(0))
        .unwrap();
    conn.poly_fill_rectangle(
        bitmap,
        ink,
        &[Rectangle {
            x: 1,
            y: 0,
            width: 2,
            height: 1,
        }],
    )
    .unwrap();
    conn.copy_plane(bitmap, window, gc, 0, 0, 0, 10, 8, 1, 1)
        .unwrap();
    sync(&conn);
    assert_eq!(
        (at(0, 10), at(1, 10), at(2, 10), at(3, 10)),
        (blue, WHITE, WHITE, blue)
    );

    let wrong_gc = conn.poly_fill_rectangle(bitmap, gc, &[]).unwrap().check();
    assert_eq!(error_kind(wrong_gc), ErrorKind::Match);
    let wrong_depth = conn
        .copy_area(bitmap, window, gc, 0, 0, 0, 0, 1, 1)
        .unwrap()
        .check();
    assert_eq!(error_kind(wrong_depth), ErrorKind::Match);
    let wrong_plane = conn
        .copy_plane(bitmap, window, gc, 0, 0, 0, 0, 1, 1, 3)
        .unwrap()
        .check();
    assert_eq!(error_kind(wrong_plane), ErrorKind::Value);
    let wrong_image = conn
        .put_image(
            ImageFormat::Z_PIXMAP,
            window,
            gc,
            8,
            1,
            0,
            0,
            0,
            1,
            &[0u8; 4],
        )
        .unwrap()
        .check();
    assert_eq!(error_kind(wrong_image), ErrorKind::Match);
}

#[test]
fn clip_rectangles_limit_all_drawing() {
    let (path, display) = start_server_with_display("clip");
    let conn = connect(&path);
    let (window, gc) = mapped_window(&conn, 100, 100, 50);
    let rect = |x, y, width, height| Rectangle {
        x,
        y,
        width,
        height,
    };
    let at = |x: usize, y: usize| screen_pixel(&display, 100 + x, 100 + y);

    conn.set_clip_rectangles(ClipOrdering::UNSORTED, gc, 10, 10, &[rect(0, 0, 5, 5)])
        .unwrap();
    conn.poly_fill_rectangle(window, gc, &[rect(0, 0, 50, 50)])
        .unwrap();
    conn.poly_line(
        CoordMode::ORIGIN,
        window,
        gc,
        &[Point { x: 0, y: 12 }, Point { x: 49, y: 12 }],
    )
    .unwrap();
    sync(&conn);
    assert_eq!((at(10, 10), at(14, 14)), (WHITE, WHITE));
    assert_eq!(
        (at(9, 10), at(15, 10), at(10, 15), at(30, 30)),
        (BLACK, BLACK, BLACK, BLACK)
    );
    assert_eq!(at(20, 12), BLACK, "the line is clipped too");

    conn.set_clip_rectangles(ClipOrdering::UNSORTED, gc, 0, 0, &[])
        .unwrap();
    conn.poly_fill_rectangle(window, gc, &[rect(20, 20, 5, 5)])
        .unwrap();
    sync(&conn);
    assert_eq!(at(21, 21), BLACK, "an empty clip draws nothing");

    conn.change_gc(gc, &ChangeGCAux::new().clip_mask(0u32))
        .unwrap();
    conn.poly_fill_rectangle(window, gc, &[rect(20, 20, 5, 5)])
        .unwrap();
    sync(&conn);
    assert_eq!(at(21, 21), WHITE, "a clip mask of None clears the clip");
}

#[test]
fn stipples_and_tiles_fill_rectangles() {
    let (path, display) = start_server_with_display("patterns");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let (window, gc) = mapped_window(&conn, 100, 100, 50);
    let rect = |x, y, width, height| Rectangle {
        x,
        y,
        width,
        height,
    };
    let at = |x: usize, y: usize| screen_pixel(&display, 100 + x, 100 + y);

    let stipple = conn.generate_id().unwrap();
    conn.create_pixmap(1, stipple, root, 2, 1).unwrap();
    let ink = conn.generate_id().unwrap();
    conn.create_gc(
        ink,
        stipple,
        &CreateGCAux::new().foreground(1).background(0),
    )
    .unwrap();
    conn.poly_fill_rectangle(stipple, ink, &[rect(0, 0, 1, 1)])
        .unwrap();

    conn.change_gc(
        gc,
        &ChangeGCAux::new()
            .fill_style(FillStyle::STIPPLED)
            .stipple(stipple)
            .background(0x0000FF),
    )
    .unwrap();
    conn.poly_fill_rectangle(window, gc, &[rect(0, 0, 4, 1)])
        .unwrap();
    conn.change_gc(
        gc,
        &ChangeGCAux::new().fill_style(FillStyle::OPAQUE_STIPPLED),
    )
    .unwrap();
    conn.poly_fill_rectangle(window, gc, &[rect(0, 2, 4, 1)])
        .unwrap();
    sync(&conn);
    let blue = BLUE_GREEN_RED(0x0000FF);
    assert_eq!(
        [at(0, 0), at(1, 0), at(2, 0), at(3, 0)],
        [WHITE, BLACK, WHITE, BLACK],
        "stippled"
    );
    assert_eq!(
        [at(0, 2), at(1, 2), at(2, 2), at(3, 2)],
        [WHITE, blue, WHITE, blue],
        "opaque"
    );

    let tile = conn.generate_id().unwrap();
    conn.create_pixmap(24, tile, root, 2, 1).unwrap();
    let paint = conn.generate_id().unwrap();
    conn.create_gc(paint, tile, &CreateGCAux::new().foreground(0xFF0000))
        .unwrap();
    conn.poly_fill_rectangle(tile, paint, &[rect(0, 0, 1, 1)])
        .unwrap();
    conn.change_gc(paint, &ChangeGCAux::new().foreground(0x00FF00))
        .unwrap();
    conn.poly_fill_rectangle(tile, paint, &[rect(1, 0, 1, 1)])
        .unwrap();
    conn.change_gc(
        gc,
        &ChangeGCAux::new().fill_style(FillStyle::TILED).tile(tile),
    )
    .unwrap();
    conn.poly_fill_rectangle(window, gc, &[rect(0, 4, 4, 1)])
        .unwrap();
    sync(&conn);
    let (red, green) = (BLUE_GREEN_RED(0xFF0000), BLUE_GREEN_RED(0x00FF00));
    assert_eq!(
        [at(0, 4), at(1, 4), at(2, 4), at(3, 4)],
        [red, green, red, green],
        "tiled"
    );

    let wrong_tile = conn
        .change_gc(gc, &ChangeGCAux::new().tile(stipple))
        .unwrap()
        .check();
    assert_eq!(
        error_kind(wrong_tile),
        ErrorKind::Match,
        "a tile has the depth of the GC"
    );
    let wrong_stipple = conn
        .change_gc(gc, &ChangeGCAux::new().stipple(tile))
        .unwrap()
        .check();
    assert_eq!(
        error_kind(wrong_stipple),
        ErrorKind::Match,
        "a stipple is a bitmap"
    );
}

#[test]
fn subwindow_requests_map_unmap_and_destroy_the_children() {
    let conn = connect(&start_server("subwindows"));
    let root = conn.setup().roots[0].root;
    let parent = input_window(&conn, root, (0, 0, 100), EventMask::NO_EVENT);
    let mask = EventMask::STRUCTURE_NOTIFY;
    let first = new_window(&conn, parent, &CreateWindowAux::new().event_mask(mask));
    let second = new_window(&conn, parent, &CreateWindowAux::new().event_mask(mask));
    sync(&conn);
    drain_events(&conn);

    conn.map_subwindows(parent).unwrap();
    let events = drain_events(&conn);
    let mapped: Vec<u32> = events
        .iter()
        .filter_map(|event| match event {
            Event::MapNotify(e) => Some(e.window),
            _ => None,
        })
        .collect();
    assert_eq!(mapped, [first, second], "the lowest first");

    conn.unmap_subwindows(parent).unwrap();
    let unmapped: Vec<u32> = drain_events(&conn)
        .iter()
        .filter_map(|event| match event {
            Event::UnmapNotify(e) => Some(e.window),
            _ => None,
        })
        .collect();
    assert_eq!(unmapped, [second, first], "the highest first");

    conn.destroy_subwindows(parent).unwrap();
    sync(&conn);
    let children = conn.query_tree(parent).unwrap().reply().unwrap().children;
    assert!(children.is_empty());
}

#[test]
fn get_image_reads_pixels_back() {
    let conn = connect(&start_server("getimage"));
    let root = conn.setup().roots[0].root;
    let (window, gc) = mapped_window(&conn, 10, 10, 20);
    conn.change_gc(gc, &ChangeGCAux::new().foreground(0x336699))
        .unwrap();
    conn.poly_fill_rectangle(
        window,
        gc,
        &[Rectangle {
            x: 2,
            y: 1,
            width: 2,
            height: 1,
        }],
    )
    .unwrap();

    let image = conn
        .get_image(ImageFormat::Z_PIXMAP, window, 1, 1, 4, 1, u32::MAX)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(image.depth, 24);
    assert_eq!(
        image.data,
        [0, 0, 0, 0, 0x99, 0x66, 0x33, 0, 0x99, 0x66, 0x33, 0, 0, 0, 0, 0]
    );
    let masked = conn
        .get_image(ImageFormat::Z_PIXMAP, window, 2, 1, 1, 1, 0x00FF00)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(masked.data, [0, 0x66, 0, 0], "the plane mask is applied");

    let bitmap = conn.generate_id().unwrap();
    conn.create_pixmap(1, bitmap, root, 10, 2).unwrap();
    let ink = conn.generate_id().unwrap();
    conn.create_gc(ink, bitmap, &CreateGCAux::new().foreground(1).background(0))
        .unwrap();
    conn.poly_fill_rectangle(
        bitmap,
        ink,
        &[
            Rectangle {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            Rectangle {
                x: 9,
                y: 1,
                width: 1,
                height: 1,
            },
        ],
    )
    .unwrap();
    let bits = conn
        .get_image(ImageFormat::Z_PIXMAP, bitmap, 0, 0, 10, 2, u32::MAX)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(bits.depth, 1);
    assert_eq!(
        bits.data,
        [0b0000_0001, 0, 0, 0, 0, 0b0000_0010, 0, 0],
        "rows are padded to 32 bits"
    );

    let outside = conn
        .get_image(ImageFormat::Z_PIXMAP, window, 15, 0, 10, 1, u32::MAX)
        .unwrap()
        .reply();
    assert_eq!(error_kind(outside), ErrorKind::Match);
    let planes = conn
        .get_image(ImageFormat::XY_PIXMAP, window, 0, 0, 1, 1, u32::MAX)
        .unwrap()
        .reply();
    assert_eq!(error_kind(planes), ErrorKind::Implementation);
}

#[test]
fn send_event_reaches_the_owner_or_the_selected_listeners() {
    let path = start_server("sendevent");
    let sender = connect(&path);
    let receiver = connect(&path);
    let root = sender.setup().roots[0].root;
    let target = input_window(&receiver, root, (0, 0, 50), EventMask::STRUCTURE_NOTIFY);
    drain_events(&receiver);
    let atom = sender
        .intern_atom(false, b"MY_MESSAGE")
        .unwrap()
        .reply()
        .unwrap()
        .atom;

    let message = ClientMessageEvent::new(32, target, atom, [1, 2, 3, 4, 5]);
    sender
        .send_event(false, target, EventMask::NO_EVENT, message)
        .unwrap();
    sender.flush().unwrap();
    let Event::ClientMessage(received) = next_event(&receiver) else {
        panic!("expected ClientMessage");
    };
    assert_eq!((received.window, received.type_), (target, atom));
    assert_eq!(received.data.as_data32(), [1, 2, 3, 4, 5]);
    assert_eq!(received.format, 32);

    let bad = sender
        .send_event(false, target, EventMask::NO_EVENT, [1u8; 32])
        .unwrap()
        .check();
    assert_eq!(
        error_kind(bad),
        ErrorKind::Value,
        "event types 0 and 1 cannot be sent"
    );
    let missing = sender
        .send_event(false, 0x7777_7777u32, EventMask::NO_EVENT, message)
        .unwrap()
        .check();
    assert_eq!(error_kind(missing), ErrorKind::Window);
}

#[test]
fn selections_are_owned_converted_and_cleared() {
    let path = start_server("selection");
    let owner = connect(&path);
    let asker = connect(&path);
    let root = owner.setup().roots[0].root;
    let owner_window = input_window(&owner, root, (0, 0, 20), EventMask::NO_EVENT);
    let asker_window = input_window(&asker, root, (50, 0, 20), EventMask::NO_EVENT);
    sync(&owner);
    sync(&asker);
    // SECONDARY: the server asks the owner of PRIMARY and CLIPBOARD for their text, and this test
    // follows the plain protocol without that request.
    let primary = u32::from(AtomEnum::SECONDARY);
    let string = u32::from(AtomEnum::STRING);
    let property = asker
        .intern_atom(false, b"PASTE")
        .unwrap()
        .reply()
        .unwrap()
        .atom;

    let nobody = asker
        .get_selection_owner(primary)
        .unwrap()
        .reply()
        .unwrap()
        .owner;
    assert_eq!(nobody, 0);
    asker
        .convert_selection(asker_window, primary, string, property, x11rb::CURRENT_TIME)
        .unwrap();
    asker.flush().unwrap();
    let Event::SelectionNotify(none) = next_event(&asker) else {
        panic!("with no owner the requestor gets SelectionNotify");
    };
    assert_eq!(none.property, 0);

    owner
        .set_selection_owner(owner_window, primary, x11rb::CURRENT_TIME)
        .unwrap();
    sync(&owner);
    assert_eq!(
        asker
            .get_selection_owner(primary)
            .unwrap()
            .reply()
            .unwrap()
            .owner,
        owner_window
    );
    asker
        .convert_selection(asker_window, primary, string, property, x11rb::CURRENT_TIME)
        .unwrap();
    asker.flush().unwrap();
    let Event::SelectionRequest(request) = next_event(&owner) else {
        panic!("the owner must get SelectionRequest");
    };
    assert_eq!(
        (
            request.requestor,
            request.selection,
            request.target,
            request.property
        ),
        (asker_window, primary, string, property)
    );

    let answer = x11rb::protocol::xproto::SelectionNotifyEvent {
        response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: request.time,
        requestor: request.requestor,
        selection: request.selection,
        target: request.target,
        property: request.property,
    };
    owner
        .send_event(false, request.requestor, EventMask::NO_EVENT, answer)
        .unwrap();
    owner.flush().unwrap();
    let Event::SelectionNotify(done) = next_event(&asker) else {
        panic!("the requestor must get the answer");
    };
    assert_eq!((done.selection, done.property), (primary, property));

    asker
        .set_selection_owner(asker_window, primary, x11rb::CURRENT_TIME)
        .unwrap();
    asker.flush().unwrap();
    assert!(matches!(next_event(&owner), Event::SelectionClear(e) if e.selection == primary));
    asker.destroy_window(asker_window).unwrap();
    assert_eq!(
        asker
            .get_selection_owner(primary)
            .unwrap()
            .reply()
            .unwrap()
            .owner,
        0
    );
}

const FRAME_BLUE: [u8; 3] = [0xA4, 0x65, 0x34];

#[test]
fn the_window_manager_frames_places_and_focuses_a_new_window() {
    let (path, display) = start_server_wm("wmframe");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = conn.generate_id().unwrap();
    conn.create_window(
        24,
        window,
        root,
        0,
        0,
        100,
        60,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .background_pixel(0xFFFFFF)
            .event_mask(EventMask::STRUCTURE_NOTIFY),
    )
    .unwrap();
    let name = AtomEnum::WM_NAME;
    conn.change_property8(PropMode::REPLACE, window, name, AtomEnum::STRING, b"Hello")
        .unwrap();
    conn.map_window(window).unwrap();
    sync(&conn);

    let geometry = conn.get_geometry(window).unwrap().reply().unwrap();
    assert_eq!(
        (geometry.x, geometry.y),
        (4, 22),
        "the title bar is on the screen"
    );
    assert_eq!(
        conn.get_input_focus().unwrap().reply().unwrap().focus,
        window
    );
    let moved = drain_events(&conn)
        .into_iter()
        .any(|event| matches!(event, Event::ConfigureNotify(e) if (e.x, e.y) == (4, 22)));
    assert!(moved, "the client is told that it was moved");

    assert_eq!(screen_pixel(&display, 10, 30), WHITE, "the window itself");
    assert_eq!(
        screen_pixel(&display, 1, 30),
        FRAME_BLUE,
        "the left border of the frame"
    );
    assert_eq!(screen_pixel(&display, 30, 3), FRAME_BLUE, "the title bar");
    let title_ink = (8..40)
        .flat_map(|x| (0..20).map(move |y| (x, y)))
        .filter(|&(x, y)| screen_pixel(&display, x, y) == WHITE)
        .count();
    assert!(
        title_ink > 10,
        "the title text is drawn, found {title_ink} pixels"
    );
}

#[test]
fn dragging_the_title_bar_moves_the_window_without_telling_the_client_about_the_clicks() {
    let (path, display) = start_server_wm("wmdrag");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(
        &conn,
        root,
        (100, 100, 100),
        EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::STRUCTURE_NOTIFY,
    );
    sync(&conn);
    drain_events(&conn);

    pointer_to(&display, 120, 90);
    button(&display, 1, true);
    pointer_to(&display, 220, 190);
    button(&display, 1, false);
    let mut moved = None;
    for _ in 0..20 {
        let geometry = conn.get_geometry(window).unwrap().reply().unwrap();
        if (geometry.x, geometry.y) == (200, 200) {
            moved = Some(geometry);
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        moved.is_some(),
        "the window followed the pointer by (100, 100)"
    );
    let events = drain_events(&conn);
    assert!(!events
        .iter()
        .any(|e| matches!(e, Event::ButtonPress(_) | Event::ButtonRelease(_))));
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::ConfigureNotify(c) if (c.x, c.y) == (200, 200))));
}

#[test]
fn clicking_a_window_raises_it_and_gives_it_the_focus() {
    let (path, display) = start_server_wm("wmraise");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let lower = input_window(&conn, root, (100, 100, 100), EventMask::BUTTON_PRESS);
    let upper = input_window(&conn, root, (150, 100, 100), EventMask::BUTTON_PRESS);
    sync(&conn);
    assert_eq!(
        conn.get_input_focus().unwrap().reply().unwrap().focus,
        upper
    );
    drain_events(&conn);

    pointer_to(&display, 110, 150);
    button(&display, 1, true);
    let Event::ButtonPress(press) = next_event(&conn) else {
        panic!("the click on the client area still reaches the client");
    };
    assert_eq!(press.event, lower);
    let order = conn.query_tree(root).unwrap().reply().unwrap().children;
    assert_eq!(order, [upper, lower], "the clicked window is on top");
    assert_eq!(
        conn.get_input_focus().unwrap().reply().unwrap().focus,
        lower
    );
}

#[test]
fn the_close_button_asks_a_cooperative_client_and_destroys_the_others() {
    let (path, display) = start_server_wm("wmclose");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let polite = input_window(&conn, root, (100, 100, 100), EventMask::STRUCTURE_NOTIFY);
    let rude = input_window(&conn, root, (300, 100, 100), EventMask::STRUCTURE_NOTIFY);
    let protocols = conn
        .intern_atom(false, b"WM_PROTOCOLS")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let delete = conn
        .intern_atom(false, b"WM_DELETE_WINDOW")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    conn.change_property32(
        PropMode::REPLACE,
        polite,
        protocols,
        AtomEnum::ATOM,
        &[delete],
    )
    .unwrap();
    sync(&conn);
    drain_events(&conn);

    pointer_to(&display, 190, 90);
    button(&display, 1, true);
    button(&display, 1, false);
    let Event::ClientMessage(message) = next_event(&conn) else {
        panic!("expected WM_DELETE_WINDOW");
    };
    assert_eq!((message.window, message.type_), (polite, protocols));
    assert_eq!(message.data.as_data32()[0], delete);
    assert!(
        conn.get_geometry(polite).unwrap().reply().is_ok(),
        "the client decides"
    );

    pointer_to(&display, 390, 90);
    button(&display, 1, true);
    button(&display, 1, false);
    let destroyed = loop {
        if let Event::DestroyNotify(e) = next_event(&conn) {
            break e.window;
        }
    };
    assert_eq!(destroyed, rude);
}

#[test]
fn another_client_with_substructure_redirect_becomes_the_window_manager() {
    let (path, _) = start_server_wm("wmexternal");
    let manager = connect(&path);
    let app = connect(&path);
    let root = manager.setup().roots[0].root;
    manager
        .change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new()
                .event_mask(EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY),
        )
        .unwrap()
        .check()
        .unwrap();
    let second_manager = app
        .change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::SUBSTRUCTURE_REDIRECT),
        )
        .unwrap()
        .check();
    assert_eq!(
        error_kind(second_manager),
        ErrorKind::Access,
        "only one window manager"
    );

    let window = new_window(
        &app,
        root,
        &CreateWindowAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
    );
    app.map_window(window).unwrap();
    sync(&app);
    let Event::MapRequest(request) =
        wait_for_event(&manager, |e| matches!(e, Event::MapRequest(_)))
    else {
        panic!("the window manager must get a MapRequest");
    };
    assert_eq!((request.window, request.parent), (window, root));
    let state = app.get_window_attributes(window).unwrap().reply().unwrap();
    assert_eq!(state.map_state, x11rb::protocol::xproto::MapState::UNMAPPED);

    app.configure_window(window, &ConfigureWindowAux::new().x(30).y(40).width(70))
        .unwrap();
    sync(&app);
    let Event::ConfigureRequest(configure) =
        wait_for_event(&manager, |e| matches!(e, Event::ConfigureRequest(_)))
    else {
        panic!("the window manager must get a ConfigureRequest");
    };
    assert_eq!(
        (configure.window, configure.x, configure.y, configure.width),
        (window, 30, 40, 70)
    );
    let unchanged = app.get_geometry(window).unwrap().reply().unwrap();
    assert_eq!(
        (unchanged.x, unchanged.width),
        (0, 50),
        "nothing changes until the manager decides"
    );

    manager.map_window(window).unwrap();
    manager
        .configure_window(window, &ConfigureWindowAux::new().x(30).y(40))
        .unwrap();
    let frame = new_window(&manager, root, &CreateWindowAux::new());
    manager.reparent_window(window, frame, 5, 6).unwrap();
    manager.flush().unwrap();
    let reparented = loop {
        if let Event::ReparentNotify(e) = next_event(&app) {
            break e;
        }
    };
    assert_eq!(
        (
            reparented.window,
            reparented.parent,
            reparented.x,
            reparented.y
        ),
        (window, frame, 5, 6)
    );
    sync(&manager);
    assert_eq!(
        manager.query_tree(frame).unwrap().reply().unwrap().children,
        [window]
    );
    assert!(!manager
        .query_tree(root)
        .unwrap()
        .reply()
        .unwrap()
        .children
        .contains(&window));
}

fn wait_for_cursor(
    tiles: &mut tokio::sync::broadcast::Receiver<xsrv_server::display::TileMessage>,
) -> wire::CursorShape {
    for _ in 0..500 {
        while let Ok(message) = tiles.try_recv() {
            if let Ok(wire::Message::Cursor(shape)) = wire::decode(&message) {
                return shape;
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("no cursor message arrived");
}

#[test]
fn the_cursor_of_the_window_under_the_pointer_reaches_the_page() {
    let (path, display) = start_server_with_display("cursorshape");
    let mut tiles = display.subscribe();
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let font = conn.generate_id().unwrap();
    conn.open_font(font, b"cursor").unwrap();
    let text_cursor = conn.generate_id().unwrap();
    conn.create_glyph_cursor(
        text_cursor,
        font,
        font,
        152,
        153,
        0,
        0,
        0,
        0xFFFF,
        0xFFFF,
        0xFFFF,
    )
    .unwrap();
    let outer = input_window(&conn, root, (100, 100, 100), EventMask::NO_EVENT);
    conn.change_window_attributes(outer, &ChangeWindowAttributesAux::new().cursor(text_cursor))
        .unwrap();
    let inner = input_window(&conn, outer, (10, 10, 20), EventMask::NO_EVENT);
    sync(&conn);

    pointer_to(&display, 150, 150);
    assert_eq!(wait_for_cursor(&mut tiles), wire::CursorShape::Text);
    pointer_to(&display, 5, 5);
    assert_eq!(wait_for_cursor(&mut tiles), wire::CursorShape::Default);
    pointer_to(&display, 115, 115);
    assert_eq!(
        wait_for_cursor(&mut tiles),
        wire::CursorShape::Text,
        "the inner window has no cursor of its own and takes the one of its parent"
    );
    assert_eq!(
        display.subscribe_with_snapshot().1 .2,
        wire::encode_cursor(wire::CursorShape::Text)
    );
    assert_ne!(inner, 0);
}

/// The page switches the keyboard layout of the session; running clients hear it with
/// MappingNotify and read the new mapping, and every page learns the layout.
#[test]
fn a_layout_switch_from_the_page_reaches_running_clients() {
    let (path, display) = start_server_with_display("layout");
    let mut tiles = display.subscribe();
    let conn = connect(&path);
    let semicolon = 39 + 8;
    let keysyms = |conn: &RustConnection| {
        let reply = conn
            .get_keyboard_mapping(semicolon, 1)
            .unwrap()
            .reply()
            .unwrap();
        reply.keysyms[0]
    };
    assert_eq!(keysyms(&conn), 0x3B, "us: semicolon");

    display.send_input(Input::Layout { layout: 1 });
    let mut requests = Vec::new();
    while requests.len() < 2 {
        if let x11rb::protocol::Event::MappingNotify(event) = conn.wait_for_event().unwrap() {
            requests.push(event.request);
        }
    }
    assert_eq!(
        requests,
        [
            x11rb::protocol::xproto::Mapping::MODIFIER,
            x11rb::protocol::xproto::Mapping::KEYBOARD
        ]
    );
    assert_eq!(keysyms(&conn), 0xF1, "es: n with tilde");
    let layout = (0..500)
        .find_map(|_| {
            let found =
                std::iter::from_fn(|| tiles.try_recv().ok()).find_map(
                    |message| match wire::decode(&message) {
                        Ok(wire::Message::Layout(layout)) => Some(layout),
                        _ => None,
                    },
                );
            found.or_else(|| {
                thread::sleep(Duration::from_millis(10));
                None
            })
        })
        .expect("the pages hear of the new layout");
    assert_eq!(wire::LAYOUTS[usize::from(layout)], "es");
}

/// libXcursor makes theme cursors from bitmaps (CreateCursor) and names them with XFIXES; GTK
/// stopped with BadRequest when CreateCursor was missing.
#[test]
fn bitmap_cursors_take_their_shape_from_the_name_or_hide_the_pointer() {
    use x11rb::protocol::xfixes::ConnectionExt as _;
    let (path, display) = start_server_with_display("bitmapcursor");
    let mut tiles = display.subscribe();
    let conn = connect(&path);
    conn.xfixes_query_version(5, 0).unwrap().reply().unwrap();
    let root = conn.setup().roots[0].root;
    let bitmap = |fill: u32| {
        let pixmap = conn.generate_id().unwrap();
        conn.create_pixmap(1, pixmap, root, 16, 16).unwrap();
        let gc = conn.generate_id().unwrap();
        conn.create_gc(gc, pixmap, &CreateGCAux::new().foreground(fill))
            .unwrap();
        conn.poly_fill_rectangle(
            pixmap,
            gc,
            &[Rectangle {
                x: 0,
                y: 0,
                width: 16,
                height: 16,
            }],
        )
        .unwrap();
        pixmap
    };
    let (shape, solid, empty) = (bitmap(1), bitmap(1), bitmap(0));
    let named = conn.generate_id().unwrap();
    conn.create_cursor(named, shape, solid, 0, 0, 0, 0xFFFF, 0xFFFF, 0xFFFF, 1, 1)
        .unwrap()
        .check()
        .unwrap();
    conn.xfixes_set_cursor_name(named, b"hand2")
        .unwrap()
        .check()
        .unwrap();
    let hidden = conn.generate_id().unwrap();
    conn.create_cursor(hidden, shape, empty, 0, 0, 0, 0, 0, 0, 0, 0)
        .unwrap()
        .check()
        .unwrap();
    let left = input_window(&conn, root, (100, 100, 50), EventMask::NO_EVENT);
    conn.change_window_attributes(left, &ChangeWindowAttributesAux::new().cursor(named))
        .unwrap();
    let right = input_window(&conn, root, (200, 100, 50), EventMask::NO_EVENT);
    conn.change_window_attributes(right, &ChangeWindowAttributesAux::new().cursor(hidden))
        .unwrap();
    sync(&conn);

    pointer_to(&display, 120, 120);
    assert_eq!(wait_for_cursor(&mut tiles), wire::CursorShape::Pointer);
    pointer_to(&display, 220, 120);
    assert_eq!(wait_for_cursor(&mut tiles), wire::CursorShape::Hidden);

    let deep = conn.generate_id().unwrap();
    conn.create_pixmap(24, deep, root, 16, 16).unwrap();
    let wrong = conn.generate_id().unwrap();
    let refused = conn
        .create_cursor(wrong, deep, 0u32, 0, 0, 0, 0, 0, 0, 0, 0)
        .unwrap()
        .check();
    assert_eq!(
        error_kind(refused),
        ErrorKind::Match,
        "the source must be a bitmap"
    );
    let refused = conn
        .create_cursor(wrong, shape, 0u32, 0, 0, 0, 0, 0, 0, 17, 0)
        .unwrap()
        .check();
    assert_eq!(
        error_kind(refused),
        ErrorKind::Match,
        "the hot spot must be inside or on the edge"
    );
    let edge = conn.generate_id().unwrap();
    conn.create_cursor(edge, shape, shape, 0, 0, 0, 0, 0, 0, 16, 16)
        .unwrap()
        .check()
        .expect("a hot spot on the edge, as GTK's blank cursor has");
}

/// Connections that send no setup are limited, so a local user cannot keep many open.
#[test]
fn connections_without_a_setup_are_limited() {
    use std::io::Read;
    let path = start_server("setup-limit");
    // The server accepts in order, so the 33rd connection comes after the 32 silent ones.
    let mut silent: Vec<_> = (0..32)
        .map(|_| UnixStream::connect(&path).unwrap())
        .collect();
    let mut refused = UnixStream::connect(&path).unwrap();
    refused
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(refused.read(&mut [0; 8]).unwrap(), 0, "closed at once");
    silent.pop();
    thread::sleep(Duration::from_millis(100));
    let conn = connect(&path);
    assert!(conn.get_input_focus().unwrap().reply().is_ok());
}

#[test]
fn list_fonts_with_info_sends_one_reply_for_each_font() {
    let conn = connect(&start_server("fontinfo"));
    let replies: Vec<_> = conn
        .list_fonts_with_info(10, b"fix*")
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!(
        replies[0].name, b"fixed",
        "the name comes after the properties"
    );
    assert_eq!((replies[0].font_ascent, replies[0].font_descent), (13, 3));
    assert_eq!(replies[0].properties.len(), 17);
    let none: Vec<_> = conn
        .list_fonts_with_info(10, b"nothing*")
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(none.is_empty());
    assert!(
        conn.get_input_focus().unwrap().reply().is_ok(),
        "the connection is still in sync"
    );
}

#[test]
fn dragging_the_right_and_bottom_edges_resizes_the_window() {
    let (path, display) = start_server_wm("wmresize");
    let mut tiles = display.subscribe();
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(
        &conn,
        root,
        (100, 100, 100),
        EventMask::STRUCTURE_NOTIFY | EventMask::EXPOSURE,
    );
    sync(&conn);
    drain_events(&conn);

    pointer_to(&display, 202, 202);
    assert_eq!(wait_for_cursor(&mut tiles), wire::CursorShape::ResizeNwse);
    button(&display, 1, true);
    pointer_to(&display, 252, 232);
    button(&display, 1, false);
    let mut size = (0, 0);
    for _ in 0..50 {
        let geometry = conn.get_geometry(window).unwrap().reply().unwrap();
        size = (geometry.width, geometry.height);
        if size == (152, 132) {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(size, (152, 132));
    let events = drain_events(&conn);
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::ConfigureNotify(c) if (c.width, c.height) == (152, 132))));
    assert!(events
        .iter()
        .any(|e| matches!(e, Event::Expose(x) if (x.width, x.height) == (152, 132))));

    pointer_to(&display, 120, 50);
    button(&display, 1, true);
    pointer_to(&display, 120, 52);
    button(&display, 1, false);
    pointer_to(&display, 253, 150);
    loop {
        if wait_for_cursor(&mut tiles) == wire::CursorShape::ResizeEw {
            break;
        }
    }
}

#[test]
fn big_requests_allow_a_request_over_256_kib() {
    let conn = connect(&start_server("bigreq"));
    assert!(
        conn.maximum_request_bytes() > 1 << 20,
        "BIG-REQUESTS was enabled when the connection was made"
    );
    let root = conn.setup().roots[0].root;
    let pixmap = conn.generate_id().unwrap();
    conn.create_pixmap(24, pixmap, root, 300, 300).unwrap();
    let gc = conn.generate_id().unwrap();
    conn.create_gc(gc, pixmap, &CreateGCAux::new()).unwrap();
    let mut data = vec![0u8; 300 * 300 * 4];
    data[(299 * 300 + 299) * 4..][..3].copy_from_slice(&[1, 2, 3]);
    assert!(
        data.len() > 262_140,
        "more than a request can hold without BIG-REQUESTS"
    );
    conn.put_image(
        ImageFormat::Z_PIXMAP,
        pixmap,
        gc,
        300,
        300,
        0,
        0,
        0,
        24,
        &data,
    )
    .unwrap();
    let back = conn
        .get_image(ImageFormat::Z_PIXMAP, pixmap, 299, 299, 1, 1, u32::MAX)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(back.data, [1, 2, 3, 0]);
}

#[test]
fn xc_misc_reports_free_resource_ids() {
    use x11rb::protocol::xc_misc::ConnectionExt as _;
    let conn = connect(&start_server("xcmisc"));
    let version = conn.xc_misc_get_version(1, 1).unwrap().reply().unwrap();
    assert_eq!(
        (version.server_major_version, version.server_minor_version),
        (1, 1)
    );

    let used = conn.generate_id().unwrap();
    let root = conn.setup().roots[0].root;
    conn.create_pixmap(24, used, root, 2, 2).unwrap();
    sync(&conn);
    let range = conn.xc_misc_get_xid_range().unwrap().reply().unwrap();
    assert!(range.count > 1000);
    assert!(
        used < range.start_id || used >= range.start_id + range.count,
        "the id range does not contain an id that is in use"
    );
    let list = conn.xc_misc_get_xid_list(5).unwrap().reply().unwrap();
    assert_eq!(list.ids.len(), 5);
    assert!(!list.ids.contains(&used));
    assert!(list.ids.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn extensions_are_listed_and_queried() {
    let conn = connect(&start_server("extlist"));
    let listed = conn.list_extensions().unwrap().reply().unwrap();
    let names: Vec<_> = listed.names.iter().map(|s| s.name.clone()).collect();
    assert!(names.contains(&b"BIG-REQUESTS".to_vec()));
    assert!(names.contains(&b"XC-MISC".to_vec()));
    let present = conn.query_extension(b"XC-MISC").unwrap().reply().unwrap();
    assert!(present.present && present.major_opcode >= 128);
    let missing = conn
        .query_extension(b"NOT-AN-EXTENSION")
        .unwrap()
        .reply()
        .unwrap();
    assert!(!missing.present);
}

#[test]
fn a_shaped_window_is_drawn_and_hit_only_inside_its_shape() {
    use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
    let (path, display) = start_server_with_display("shape");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let version = conn.shape_query_version().unwrap().reply().unwrap();
    assert_eq!((version.major_version, version.minor_version), (1, 1));
    let window = conn.generate_id().unwrap();
    conn.create_window(
        24,
        window,
        root,
        100,
        100,
        50,
        50,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new()
            .background_pixel(0xFFFFFF)
            .event_mask(EventMask::ENTER_WINDOW),
    )
    .unwrap();
    conn.map_window(window).unwrap();
    conn.shape_select_input(window, true).unwrap();
    conn.shape_rectangles(
        SO::SET,
        SK::BOUNDING,
        ClipOrdering::UNSORTED,
        window,
        0,
        0,
        &[Rectangle {
            x: 0,
            y: 0,
            width: 25,
            height: 50,
        }],
    )
    .unwrap();
    sync(&conn);
    let at = |x: usize, y: usize| screen_pixel(&display, x, y);
    assert_eq!(at(110, 110), WHITE, "inside the shape");
    assert_eq!(at(130, 110), BLACK, "outside the shape nothing is drawn");

    let notified = wait_for_event(&conn, |e| matches!(e, Event::ShapeNotify(_)));
    let Event::ShapeNotify(shape) = notified else {
        unreachable!()
    };
    assert_eq!((shape.affected_window, shape.shaped), (window, true));
    assert_eq!((shape.extents_width, shape.extents_height), (25, 50));

    pointer_to(&display, 130, 110);
    pointer_to(&display, 110, 110);
    let Event::EnterNotify(enter) = wait_for_event(&conn, |e| matches!(e, Event::EnterNotify(_)))
    else {
        unreachable!()
    };
    assert_eq!(
        (enter.event, enter.event_x),
        (window, 10),
        "the pointer went through the hole"
    );

    let extents = conn.shape_query_extents(window).unwrap().reply().unwrap();
    assert!(extents.bounding_shaped && !extents.clip_shaped);
    assert_eq!(extents.bounding_shape_extents_width, 25);
    assert!(
        conn.shape_input_selected(window)
            .unwrap()
            .reply()
            .unwrap()
            .enabled
    );

    conn.shape_rectangles(
        SO::SUBTRACT,
        SK::BOUNDING,
        ClipOrdering::UNSORTED,
        window,
        0,
        0,
        &[Rectangle {
            x: 5,
            y: 5,
            width: 10,
            height: 10,
        }],
    )
    .unwrap();
    sync(&conn);
    assert_eq!(at(110, 110), BLACK, "a hole was cut");
    assert_eq!(at(102, 102), WHITE);
    let rects = conn
        .shape_get_rectangles(window, SK::BOUNDING)
        .unwrap()
        .reply()
        .unwrap()
        .rectangles;
    let area: u32 = rects
        .iter()
        .map(|r| u32::from(r.width) * u32::from(r.height))
        .sum();
    assert_eq!(area, 25 * 50 - 100);
}

#[test]
fn a_bitmap_makes_a_shape_and_offset_and_combine_move_it() {
    use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
    let (path, display) = start_server_with_display("shapemask");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let (window, _) = mapped_window(&conn, 100, 100, 20);
    conn.change_window_attributes(
        window,
        &ChangeWindowAttributesAux::new().background_pixel(0xFFFFFF),
    )
    .unwrap();
    let bitmap = conn.generate_id().unwrap();
    conn.create_pixmap(1, bitmap, root, 20, 20).unwrap();
    let ink = conn.generate_id().unwrap();
    conn.create_gc(ink, bitmap, &CreateGCAux::new().foreground(1).background(0))
        .unwrap();
    conn.poly_fill_rectangle(
        bitmap,
        ink,
        &[Rectangle {
            x: 0,
            y: 0,
            width: 4,
            height: 4,
        }],
    )
    .unwrap();
    conn.shape_mask(SO::SET, SK::BOUNDING, window, 0, 0, bitmap)
        .unwrap();
    sync(&conn);
    let at = |x: usize, y: usize| screen_pixel(&display, x, y);
    conn.change_window_attributes(
        window,
        &ChangeWindowAttributesAux::new().background_pixel(0xFFFFFF),
    )
    .unwrap();
    conn.clear_area(false, window, 0, 0, 0, 0).unwrap();
    sync(&conn);
    assert_eq!(
        (at(101, 101), at(110, 110)),
        (WHITE, BLACK),
        "only the 4x4 corner is shown"
    );

    conn.shape_offset(SK::BOUNDING, window, 10, 10).unwrap();
    sync(&conn);
    assert_eq!(
        (at(101, 101), at(111, 111)),
        (BLACK, WHITE),
        "the shape moved with the offset"
    );

    let other = conn.generate_id().unwrap();
    conn.create_window(
        24,
        other,
        root,
        300,
        300,
        30,
        30,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    conn.shape_rectangles(
        SO::SET,
        SK::BOUNDING,
        ClipOrdering::UNSORTED,
        other,
        0,
        0,
        &[Rectangle {
            x: 0,
            y: 0,
            width: 3,
            height: 3,
        }],
    )
    .unwrap();
    conn.shape_combine(SO::SET, SK::BOUNDING, SK::BOUNDING, window, 0, 0, other)
        .unwrap();
    let rects = conn
        .shape_get_rectangles(window, SK::BOUNDING)
        .unwrap()
        .reply()
        .unwrap()
        .rectangles;
    assert_eq!(rects.len(), 1);
    assert_eq!((rects[0].width, rects[0].height), (3, 3));

    let bad = conn
        .shape_mask(SO::SET, SK::BOUNDING, window, 0, 0, root)
        .unwrap()
        .check();
    assert_eq!(error_kind(bad), ErrorKind::Pixmap);
    conn.shape_mask(SO::SET, SK::BOUNDING, window, 0, 0, 0u32)
        .unwrap()
        .check()
        .unwrap();
    assert!(
        !conn
            .shape_query_extents(window)
            .unwrap()
            .reply()
            .unwrap()
            .bounding_shaped
    );
}

#[test]
fn randr_describes_the_screen_and_resizes_it() {
    use x11rb::protocol::randr::{ConnectionExt as _, NotifyMask};
    let (path, display) = start_server_with_display("randr");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let version = conn.randr_query_version(1, 5).unwrap().reply().unwrap();
    assert_eq!((version.major_version, version.minor_version), (1, 5));
    let older = conn.randr_query_version(1, 1).unwrap().reply().unwrap();
    assert_eq!((older.major_version, older.minor_version), (1, 1));

    let resources = conn
        .randr_get_screen_resources(root)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(
        (
            resources.crtcs.len(),
            resources.outputs.len(),
            resources.modes.len()
        ),
        (1, 1, 1)
    );
    assert_eq!(
        (resources.modes[0].width, resources.modes[0].height),
        (640, 480)
    );
    assert_eq!(resources.names, b"640x480");
    let output = conn
        .randr_get_output_info(resources.outputs[0], resources.config_timestamp)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(output.crtc, resources.crtcs[0]);
    assert_eq!(output.name, b"xsrv-0");
    let crtc = conn
        .randr_get_crtc_info(resources.crtcs[0], resources.config_timestamp)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!((crtc.width, crtc.height), (640, 480));
    let range = conn
        .randr_get_screen_size_range(root)
        .unwrap()
        .reply()
        .unwrap();
    assert!(range.max_width >= 1920 && range.min_width <= 640);

    let missing = conn.randr_get_output_info(0x1234, 0).unwrap().reply();
    assert!(matches!(missing, Err(ReplyError::X11Error(e)) if e.error_code == 130));

    conn.randr_select_input(root, NotifyMask::SCREEN_CHANGE | NotifyMask::CRTC_CHANGE)
        .unwrap();
    let (window, gc) = mapped_window(&conn, 100, 100, 20);
    conn.poly_fill_rectangle(
        window,
        gc,
        &[Rectangle {
            x: 0,
            y: 0,
            width: 20,
            height: 20,
        }],
    )
    .unwrap();
    conn.randr_set_screen_size(root, 800, 600, 211, 159)
        .unwrap();
    conn.flush().unwrap();
    let Event::RandrScreenChangeNotify(change) =
        wait_for_event(&conn, |e| matches!(e, Event::RandrScreenChangeNotify(_)))
    else {
        unreachable!()
    };
    assert_eq!((change.width, change.height), (800, 600));
    assert!(matches!(
        wait_for_event(&conn, |e| matches!(e, Event::RandrNotify(_))),
        Event::RandrNotify(_)
    ));
    sync(&conn);

    assert_eq!(display.size(), (800, 600));
    let geometry = conn.get_geometry(root).unwrap().reply().unwrap();
    assert_eq!((geometry.width, geometry.height), (800, 600));
    assert_eq!(
        screen_pixel(&display, 110, 110),
        WHITE,
        "windows are painted on the new screen"
    );
    let (screen, full, _) = display.subscribe_with_snapshot().1;
    assert_eq!(
        wire::decode(&screen),
        Ok(wire::Message::Screen {
            width: 800,
            height: 600
        })
    );
    assert_eq!(tile_pixels(&full).len(), 800 * 600 * 4);

    let late = connect(&path);
    assert_eq!(
        (
            late.setup().roots[0].width_in_pixels,
            late.setup().roots[0].height_in_pixels
        ),
        (800, 600),
        "a new client sees the new size"
    );
    let too_small = conn
        .randr_set_screen_size(root, 10, 10, 1, 1)
        .unwrap()
        .check();
    assert_eq!(error_kind(too_small), ErrorKind::Value);
}

#[test]
fn randr_lists_monitors_screen_info_and_no_providers() {
    use x11rb::protocol::randr::ConnectionExt as _;
    let conn = connect(&start_server("randr15"));
    let root = conn.setup().roots[0].root;

    let monitors = conn
        .randr_get_monitors(root, true)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(monitors.monitors.len(), 1);
    let monitor = &monitors.monitors[0];
    assert_eq!((monitor.x, monitor.y), (0, 0));
    assert_eq!((monitor.width, monitor.height), (640, 480));
    assert!(monitor.primary && monitor.automatic);
    let resources = conn
        .randr_get_screen_resources(root)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(monitor.outputs, resources.outputs);
    let name = conn
        .get_atom_name(monitor.name)
        .unwrap()
        .reply()
        .unwrap()
        .name;
    assert_eq!(name, b"xsrv-0");

    let info = conn.randr_get_screen_info(root).unwrap().reply().unwrap();
    assert_eq!(info.sizes.len(), 1);
    assert_eq!((info.sizes[0].width, info.sizes[0].height), (640, 480));
    assert_eq!(info.rates.len(), 1);

    let providers = conn.randr_get_providers(root).unwrap().reply().unwrap();
    assert!(providers.providers.is_empty());

    conn.randr_set_output_primary(root, resources.outputs[0])
        .unwrap()
        .check()
        .unwrap();
    for primary in [0, resources.outputs[0]] {
        conn.randr_set_output_primary(root, primary)
            .unwrap()
            .check()
            .unwrap();
        let got = conn
            .randr_get_output_primary(root)
            .unwrap()
            .reply()
            .unwrap();
        assert_eq!(got.output, primary);
        let monitors = conn
            .randr_get_monitors(root, true)
            .unwrap()
            .reply()
            .unwrap();
        assert_eq!(monitors.monitors[0].primary, primary != 0);
    }
    let bad = conn.randr_set_output_primary(root, 0x1234).unwrap().check();
    assert!(matches!(bad, Err(ReplyError::X11Error(e)) if e.error_code == 130));
}

#[test]
fn the_page_can_resize_the_screen() {
    use x11rb::protocol::randr::{ConnectionExt as _, NotifyMask};
    let (path, display) = start_server_with_display("pageresize");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    conn.randr_select_input(root, NotifyMask::SCREEN_CHANGE)
        .unwrap();
    sync(&conn);

    display.send_input(Input::Resize {
        width: 1024,
        height: 700,
    });
    let Event::RandrScreenChangeNotify(change) =
        wait_for_event(&conn, |e| matches!(e, Event::RandrScreenChangeNotify(_)))
    else {
        unreachable!()
    };
    assert_eq!((change.width, change.height), (1024, 700));
    assert_eq!(display.size(), (1024, 700));

    display.send_input(Input::Resize {
        width: 5,
        height: 5,
    });
    display.send_input(Input::Resize {
        width: 900,
        height: 600,
    });
    let Event::RandrScreenChangeNotify(again) =
        wait_for_event(&conn, |e| matches!(e, Event::RandrScreenChangeNotify(_)))
    else {
        unreachable!()
    };
    assert_eq!(
        (again.width, again.height),
        (900, 600),
        "the size that was too small was ignored"
    );
}

#[test]
fn xfixes_regions_combine_clip_and_report_bad_region() {
    use x11rb::protocol::xfixes::ConnectionExt as _;
    let (path, display) = start_server_with_display("regions");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    conn.xfixes_query_version(5, 0).unwrap().reply().unwrap();
    let rect = |x, y, width, height| Rectangle {
        x,
        y,
        width,
        height,
    };
    let tuple = |r: &Rectangle| (r.x, r.y, r.width, r.height);

    let (left, right, both) = (
        conn.generate_id().unwrap(),
        conn.generate_id().unwrap(),
        conn.generate_id().unwrap(),
    );
    conn.xfixes_create_region(left, &[rect(0, 0, 4, 4)])
        .unwrap();
    conn.xfixes_create_region(right, &[rect(2, 0, 4, 4)])
        .unwrap();
    conn.xfixes_create_region(both, &[]).unwrap();
    conn.xfixes_union_region(left, right, both).unwrap();
    let union = conn.xfixes_fetch_region(both).unwrap().reply().unwrap();
    assert_eq!(tuple(&union.extents), (0, 0, 6, 4));
    let area: u32 = union
        .rectangles
        .iter()
        .map(|r| u32::from(r.width) * u32::from(r.height))
        .sum();
    assert_eq!(area, 24, "overlapping rectangles are not counted twice");

    conn.xfixes_intersect_region(left, right, both).unwrap();
    let common = conn.xfixes_fetch_region(both).unwrap().reply().unwrap();
    assert_eq!(
        common.rectangles.iter().map(tuple).collect::<Vec<_>>(),
        [(2, 0, 2, 4)]
    );
    conn.xfixes_translate_region(both, 10, 20).unwrap();
    let moved = conn.xfixes_fetch_region(both).unwrap().reply().unwrap();
    assert_eq!(tuple(&moved.extents), (12, 20, 2, 4));

    let window = new_window(
        &conn,
        root,
        &CreateWindowAux::new().background_pixel(0x000000),
    );
    conn.map_window(window).unwrap();
    let gc = conn.generate_id().unwrap();
    conn.create_gc(gc, window, &CreateGCAux::new().foreground(0xFF0000))
        .unwrap();
    conn.xfixes_set_gc_clip_region(gc, left, 0, 0).unwrap();
    conn.poly_fill_rectangle(window, gc, &[rect(0, 0, 8, 1)])
        .unwrap();
    sync(&conn);
    let at = |x: usize| screen_pixel(&display, x, 0);
    assert_eq!(at(3), BLUE_GREEN_RED(0xFF0000), "inside the clip region");
    assert_eq!(at(4), BLUE_GREEN_RED(0x000000), "outside the clip region");

    conn.xfixes_destroy_region(left).unwrap();
    let gone = conn.xfixes_fetch_region(left).unwrap().reply();
    assert!(matches!(gone, Err(ReplyError::X11Error(e)) if e.error_code == 129));
}

#[test]
fn xfixes_tells_a_watcher_when_the_selection_owner_changes() {
    use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
    let path = start_server("xfixes");
    let watcher = connect(&path);
    let owner = connect(&path);
    let root = watcher.setup().roots[0].root;
    let version = watcher.xfixes_query_version(5, 0).unwrap().reply().unwrap();
    assert_eq!((version.major_version, version.minor_version), (5, 0));
    let clipboard = watcher
        .intern_atom(false, b"CLIPBOARD")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let window = input_window(&watcher, root, (0, 0, 10), EventMask::NO_EVENT);
    let mask = SelectionEventMask::SET_SELECTION_OWNER
        | SelectionEventMask::SELECTION_WINDOW_DESTROY
        | SelectionEventMask::SELECTION_CLIENT_CLOSE;
    watcher
        .xfixes_select_selection_input(window, clipboard, mask)
        .unwrap();
    sync(&watcher);
    drain_events(&watcher);

    let owner_window = input_window(&owner, root, (50, 0, 10), EventMask::NO_EVENT);
    owner
        .set_selection_owner(owner_window, clipboard, x11rb::CURRENT_TIME)
        .unwrap();
    owner.flush().unwrap();
    let Event::XfixesSelectionNotify(change) =
        wait_for_event(&watcher, |e| matches!(e, Event::XfixesSelectionNotify(_)))
    else {
        unreachable!()
    };
    assert_eq!(
        (change.owner, change.selection, change.window),
        (owner_window, clipboard, window)
    );

    owner.destroy_window(owner_window).unwrap();
    owner.flush().unwrap();
    let Event::XfixesSelectionNotify(gone) =
        wait_for_event(&watcher, |e| matches!(e, Event::XfixesSelectionNotify(_)))
    else {
        unreachable!()
    };
    assert_eq!(
        gone.owner, 0,
        "nobody owns the selection after the owner window was destroyed"
    );
}

/// cairo sends a mask without alpha as a one-bit image through shared memory (a GTK 3 program on
/// a display without a desktop). The server refused it with BadMatch, and the program stopped.
#[test]
#[allow(unsafe_code)]
fn shm_put_image_takes_a_one_bit_image() {
    use x11rb::protocol::shm::ConnectionExt as _;
    let (path, display) = start_server_with_display("shm-bits");
    let conn = connect(&path);
    // SAFETY: the calls below only use the segment that this test makes, and the pointer is
    // written inside its 4096 bytes.
    let (shm_id, pointer) = unsafe {
        let id = libc::shmget(libc::IPC_PRIVATE, 4096, libc::IPC_CREAT | 0o600);
        assert!(id >= 0, "shmget failed");
        let pointer = libc::shmat(id, std::ptr::null(), 0).cast::<u8>();
        assert_ne!(pointer as isize, -1, "shmat failed");
        (id, pointer)
    };
    // Two rows of 8 pixels, each padded to 32 bits. The lowest bit is the leftmost pixel.
    let rows: [u8; 8] = [0b0000_0101, 0, 0, 0, 0b0000_0010, 0, 0, 0];
    // SAFETY: 8 bytes fit in the segment.
    unsafe { std::ptr::copy_nonoverlapping(rows.as_ptr(), pointer.add(32), rows.len()) };

    let (window, gc) = mapped_window(&conn, 100, 100, 20);
    let bitmap = conn.generate_id().unwrap();
    conn.create_pixmap(1, bitmap, window, 8, 2).unwrap();
    let bitmap_gc = conn.generate_id().unwrap();
    conn.create_gc(bitmap_gc, bitmap, &CreateGCAux::new())
        .unwrap();
    let seg = conn.generate_id().unwrap();
    conn.shm_attach(seg, shm_id as u32, true)
        .unwrap()
        .check()
        .unwrap();
    conn.shm_put_image(
        bitmap, bitmap_gc, 8, 2, 0, 0, 8, 2, 0, 0, 1, 2, false, seg, 32,
    )
    .unwrap()
    .check()
    .expect("a one-bit ZPixmap is taken");
    // Only the columns 1 to 3 of row 1, at the column 5 of the bitmap.
    conn.shm_put_image(
        bitmap, bitmap_gc, 8, 2, 1, 1, 3, 1, 5, 0, 1, 2, false, seg, 32,
    )
    .unwrap()
    .check()
    .unwrap();
    conn.change_gc(
        gc,
        &ChangeGCAux::new().foreground(0x00FF_FFFF).background(0),
    )
    .unwrap();
    conn.copy_plane(bitmap, window, gc, 0, 0, 0, 0, 8, 2, 1)
        .unwrap()
        .check()
        .unwrap();
    sync(&conn);
    let row = |y: usize| -> Vec<bool> {
        (0..8)
            .map(|x| screen_pixel(&display, 100 + x, 100 + y) == WHITE)
            .collect()
    };
    let bits = |text: &str| -> Vec<bool> { text.chars().map(|c| c == '#').collect() };
    assert_eq!(
        row(0),
        bits("#.#..#.."),
        "row 0, then row 1 from its column 1 at column 5"
    );
    assert_eq!(row(1), bits(".#......"));

    conn.shm_detach(seg).unwrap().check().unwrap();
    // SAFETY: the segment was attached above and is not used after this.
    unsafe {
        libc::shmdt(pointer.cast());
        libc::shmctl(shm_id, libc::IPC_RMID, std::ptr::null_mut());
    }
}

#[test]
#[allow(unsafe_code)]
fn shm_put_image_draws_from_shared_memory() {
    use x11rb::protocol::shm::ConnectionExt as _;
    let (path, display) = start_server_with_display("shm");
    let conn = connect(&path);
    let version = conn.shm_query_version().unwrap().reply().unwrap();
    assert_eq!((version.major_version, version.minor_version), (1, 2));
    assert!(!version.shared_pixmaps);

    // SAFETY: the calls below only use the segment that this test makes, and the pointer is
    // written inside its 4096 bytes.
    let (shm_id, pointer) = unsafe {
        let id = libc::shmget(libc::IPC_PRIVATE, 4096, libc::IPC_CREAT | 0o600);
        assert!(id >= 0, "shmget failed");
        let pointer = libc::shmat(id, std::ptr::null(), 0).cast::<u8>();
        assert_ne!(pointer as isize, -1, "shmat failed");
        (id, pointer)
    };
    let pixels: [u8; 16] = [
        1, 2, 3, 0, 4, 5, 6, 0, // row 0
        7, 8, 9, 0, 10, 11, 12, 0, // row 1
    ];
    // SAFETY: 16 bytes fit in the segment.
    unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), pointer.add(64), pixels.len()) };

    let (window, gc) = mapped_window(&conn, 100, 100, 20);
    let seg = conn.generate_id().unwrap();
    conn.shm_attach(seg, shm_id as u32, true)
        .unwrap()
        .check()
        .unwrap();
    conn.shm_put_image(window, gc, 2, 2, 1, 0, 1, 2, 5, 6, 24, 2, true, seg, 64)
        .unwrap();
    conn.flush().unwrap();
    let Event::ShmCompletion(done) =
        wait_for_event(&conn, |e| matches!(e, Event::ShmCompletion(_)))
    else {
        unreachable!()
    };
    assert_eq!((done.shmseg, done.offset, done.drawable), (seg, 64, window));
    sync(&conn);
    assert_eq!(
        screen_pixel(&display, 105, 106),
        [4, 5, 6],
        "column 1 of row 0"
    );
    assert_eq!(
        screen_pixel(&display, 105, 107),
        [10, 11, 12],
        "column 1 of row 1"
    );
    assert_eq!(
        screen_pixel(&display, 104, 106),
        BLACK,
        "column 0 was not in the source rect"
    );

    let too_far = conn
        .shm_put_image(window, gc, 2, 2, 0, 0, 2, 2, 0, 0, 24, 2, false, seg, 4090)
        .unwrap()
        .check();
    assert_eq!(error_kind(too_far), ErrorKind::Value);
    let wrong_format = conn
        .shm_put_image(window, gc, 2, 2, 0, 0, 2, 2, 0, 0, 24, 0, false, seg, 0)
        .unwrap()
        .check();
    assert_eq!(error_kind(wrong_format), ErrorKind::Match);

    // The image starts one pixel left of the window: only its second column is drawn.
    conn.shm_put_image(window, gc, 2, 2, 0, 0, 2, 2, -1, 0, 24, 2, false, seg, 64)
        .unwrap()
        .check()
        .unwrap();
    sync(&conn);
    assert_eq!(
        screen_pixel(&display, 100, 100),
        [4, 5, 6],
        "column 1 at x 0"
    );
    assert_eq!(screen_pixel(&display, 100, 101), [10, 11, 12]);
    assert_eq!(
        screen_pixel(&display, 99, 100),
        BLACK,
        "nothing outside the window"
    );

    conn.shm_detach(seg).unwrap().check().unwrap();
    let detached = conn
        .shm_put_image(window, gc, 2, 2, 0, 0, 2, 2, 0, 0, 24, 2, false, seg, 0)
        .unwrap()
        .check();
    assert!(matches!(detached, Err(ReplyError::X11Error(e)) if e.error_code == 128));
    let unknown = conn.generate_id().unwrap();
    let no_segment = conn.shm_attach(unknown, 0x7FFF_FFF0, true).unwrap().check();
    assert_eq!(error_kind(no_segment), ErrorKind::Access);

    // SAFETY: the segment was attached above and is not used after this.
    unsafe {
        libc::shmdt(pointer.cast());
        libc::shmctl(shm_id, libc::IPC_RMID, std::ptr::null_mut());
    }
}

#[test]
fn one_drawing_request_cannot_write_without_limit() {
    let conn = connect(&start_server("worklimit"));
    let root = conn.setup().roots[0].root;
    let pixmap = conn.generate_id().unwrap();
    conn.create_pixmap(24, pixmap, root, 1000, 1000).unwrap();
    let gc = conn.generate_id().unwrap();
    conn.create_gc(gc, pixmap, &CreateGCAux::new().foreground(0xFFFFFF))
        .unwrap();
    let full = Rectangle {
        x: 0,
        y: 0,
        width: 1000,
        height: 1000,
    };

    conn.poly_fill_rectangle(pixmap, gc, &vec![full; 50])
        .unwrap()
        .check()
        .unwrap();
    let too_much = conn
        .poly_fill_rectangle(pixmap, gc, &vec![full; 100])
        .unwrap()
        .check();
    assert_eq!(
        error_kind(too_much),
        ErrorKind::Alloc,
        "100 million pixels in one request"
    );
    conn.poly_fill_rectangle(pixmap, gc, &[full])
        .unwrap()
        .check()
        .unwrap();
    assert!(
        conn.get_input_focus().unwrap().reply().is_ok(),
        "the connection still works"
    );
}

#[test]
fn a_client_cannot_own_an_unlimited_number_of_resources() {
    let path = start_server("resourcequota");
    let greedy = connect(&path);
    let other = connect(&path);
    let root = greedy.setup().roots[0].root;
    let mut made = Vec::new();
    let failure = loop {
        let id = greedy.generate_id().unwrap();
        match greedy.create_pixmap(24, id, root, 1, 1).unwrap().check() {
            Ok(()) => made.push(id),
            Err(error) => break error,
        }
        assert!(made.len() <= 9000, "the quota did not stop the client");
    };
    assert_eq!(error_kind::<()>(Err(failure)), ErrorKind::Alloc);
    assert_eq!(made.len(), 8192);

    let id = other.generate_id().unwrap();
    other
        .create_pixmap(24, id, root, 1, 1)
        .unwrap()
        .check()
        .unwrap();
    greedy.free_pixmap(made[0]).unwrap().check().unwrap();
    let again = greedy.generate_id().unwrap();
    greedy
        .create_pixmap(24, again, root, 1, 1)
        .unwrap()
        .check()
        .unwrap();
}

#[test]
fn a_request_over_the_big_request_limit_closes_the_connection() {
    let mut stream = UnixStream::connect(start_server("bigcap")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(&[b'l', 0, 11, 0, 0, 0, 0, 0, 0, 0, 0, 0])
        .unwrap();
    let mut setup = [0u8; 8];
    stream.read_exact(&mut setup).unwrap();
    let mut rest = vec![0u8; u16::from_le_bytes([setup[6], setup[7]]) as usize * 4];
    stream.read_exact(&mut rest).unwrap();
    let mut extensions = Vec::new();
    extensions.extend_from_slice(&[98, 0, 5, 0, 12, 0, 0, 0]);
    extensions.extend_from_slice(b"BIG-REQUESTS");
    stream.write_all(&extensions).unwrap();
    let mut reply = [0u8; 32];
    stream.read_exact(&mut reply).unwrap();
    let opcode = reply[9];
    assert!(reply[8] == 1 && opcode >= 128, "BIG-REQUESTS is present");

    stream.write_all(&[opcode, 0, 1, 0]).unwrap();
    stream.read_exact(&mut reply).unwrap();
    let allowed_words = u32::from_le_bytes([reply[8], reply[9], reply[10], reply[11]]);
    assert_eq!(allowed_words, 1 << 20, "4 MiB");

    let mut oversized = vec![127u8, 0, 0, 0];
    oversized.extend_from_slice(&(allowed_words + 1).to_le_bytes());
    stream.write_all(&oversized).unwrap();
    let mut sink = [0u8; 1];
    assert_eq!(
        stream.read(&mut sink).unwrap_or(0),
        0,
        "the server closed the connection"
    );
}

fn atom(conn: &RustConnection<DefaultStream>, name: &[u8]) -> u32 {
    conn.intern_atom(false, name).unwrap().reply().unwrap().atom
}

#[test]
fn page_text_can_be_pasted_into_an_x_client() {
    let (path, display) = start_server_with_display("clipboard-in");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(&conn, root, (0, 0, 20), EventMask::NO_EVENT);
    let clipboard = atom(&conn, b"CLIPBOARD");
    let utf8 = atom(&conn, b"UTF8_STRING");
    let targets = atom(&conn, b"TARGETS");
    let property = atom(&conn, b"PASTE");

    display.send_clipboard_in(ClipboardData::text_only("hola ñ".into()));
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while conn
        .get_selection_owner(clipboard)
        .unwrap()
        .reply()
        .unwrap()
        .owner
        == 0
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the page text never became the owner"
        );
        thread::sleep(Duration::from_millis(5));
    }

    conn.convert_selection(window, clipboard, utf8, property, x11rb::CURRENT_TIME)
        .unwrap();
    let Event::SelectionNotify(done) =
        wait_for_event(&conn, |e| matches!(e, Event::SelectionNotify(_)))
    else {
        unreachable!()
    };
    assert_eq!((done.target, done.property), (utf8, property));
    let reply = conn
        .get_property(false, window, property, utf8, 0, 100)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(reply.value, "hola ñ".as_bytes());

    conn.convert_selection(window, clipboard, targets, property, x11rb::CURRENT_TIME)
        .unwrap();
    wait_for_event(&conn, |e| matches!(e, Event::SelectionNotify(_)));
    let list = conn
        .get_property(false, window, property, AtomEnum::ATOM, 0, 100)
        .unwrap()
        .reply()
        .unwrap();
    assert!(list.value32().unwrap().any(|atom| atom == utf8));

    let bad = atom(&conn, b"NO_SUCH_TARGET");
    conn.convert_selection(window, clipboard, bad, property, x11rb::CURRENT_TIME)
        .unwrap();
    let Event::SelectionNotify(refused) =
        wait_for_event(&conn, |e| matches!(e, Event::SelectionNotify(_)))
    else {
        unreachable!()
    };
    assert_eq!(refused.property, 0, "an unknown target is refused");
}

#[test]
fn text_of_an_x_client_reaches_the_page() {
    let (path, display) = start_server_with_display("clipboard-out");
    let mut page = display.subscribe();
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(&conn, root, (0, 0, 20), EventMask::NO_EVENT);
    let clipboard = atom(&conn, b"CLIPBOARD");

    conn.set_selection_owner(window, clipboard, x11rb::CURRENT_TIME)
        .unwrap();
    // The server asks for TARGETS, then for the text.
    serve_selection(
        &conn,
        &[Offer::new(b"UTF8_STRING", "texto ñ".as_bytes().to_vec())],
        true,
        2,
    );

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "the page got no clipboard text"
        );
        match page.try_recv() {
            Ok(message) => {
                if let Some((_, text)) = clipboard_text(&message) {
                    assert_eq!(text, "texto ñ");
                    break;
                }
            }
            Err(_) => thread::sleep(Duration::from_millis(5)),
        }
    }
}

#[test]
fn the_spanish_layout_has_four_symbols_and_altgr() {
    let (path, display) = start_server_with_options(
        "layout-es",
        x11::Options {
            layout: x11::Layout::Es,
            ..x11::Options::default()
        },
    );
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;

    // evdev 39 is the key at the position of the semicolon: n with tilde in Spanish.
    let map = conn.get_keyboard_mapping(47, 1).unwrap().reply().unwrap();
    assert_eq!(map.keysyms_per_keycode, 4);
    assert_eq!(map.keysyms, [0xF1, 0xD1, 0, 0]);
    let two = conn.get_keyboard_mapping(11, 1).unwrap().reply().unwrap();
    assert_eq!(
        two.keysyms,
        [0x32, 0x22, 0x40, 0],
        "2, double quote, at sign"
    );

    let modifiers = conn.get_modifier_mapping().unwrap().reply().unwrap();
    let per = usize::from(modifiers.keycodes_per_modifier());
    assert_eq!(modifiers.keycodes[7 * per], 108, "AltGr is Mod5");

    key(&display, 108, true);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let mask = conn
            .query_pointer(root)
            .unwrap()
            .reply()
            .unwrap()
            .mask
            .bits();
        if mask & 0x80 != 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "holding AltGr must set Mod5"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn only_the_owner_of_the_clipboard_can_send_text_to_the_page() {
    let (path, display) = start_server_with_display("clipboard-owner");
    let mut page = display.subscribe();
    let owner = connect(&path);
    let stranger = connect(&path);
    let root = owner.setup().roots[0].root;
    let window = input_window(&owner, root, (0, 0, 20), EventMask::NO_EVENT);
    let clipboard = atom(&owner, b"CLIPBOARD");
    let utf8 = atom(&owner, b"UTF8_STRING");
    let transfer = atom(&owner, b"XSRV_CLIPBOARD");
    owner
        .set_selection_owner(window, clipboard, x11rb::CURRENT_TIME)
        .unwrap();
    wait_for_event(&owner, |e| matches!(e, Event::SelectionRequest(_)));

    // A client that does not own CLIPBOARD writes the property and sends the answer.
    stranger
        .change_property8(PropMode::REPLACE, 0x30, transfer, utf8, b"intruso")
        .unwrap();
    let answer = x11rb::protocol::xproto::SelectionNotifyEvent {
        response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: 0,
        requestor: 0x30,
        selection: clipboard,
        target: utf8,
        property: transfer,
    };
    stranger
        .send_event(false, 0x30_u32, EventMask::NO_EVENT, answer)
        .unwrap();
    sync(&stranger);
    sync(&owner);
    thread::sleep(Duration::from_millis(100));
    while let Ok(message) = page.try_recv() {
        assert!(
            clipboard_text(&message).is_none(),
            "text from a client that is not the owner reached the page"
        );
    }
}

#[test]
fn a_maximized_window_fills_the_screen_and_follows_a_resize() {
    let (path, display) = start_server_with_options(
        "maximize",
        x11::Options {
            window_manager: true,
            maximize: true,
            ..x11::Options::default()
        },
    );
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(&conn, root, (0, 0, 100), EventMask::STRUCTURE_NOTIFY);
    sync(&conn);
    let size = |conn: &RustConnection<DefaultStream>| {
        let g = conn.get_geometry(window).unwrap().reply().unwrap();
        (g.width, g.height)
    };
    let (screen_width, screen_height) = display.size();
    let (width, height) = size(&conn);
    assert!(
        width > screen_width - 12 && height > screen_height - 40,
        "the window must fill the screen, not {width}x{height}"
    );

    display.send_input(Input::Resize {
        width: 900,
        height: 700,
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while size(&conn).0 < 880 {
        assert!(
            std::time::Instant::now() < deadline,
            "the window did not follow the screen"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let (width, height) = size(&conn);
    assert!(
        width > 880 && height > 650 && height < 700,
        "{width}x{height}"
    );
    wait_for_event(
        &conn,
        |e| matches!(e, Event::ConfigureNotify(c) if c.width > 880),
    );

    let dialog = conn.generate_id().unwrap();
    conn.create_window(
        24,
        dialog,
        root,
        0,
        0,
        120,
        80,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    let transient_for = 68_u32; // WM_TRANSIENT_FOR
    conn.change_property32(
        PropMode::REPLACE,
        dialog,
        transient_for,
        AtomEnum::WINDOW,
        &[window],
    )
    .unwrap();
    conn.map_window(dialog).unwrap();
    let g = conn.get_geometry(dialog).unwrap().reply().unwrap();
    assert_eq!((g.width, g.height), (120, 80), "a dialog keeps its size");
}

/// An answer about a selection that nobody owns carries the time of the request as the client wrote
/// it. CurrentTime stays 0: Xt matches the answer to its request by the time, and the Common Desktop
/// Environment waited for ever when it got a real time instead.
#[test]
fn convert_selection_answers_with_the_time_of_the_request() {
    let (path, _display) = start_server_with_display("selection-time");
    let conn = connect(&path);
    let (window, _gc) = mapped_window(&conn, 0, 0, 10);
    let selection = conn
        .intern_atom(false, b"NOBODY_OWNS_THIS")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let target = conn
        .intern_atom(false, b"STRING")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    for time in [0u32, 1234] {
        conn.convert_selection(window, selection, target, 0u32, time)
            .unwrap();
        conn.flush().unwrap();
        let Event::SelectionNotify(notify) =
            wait_for_event(&conn, |e| matches!(e, Event::SelectionNotify(_)))
        else {
            unreachable!()
        };
        assert_eq!(
            (notify.time, notify.property, notify.selection),
            (time, 0, selection)
        );
    }
}

/// dtsession of CDE sets the screen saver and installs a colormap at its start; both got BadRequest.
#[test]
fn screen_saver_and_colormap_requests_work() {
    use x11rb::protocol::xproto::{Blanking, Exposures};
    let (path, _display) = start_server_with_display("screen-saver");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let colormap = conn.setup().roots[0].default_colormap;

    let before = conn.get_screen_saver().unwrap().reply().unwrap();
    assert_eq!((before.timeout, before.interval), (600, 600));
    conn.set_screen_saver(300, 120, Blanking::NOT_PREFERRED, Exposures::NOT_ALLOWED)
        .unwrap()
        .check()
        .unwrap();
    let after = conn.get_screen_saver().unwrap().reply().unwrap();
    assert_eq!((after.timeout, after.interval), (300, 120));
    assert_eq!(after.prefer_blanking, Blanking::NOT_PREFERRED);
    assert_eq!(after.allow_exposures, Exposures::NOT_ALLOWED);
    conn.set_screen_saver(-1, -1, Blanking::DEFAULT, Exposures::DEFAULT)
        .unwrap()
        .check()
        .unwrap();
    let reset = conn.get_screen_saver().unwrap().reply().unwrap();
    assert_eq!((reset.timeout, reset.interval), (600, 600));
    let bad = conn
        .set_screen_saver(-5, 0, Blanking::DEFAULT, Exposures::DEFAULT)
        .unwrap()
        .check();
    assert_eq!(error_kind(bad), ErrorKind::Value);

    conn.install_colormap(colormap).unwrap().check().unwrap();
    conn.uninstall_colormap(colormap).unwrap().check().unwrap();
    let installed = conn
        .list_installed_colormaps(root)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(installed.cmaps, [colormap]);
    let other = conn.install_colormap(0x7777_7777).unwrap().check();
    assert_eq!(error_kind(other), ErrorKind::Colormap);
}

/// Xpm (the icons of CDE) and cairo draw through a clip mask that is a bitmap. The server refused it
/// with BadImplementation, so the icons were not drawn.
#[test]
fn a_clip_mask_bitmap_limits_the_drawing() {
    let (path, display) = start_server_with_display("clip-mask");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let (window, _gc) = mapped_window(&conn, 100, 100, 40);

    // A bitmap of 8x4 with two blocks set: columns 1 and 2 of rows 0 and 1, and column 6 of row 3.
    let bitmap = conn.generate_id().unwrap();
    conn.create_pixmap(1, bitmap, root, 8, 4).unwrap();
    let ink = conn.generate_id().unwrap();
    conn.create_gc(ink, bitmap, &CreateGCAux::new().foreground(1).background(0))
        .unwrap();
    let rect = |x, y, width, height| Rectangle {
        x,
        y,
        width,
        height,
    };
    conn.poly_fill_rectangle(bitmap, ink, &[rect(1, 0, 2, 2), rect(6, 3, 1, 1)])
        .unwrap();

    let gc = conn.generate_id().unwrap();
    conn.create_gc(
        gc,
        window,
        &CreateGCAux::new()
            .foreground(0x00FF_0000)
            .clip_mask(bitmap)
            .clip_x_origin(10)
            .clip_y_origin(20),
    )
    .unwrap()
    .check()
    .expect("a bitmap is a clip mask");
    conn.poly_fill_rectangle(window, gc, &[rect(0, 0, 40, 40)])
        .unwrap();
    sync(&conn);
    // The screen pixels are in blue, green, red order.
    let red = [0, 0, 0xFF];
    let at = |x: usize, y: usize| screen_pixel(&display, 100 + x, 100 + y);
    // The mask is moved by the clip origin (10, 20).
    assert_eq!((at(11, 20), at(12, 21), at(16, 23)), (red, red, red));
    assert_ne!(at(10, 20), red, "column 0 is not set");
    assert_ne!(at(13, 20), red, "column 3 is not set");
    assert_ne!(at(11, 22), red, "row 2 is not set");
    assert_ne!(at(5, 5), red, "outside the mask nothing is drawn");

    // A new origin moves the same mask.
    conn.change_gc(gc, &ChangeGCAux::new().clip_x_origin(0).clip_y_origin(0))
        .unwrap();
    conn.poly_fill_rectangle(window, gc, &[rect(0, 0, 40, 40)])
        .unwrap();
    sync(&conn);
    assert_eq!((at(1, 0), at(2, 1), at(6, 3)), (red, red, red));

    // Clearing the mask draws everywhere again, and a mask that is not a bitmap is refused.
    conn.change_gc(gc, &ChangeGCAux::new().clip_mask(0u32))
        .unwrap();
    conn.poly_fill_rectangle(window, gc, &[rect(30, 30, 5, 5)])
        .unwrap();
    sync(&conn);
    assert_eq!(at(32, 32), red);
    let color = conn.generate_id().unwrap();
    conn.create_pixmap(24, color, root, 4, 4).unwrap();
    let refused = conn
        .change_gc(gc, &ChangeGCAux::new().clip_mask(color))
        .unwrap()
        .check();
    assert_eq!(error_kind(refused), ErrorKind::Match);
}

/// `xset fp+` and `xprop -root` use these three requests; the Common Desktop Environment runs both
/// at the start of a session and stops when they fail.
#[test]
fn font_path_and_property_lists_work() {
    use x11rb::protocol::xproto::Str;
    let (path, _display) = start_server_with_display("font-path");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;

    let before = conn.get_font_path().unwrap().reply().unwrap();
    assert!(!before.path.is_empty(), "a font path is always given");
    let name = |text: &str| Str {
        name: text.as_bytes().to_vec(),
    };
    let wanted = [name("/usr/dt/config/xfonts"), name("built-ins")];
    conn.set_font_path(&wanted).unwrap().check().unwrap();
    let after = conn.get_font_path().unwrap().reply().unwrap();
    let names = |list: &[Str]| list.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&after.path), names(&wanted));
    let too_many: Vec<Str> = (0..65).map(|i| name(&format!("/font/{i}"))).collect();
    let refused = conn.set_font_path(&too_many).unwrap().check();
    assert_eq!(error_kind(refused), ErrorKind::Value);

    let one = conn
        .intern_atom(false, b"XSRV_TEST_ONE")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let two = conn
        .intern_atom(false, b"XSRV_TEST_TWO")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let empty = conn.list_properties(root).unwrap().reply().unwrap();
    assert!(!empty.atoms.contains(&one));
    for atom in [two, one] {
        conn.change_property8(PropMode::REPLACE, root, atom, AtomEnum::STRING, b"x")
            .unwrap();
    }
    let listed = conn.list_properties(root).unwrap().reply().unwrap();
    assert!(listed.atoms.contains(&one) && listed.atoms.contains(&two));
    let mut sorted = listed.atoms.clone();
    sorted.sort_unstable();
    assert_eq!(listed.atoms, sorted, "the order is fixed");
    let bad = conn.list_properties(0x7777_7777).unwrap().reply();
    assert!(matches!(bad, Err(ReplyError::X11Error(e)) if e.error_kind == ErrorKind::Window));
}

/// The Common Desktop Environment asks for XINERAMA at the start of its session manager, and waits
/// for the answer. The screens are the virtual monitors of RANDR.
#[test]
fn xinerama_lists_the_virtual_monitors() {
    use x11rb::protocol::xinerama::ConnectionExt as _;
    let (path, _display) = start_server_with_options(
        "xinerama",
        x11::Options {
            monitors: 2,
            ..x11::Options::default()
        },
    );
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let version = conn.xinerama_query_version(1, 1).unwrap().reply().unwrap();
    assert_eq!((version.major, version.minor), (1, 1));
    assert_eq!(conn.xinerama_is_active().unwrap().reply().unwrap().state, 1);
    let state = conn.xinerama_get_state(root).unwrap().reply().unwrap();
    assert_eq!((state.state, state.window), (1, root));
    let count = conn
        .xinerama_get_screen_count(root)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!((count.screen_count, count.window), (2, root));
    let screens = conn.xinerama_query_screens().unwrap().reply().unwrap();
    let boxes: Vec<_> = screens
        .screen_info
        .iter()
        .map(|s| (s.x_org, s.y_org, s.width, s.height))
        .collect();
    assert_eq!(boxes, [(0, 0, 320, 480), (320, 0, 320, 480)]);
    let size = conn
        .xinerama_get_screen_size(root, 1)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!((size.width, size.height, size.screen), (320, 480, 1));
    let too_far = conn.xinerama_get_screen_size(root, 2).unwrap().reply();
    assert!(matches!(too_far, Err(ReplyError::X11Error(e)) if e.error_kind == ErrorKind::Value));
}

#[test]
fn randr_reports_virtual_monitors_side_by_side() {
    use x11rb::protocol::randr::{ConnectionExt as _, Rotation};
    let (path, display) = start_server_with_options(
        "monitors",
        x11::Options {
            monitors: 2,
            ..x11::Options::default()
        },
    );
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let resources = conn
        .randr_get_screen_resources(root)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(
        (
            resources.crtcs.len(),
            resources.outputs.len(),
            resources.modes.len()
        ),
        (2, 2, 2)
    );
    assert_eq!(resources.names, b"320x480320x480");
    let geometry = |crtc| {
        let info = conn
            .randr_get_crtc_info(crtc, resources.config_timestamp)
            .unwrap()
            .reply()
            .unwrap();
        (info.x, info.y, info.width, info.height)
    };
    assert_eq!(geometry(resources.crtcs[0]), (0, 0, 320, 480));
    assert_eq!(geometry(resources.crtcs[1]), (320, 0, 320, 480));
    let output = conn
        .randr_get_output_info(resources.outputs[1], resources.config_timestamp)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(
        (output.crtc, output.name.as_slice()),
        (resources.crtcs[1], &b"xsrv-1"[..])
    );

    let gamma = conn
        .randr_get_crtc_gamma(resources.crtcs[0])
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(
        (gamma.red.len(), gamma.red[255]),
        (256, 65535),
        "an identity table"
    );
    let keep = conn
        .randr_set_crtc_config(
            resources.crtcs[1],
            0,
            resources.config_timestamp,
            320,
            0,
            resources.modes[1].id,
            Rotation::ROTATE0,
            &[resources.outputs[1]],
        )
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(
        u8::from(keep.status),
        0,
        "the current configuration is accepted"
    );
    let moved = conn
        .randr_set_crtc_config(
            resources.crtcs[1],
            0,
            resources.config_timestamp,
            0,
            0,
            resources.modes[1].id,
            Rotation::ROTATE0,
            &[resources.outputs[1]],
        )
        .unwrap()
        .reply()
        .unwrap();
    assert_ne!(u8::from(moved.status), 0, "moving a monitor is refused");

    display.send_input(Input::Resize {
        width: 801,
        height: 600,
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let again = conn
            .randr_get_screen_resources(root)
            .unwrap()
            .reply()
            .unwrap();
        let last = conn
            .randr_get_crtc_info(again.crtcs[1], again.config_timestamp)
            .unwrap()
            .reply()
            .unwrap();
        if last.width == 401 {
            assert_eq!(
                (last.x, last.height),
                (400, 600),
                "the last monitor takes the rest"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the monitors did not follow the screen"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_big_endian_client_gets_big_endian_replies() {
    let path = start_server("big-endian");
    let mut stream = UnixStream::connect(&path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    // Byte order 'B', protocol 11.0, no authorization.
    stream
        .write_all(&[b'B', 0, 0, 11, 0, 0, 0, 0, 0, 0, 0, 0])
        .unwrap();
    let mut head = [0u8; 8];
    stream.read_exact(&mut head).unwrap();
    assert_eq!(head[0], 1, "the setup was accepted");
    let extra = usize::from(u16::from_be_bytes([head[6], head[7]])) * 4;
    let mut body = vec![0u8; extra];
    stream.read_exact(&mut body).unwrap();

    // GetInputFocus (opcode 43), then InternAtom "PRIMARY" with a big-endian name length.
    stream.write_all(&[43, 0, 0, 1]).unwrap();
    let mut intern = vec![16, 0, 0, 4, 0, 7, 0, 0];
    intern.extend_from_slice(b"PRIMARY\0");
    stream.write_all(&intern).unwrap();
    let mut focus = [0u8; 32];
    stream.read_exact(&mut focus).unwrap();
    assert_eq!(focus[0], 1, "a reply");
    assert_eq!(
        u16::from_be_bytes([focus[2], focus[3]]),
        1,
        "sequence 1, big-endian"
    );
    let mut atom = [0u8; 32];
    stream.read_exact(&mut atom).unwrap();
    assert_eq!(u16::from_be_bytes([atom[2], atom[3]]), 2);
    assert_eq!(
        u32::from_be_bytes([atom[8], atom[9], atom[10], atom[11]]),
        1,
        "PRIMARY is atom 1"
    );
}

#[test]
fn the_focus_goes_to_the_next_window_when_the_focused_one_is_destroyed() {
    let (path, _display) = start_server_wm("focus-next");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let first = input_window(&conn, root, (50, 50, 100), EventMask::NO_EVENT);
    let second = input_window(&conn, root, (200, 50, 100), EventMask::NO_EVENT);
    sync(&conn);
    let focus = |conn: &RustConnection<DefaultStream>| {
        conn.get_input_focus().unwrap().reply().unwrap().focus
    };
    assert_eq!(focus(&conn), second, "a new window gets the focus");
    conn.destroy_window(second).unwrap();
    sync(&conn);
    assert_eq!(focus(&conn), first, "the window below gets it back");
    conn.destroy_window(first).unwrap();
    sync(&conn);
    assert_ne!(focus(&conn), first);
}

#[test]
fn translate_coordinates_names_the_child_of_the_target() {
    let conn = connect(&start_server("translate-child"));
    let root = conn.setup().roots[0].root;
    let outer = input_window(&conn, root, (100, 100, 100), EventMask::NO_EVENT);
    let inner = input_window(&conn, outer, (10, 10, 20), EventMask::NO_EVENT);
    sync(&conn);
    let at = |target: u32, x: i16, y: i16| {
        let reply = conn
            .translate_coordinates(root, target, x, y)
            .unwrap()
            .reply()
            .unwrap();
        (reply.child, reply.dst_x, reply.dst_y)
    };
    assert_eq!(
        at(outer, 115, 115),
        (inner, 15, 15),
        "the child of the outer window"
    );
    assert_eq!(
        at(root, 115, 115),
        (outer, 115, 115),
        "from the root, the top-level window"
    );
    assert_eq!(
        at(inner, 115, 115),
        (0, 5, 5),
        "no child below the inner window"
    );
    assert_eq!(
        at(outer, 50, 50),
        (0, -50, -50),
        "the point is outside every child"
    );
}

#[test]
fn crossing_events_name_the_child_on_the_way_to_the_inferior() {
    let (path, display) = start_server_with_display("crossing-child");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let mask = EventMask::ENTER_WINDOW | EventMask::LEAVE_WINDOW;
    let outer = input_window(&conn, root, (100, 100, 100), mask);
    let inner = input_window(&conn, outer, (10, 10, 20), mask);
    sync(&conn);
    pointer_to(&display, 150, 150); // inside outer, outside inner
    sync(&conn);
    let first = wait_for_event(
        &conn,
        |e| matches!(e, Event::EnterNotify(e) if e.event == outer),
    );
    let Event::EnterNotify(entered) = first else {
        unreachable!()
    };
    assert_eq!(entered.child, 0, "entering from the root: no child");
    drain_events(&conn);

    pointer_to(&display, 115, 115); // into inner
    let leave = wait_for_event(
        &conn,
        |e| matches!(e, Event::LeaveNotify(e) if e.event == outer),
    );
    let Event::LeaveNotify(left) = leave else {
        unreachable!()
    };
    assert_eq!(u8::from(left.detail), 2, "Inferior");
    assert_eq!(left.child, inner, "the child of outer on the way to inner");
    let enter = wait_for_event(
        &conn,
        |e| matches!(e, Event::EnterNotify(e) if e.event == inner),
    );
    let Event::EnterNotify(into) = enter else {
        unreachable!()
    };
    assert_eq!(
        (u8::from(into.detail), into.child),
        (0, 0),
        "Ancestor has no child"
    );
}

#[test]
fn a_cookie_protects_the_x11_socket() {
    let cookie = [7u8; 16];
    let (path, _display) = start_server_with_options(
        "cookie",
        x11::Options {
            cookie: Some(cookie),
            ..x11::Options::default()
        },
    );
    let connect_with = |name: &[u8], data: &[u8]| {
        let stream = UnixStream::connect(&path).unwrap();
        let (stream, _) = DefaultStream::from_unix_stream(stream).unwrap();
        RustConnection::connect_to_stream_with_auth_info(stream, 0, name.to_vec(), data.to_vec())
    };
    let magic: &[u8] = b"MIT-MAGIC-COOKIE-1";
    assert!(connect_with(b"", b"").is_err(), "no cookie is refused");
    assert!(
        connect_with(magic, &[8u8; 16]).is_err(),
        "a wrong cookie is refused"
    );
    assert!(
        connect_with(b"OTHER", &cookie).is_err(),
        "another protocol is refused"
    );
    assert!(
        connect_with(magic, &cookie[..15]).is_err(),
        "a short cookie is refused"
    );
    let conn = connect_with(magic, &cookie).expect("the right cookie is accepted");
    assert!(conn.get_input_focus().unwrap().reply().is_ok());
}

#[test]
fn focus_events_between_windows_have_the_details_of_the_protocol() {
    let path = start_server("focus-details");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let outer = input_window(&conn, root, (100, 100, 100), EventMask::FOCUS_CHANGE);
    let inner = input_window(&conn, outer, (10, 10, 20), EventMask::FOCUS_CHANGE);
    let other = input_window(&conn, root, (300, 100, 50), EventMask::FOCUS_CHANGE);
    sync(&conn);
    let kind = |event: Event| match event {
        Event::FocusIn(e) => Some(("in", e.event, u8::from(e.detail))),
        Event::FocusOut(e) => Some(("out", e.event, u8::from(e.detail))),
        _ => None,
    };
    let next_focus = |conn: &RustConnection<DefaultStream>| loop {
        if let Some(found) = kind(next_event(conn)) {
            return found;
        }
    };
    let set = |window: u32| {
        conn.set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)
            .unwrap();
        conn.flush().unwrap();
    };
    set(outer);
    drain_events(&conn);

    set(inner); // into a window below the focus
    assert_eq!(next_focus(&conn), ("out", outer, 2), "Inferior");
    assert_eq!(next_focus(&conn), ("in", inner, 0), "Ancestor");
    set(outer); // back to the window above
    assert_eq!(next_focus(&conn), ("out", inner, 0), "Ancestor");
    assert_eq!(next_focus(&conn), ("in", outer, 2), "Inferior");
    set(other); // to a window that is not related
    assert_eq!(next_focus(&conn), ("out", outer, 3), "Nonlinear");
    assert_eq!(next_focus(&conn), ("in", other, 3), "Nonlinear");
}

#[test]
fn get_image_on_a_window_includes_its_children() {
    let (path, _display) = start_server_with_display("getimage-children");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let outer = conn.generate_id().unwrap();
    conn.create_window(
        24,
        outer,
        root,
        100,
        100,
        50,
        50,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new().background_pixel(0x00FF00),
    )
    .unwrap();
    let inner = conn.generate_id().unwrap();
    conn.create_window(
        24,
        inner,
        outer,
        10,
        10,
        20,
        20,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new().background_pixel(0xFF0000),
    )
    .unwrap();
    conn.map_window(outer).unwrap();
    conn.map_window(inner).unwrap();
    sync(&conn);
    let image = |window: u32, x: i16, y: i16| {
        let reply = conn
            .get_image(ImageFormat::Z_PIXMAP, window, x, y, 1, 1, u32::MAX)
            .unwrap()
            .reply()
            .unwrap();
        [reply.data[2], reply.data[1], reply.data[0]]
    };
    assert_eq!(
        image(outer, 15, 15),
        [0xFF, 0, 0],
        "the child is part of the outer window image"
    );
    assert_eq!(
        image(outer, 2, 2),
        [0, 0xFF, 0],
        "the outer window's own pixels"
    );
    assert_eq!(
        image(root, 115, 115),
        [0xFF, 0, 0],
        "the root shows the whole screen"
    );
    assert_eq!(image(root, 102, 102), [0, 0xFF, 0]);
}

#[test]
fn one_client_cannot_use_all_the_passive_key_grabs() {
    let path = start_server("grab-limit");
    let greedy = connect(&path);
    let other = connect(&path);
    let root = greedy.setup().roots[0].root;
    let window = input_window(&greedy, root, (0, 0, 50), EventMask::NO_EVENT);
    let other_window = input_window(&other, root, (100, 0, 50), EventMask::NO_EVENT);
    sync(&greedy);
    sync(&other);
    fn grab(
        conn: &RustConnection<DefaultStream>,
        window: u32,
        key: u8,
        modifiers: u16,
    ) -> x11rb::cookie::VoidCookie<'_, RustConnection<DefaultStream>> {
        conn.grab_key(
            true,
            window,
            ModMask::from(modifiers),
            key,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .unwrap()
    }
    // 2048 grabs: 8 keys with each of the 256 modifier masks. The checks run once, at the end.
    let cookies: Vec<_> = (8..16)
        .flat_map(|key| (0..256).map(move |modifiers| (key, modifiers)))
        .map(|(key, modifiers)| grab(&greedy, window, key, modifiers))
        .collect();
    for cookie in cookies {
        cookie.check().unwrap();
    }
    let refused = grab(&greedy, window, 16, 0).check();
    assert_eq!(
        error_kind(refused),
        ErrorKind::Alloc,
        "the 2049th grab of one client"
    );
    grab(&other, other_window, 100, 0)
        .check()
        .expect("another client still gets its grab");
}

#[test]
fn two_small_changes_far_apart_are_sent_as_two_small_tiles() {
    let (path, display) = start_server_with_display("dirty-list");
    let mut page = display.subscribe();
    let conn = connect(&path);
    let (window, gc) = mapped_window(&conn, 100, 100, 20);
    sync(&conn);
    let (far_window, far_gc) = mapped_window(&conn, 500, 400, 20);
    sync(&conn);
    while page.try_recv().is_ok() {}
    for (target, context) in [(window, gc), (far_window, far_gc)] {
        conn.poly_fill_rectangle(
            target,
            context,
            &[Rectangle {
                x: 0,
                y: 0,
                width: 5,
                height: 5,
            }],
        )
        .unwrap();
    }
    sync(&conn);
    let mut tile_pixels = Vec::new();
    thread::sleep(Duration::from_millis(50));
    while let Ok(message) = page.try_recv() {
        if let Some((rect, _)) = tile(&message) {
            tile_pixels.push(u32::from(rect.w) * u32::from(rect.h));
        }
    }
    assert!(
        tile_pixels.len() >= 2,
        "two areas, two tiles: {tile_pixels:?}"
    );
    assert!(
        tile_pixels.iter().all(|pixels| *pixels < 2000),
        "no tile covers the gap between the two areas: {tile_pixels:?}"
    );
}

#[test]
fn the_parent_gets_enter_notify_when_the_window_under_the_pointer_is_destroyed() {
    let (path, display) = start_server_with_display("enter-on-destroy");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let outer = input_window(&conn, root, (100, 100, 100), EventMask::ENTER_WINDOW);
    let inner = input_window(&conn, outer, (10, 10, 20), EventMask::NO_EVENT);
    sync(&conn);
    pointer_to(&display, 115, 115);
    sync(&conn);
    thread::sleep(Duration::from_millis(50));
    sync(&conn);
    drain_events(&conn);
    conn.destroy_window(inner).unwrap();
    conn.flush().unwrap();
    let Event::EnterNotify(entered) = wait_for_event(
        &conn,
        |e| matches!(e, Event::EnterNotify(e) if e.event == outer),
    ) else {
        unreachable!()
    };
    assert_eq!(
        u8::from(entered.detail),
        2,
        "Inferior: the pointer came up from a child"
    );
}

#[test]
fn focus_events_for_none_and_pointer_root_use_the_root_window() {
    let path = start_server("focus-special");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let outer = input_window(&conn, root, (100, 100, 100), EventMask::FOCUS_CHANGE);
    conn.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::FOCUS_CHANGE),
    )
    .unwrap();
    sync(&conn);
    let next = |conn: &RustConnection<DefaultStream>| loop {
        match next_event(conn) {
            Event::FocusIn(e) => return ("in", e.event, u8::from(e.detail)),
            Event::FocusOut(e) => return ("out", e.event, u8::from(e.detail)),
            _ => {}
        }
    };
    let set = |window: u32| {
        conn.set_input_focus(InputFocus::NONE, window, x11rb::CURRENT_TIME)
            .unwrap();
        conn.flush().unwrap();
    };
    set(outer);
    drain_events(&conn);

    set(0); // None
    assert_eq!(next(&conn), ("out", outer, 3), "Nonlinear");
    assert_eq!(
        next(&conn),
        ("out", root, 4),
        "NonlinearVirtual up to the root"
    );
    assert_eq!(next(&conn), ("in", root, 7), "NotifyDetailNone");
    set(1); // PointerRoot
    assert_eq!(next(&conn), ("out", root, 7));
    assert_eq!(next(&conn), ("in", root, 6), "NotifyPointerRoot");
    set(outer);
    assert_eq!(next(&conn), ("out", root, 6));
    assert_eq!(next(&conn), ("in", outer, 3), "Nonlinear");
}

#[test]
fn a_pointer_grab_sends_crossing_events_with_the_modes_grab_and_ungrab() {
    let (path, display) = start_server_with_display("grab-crossing");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let mask = EventMask::ENTER_WINDOW | EventMask::LEAVE_WINDOW;
    let first = input_window(&conn, root, (100, 100, 100), mask);
    let second = input_window(&conn, root, (300, 100, 100), mask);
    sync(&conn);
    pointer_to(&display, 150, 150);
    sync(&conn);
    thread::sleep(Duration::from_millis(50));
    sync(&conn);
    drain_events(&conn);

    let crossing = |event: Event| match event {
        Event::EnterNotify(e) => Some(("enter", e.event, u8::from(e.mode))),
        Event::LeaveNotify(e) => Some(("leave", e.event, u8::from(e.mode))),
        _ => None,
    };
    let next = |conn: &RustConnection<DefaultStream>| loop {
        if let Some(found) = crossing(next_event(conn)) {
            return found;
        }
    };
    let status = conn
        .grab_pointer(
            true,
            second,
            EventMask::NO_EVENT,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            0u32,
            0u32,
            x11rb::CURRENT_TIME,
        )
        .unwrap()
        .reply()
        .unwrap()
        .status;
    assert_eq!(u8::from(status), 0, "the grab succeeded");
    assert_eq!(next(&conn), ("leave", first, 1), "mode Grab");
    assert_eq!(next(&conn), ("enter", second, 1), "mode Grab");
    conn.ungrab_pointer(x11rb::CURRENT_TIME).unwrap();
    conn.flush().unwrap();
    assert_eq!(next(&conn), ("leave", second, 2), "mode Ungrab");
    assert_eq!(next(&conn), ("enter", first, 2), "mode Ungrab");
}

#[test]
fn a_pointer_grab_reports_crossings_to_the_grabbing_client_only() {
    let (path, display) = start_server_with_display("grab-crossing-normal");
    let grabber = connect(&path);
    let bystander = connect(&path);
    let root = grabber.setup().roots[0].root;
    let mask = EventMask::ENTER_WINDOW | EventMask::LEAVE_WINDOW;
    let first = input_window(&grabber, root, (100, 100, 100), mask);
    let second = input_window(&grabber, root, (300, 100, 100), mask);
    bystander
        .change_window_attributes(second, &ChangeWindowAttributesAux::new().event_mask(mask))
        .unwrap();
    sync(&grabber);
    sync(&bystander);
    pointer_to(&display, 150, 150);
    sync(&grabber);
    thread::sleep(Duration::from_millis(50));
    sync(&grabber);
    sync(&bystander);
    drain_events(&grabber);
    drain_events(&bystander);

    let status = grabber
        .grab_pointer(
            true,
            first,
            EventMask::NO_EVENT,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            0u32,
            0u32,
            x11rb::CURRENT_TIME,
        )
        .unwrap()
        .reply()
        .unwrap()
        .status;
    assert_eq!(u8::from(status), 0, "the grab succeeded");
    pointer_to(&display, 350, 150);
    sync(&grabber);
    thread::sleep(Duration::from_millis(50));
    sync(&grabber);
    sync(&bystander);

    let crossings: Vec<_> = drain_events(&grabber)
        .into_iter()
        .filter_map(|event| match event {
            Event::LeaveNotify(e) => Some(("leave", e.event, u8::from(e.mode))),
            Event::EnterNotify(e) => Some(("enter", e.event, u8::from(e.mode))),
            _ => None,
        })
        .collect();
    assert_eq!(
        crossings,
        [("leave", first, 0), ("enter", second, 0)],
        "the grabbing client sees the pointer cross windows"
    );
    let others = drain_events(&bystander)
        .into_iter()
        .filter(|event| matches!(event, Event::EnterNotify(_) | Event::LeaveNotify(_)))
        .count();
    assert_eq!(
        others, 0,
        "another client gets no crossing event during the grab"
    );
}

#[test]
fn change_active_pointer_grab_changes_the_event_mask_of_the_grab() {
    let (path, display) = start_server_with_display("change-grab");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(&conn, root, (100, 100, 200), EventMask::NO_EVENT);
    sync(&conn);
    pointer_to(&display, 150, 150);
    sync(&conn);
    conn.grab_pointer(
        false,
        window,
        EventMask::BUTTON_PRESS,
        GrabMode::ASYNC,
        GrabMode::ASYNC,
        0u32,
        0u32,
        x11rb::CURRENT_TIME,
    )
    .unwrap()
    .reply()
    .unwrap();
    drain_events(&conn);
    let motions = |conn: &RustConnection<DefaultStream>| {
        sync(conn);
        thread::sleep(Duration::from_millis(50));
        sync(conn);
        drain_events(conn)
            .iter()
            .filter(|event| matches!(event, Event::MotionNotify(_)))
            .count()
    };
    pointer_to(&display, 160, 160);
    assert_eq!(motions(&conn), 0, "the mask of the grab has no motion");

    conn.change_active_pointer_grab(0u32, x11rb::CURRENT_TIME, EventMask::POINTER_MOTION)
        .unwrap()
        .check()
        .unwrap();
    pointer_to(&display, 170, 170);
    assert_eq!(motions(&conn), 1, "the new mask has motion");

    let other = connect(&path);
    other
        .change_active_pointer_grab(0u32, x11rb::CURRENT_TIME, EventMask::NO_EVENT)
        .unwrap()
        .check()
        .unwrap();
    pointer_to(&display, 180, 180);
    assert_eq!(
        motions(&conn),
        1,
        "a client without the grab cannot change it"
    );
}

#[test]
fn a_keyboard_grab_sends_focus_events_with_the_modes_grab_and_ungrab() {
    let path = start_server("grab-focus");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let first = input_window(&conn, root, (100, 100, 100), EventMask::FOCUS_CHANGE);
    let second = input_window(&conn, root, (300, 100, 100), EventMask::FOCUS_CHANGE);
    sync(&conn);
    conn.set_input_focus(InputFocus::NONE, first, x11rb::CURRENT_TIME)
        .unwrap();
    sync(&conn);
    drain_events(&conn);
    let next = |conn: &RustConnection<DefaultStream>| loop {
        match next_event(conn) {
            Event::FocusIn(e) => return ("in", e.event, u8::from(e.mode)),
            Event::FocusOut(e) => return ("out", e.event, u8::from(e.mode)),
            _ => {}
        }
    };
    let status = conn
        .grab_keyboard(
            true,
            second,
            x11rb::CURRENT_TIME,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
        )
        .unwrap()
        .reply()
        .unwrap()
        .status;
    assert_eq!(u8::from(status), 0);
    assert_eq!(next(&conn), ("out", first, 1), "mode Grab");
    assert_eq!(next(&conn), ("in", second, 1), "mode Grab");
    conn.ungrab_keyboard(x11rb::CURRENT_TIME).unwrap();
    conn.flush().unwrap();
    assert_eq!(next(&conn), ("out", second, 2), "mode Ungrab");
    assert_eq!(next(&conn), ("in", first, 2), "mode Ungrab");
}

#[test]
fn a_clip_with_far_apart_rectangles_sends_small_tiles() {
    let (path, display) = start_server_with_display("clip-tiles");
    let mut page = display.subscribe();
    let conn = connect(&path);
    let (window, gc) = mapped_window(&conn, 100, 100, 20);
    sync(&conn);
    // A big window, so the clip rectangles can be far apart.
    let big = conn.generate_id().unwrap();
    conn.create_window(
        24,
        big,
        window,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    sync(&conn);
    conn.configure_window(window, &ConfigureWindowAux::new().width(400).height(300))
        .unwrap();
    conn.set_clip_rectangles(
        ClipOrdering::UNSORTED,
        gc,
        0,
        0,
        &[
            Rectangle {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
            Rectangle {
                x: 380,
                y: 280,
                width: 10,
                height: 10,
            },
        ],
    )
    .unwrap();
    sync(&conn);
    while page.try_recv().is_ok() {}
    conn.poly_fill_rectangle(
        window,
        gc,
        &[Rectangle {
            x: 0,
            y: 0,
            width: 400,
            height: 300,
        }],
    )
    .unwrap();
    sync(&conn);
    thread::sleep(Duration::from_millis(50));
    let mut sizes = Vec::new();
    while let Ok(message) = page.try_recv() {
        if let Some((rect, _)) = tile(&message) {
            sizes.push(u32::from(rect.w) * u32::from(rect.h));
        }
    }
    assert!(!sizes.is_empty(), "something was sent");
    assert!(
        sizes.iter().all(|pixels| *pixels <= 400),
        "the two 10x10 clip rectangles are sent alone: {sizes:?}"
    );
}

#[test]
fn replay_pointer_gives_the_click_to_the_window_below_the_grab() {
    let (path, display) = start_server_with_display("replay-pointer");
    let manager = connect(&path);
    let app = connect(&path);
    let root = app.setup().roots[0].root;
    let window = input_window(&app, root, (100, 100, 100), EventMask::BUTTON_PRESS);
    sync(&app);
    // The manager takes button 1 on the window with a synchronous passive grab, as i3 does to look
    // at a click before it gives the focus.
    manager
        .grab_button(
            false,
            window,
            EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE,
            GrabMode::SYNC,
            GrabMode::ASYNC,
            0u32,
            0u32,
            x11rb::protocol::xproto::ButtonIndex::M1,
            ModMask::ANY,
        )
        .unwrap()
        .check()
        .unwrap();
    pointer_to(&display, 150, 150);
    sync(&app);
    thread::sleep(Duration::from_millis(50));
    drain_events(&app);
    button(&display, 1, true);

    let Event::ButtonPress(taken) =
        wait_for_event(&manager, |e| matches!(e, Event::ButtonPress(_)))
    else {
        unreachable!()
    };
    assert_eq!(taken.detail, 1);
    sync(&app);
    assert!(
        app.poll_for_event().unwrap().is_none(),
        "the application sees nothing while the grab holds the press"
    );

    manager
        .allow_events(
            x11rb::protocol::xproto::Allow::REPLAY_POINTER,
            x11rb::CURRENT_TIME,
        )
        .unwrap();
    manager.flush().unwrap();
    let Event::ButtonPress(replayed) = wait_for_event(&app, |e| matches!(e, Event::ButtonPress(_)))
    else {
        unreachable!()
    };
    assert_eq!((replayed.detail, replayed.event), (1, window));
    assert_eq!(
        u16::from(replayed.state) & 0x0100,
        0,
        "the replayed press does not show its own button as down"
    );
    button(&display, 1, false);
}

/// dtwm (Motif) grabs the buttons of a client window with an event mask of ButtonRelease only, and
/// waits for the press to give the focus and to call AllowEvents. Xorg sends the press that starts a
/// passive grab whatever the mask says; the server held it back, so a click never gave the focus.
#[test]
fn a_passive_grab_gets_its_press_although_its_mask_has_only_the_release() {
    let (path, display) = start_server_with_display("grab-press");
    let manager = connect(&path);
    let app = connect(&path);
    let root = app.setup().roots[0].root;
    let window = input_window(&app, root, (100, 100, 100), EventMask::BUTTON_PRESS);
    sync(&app);
    manager
        .grab_button(
            false,
            window,
            EventMask::BUTTON_RELEASE,
            GrabMode::SYNC,
            GrabMode::ASYNC,
            0u32,
            0u32,
            x11rb::protocol::xproto::ButtonIndex::M1,
            ModMask::ANY,
        )
        .unwrap()
        .check()
        .unwrap();
    pointer_to(&display, 150, 150);
    sync(&app);
    thread::sleep(Duration::from_millis(50));
    drain_events(&app);
    button(&display, 1, true);
    let Event::ButtonPress(taken) =
        wait_for_event(&manager, |e| matches!(e, Event::ButtonPress(_)))
    else {
        unreachable!()
    };
    assert_eq!((taken.detail, taken.event), (1, window));
    sync(&app);
    assert!(
        app.poll_for_event().unwrap().is_none(),
        "the application sees nothing while the grab holds the press"
    );
    manager
        .allow_events(
            x11rb::protocol::xproto::Allow::REPLAY_POINTER,
            x11rb::CURRENT_TIME,
        )
        .unwrap();
    manager.flush().unwrap();
    let Event::ButtonPress(replayed) = wait_for_event(&app, |e| matches!(e, Event::ButtonPress(_)))
    else {
        unreachable!()
    };
    assert_eq!((replayed.detail, replayed.event), (1, window));
    button(&display, 1, false);
}

#[test]
fn put_image_in_the_xy_pixmap_format_draws_planes() {
    let (path, display) = start_server_with_display("xy-pixmap");
    let conn = connect(&path);
    let (window, gc) = mapped_window(&conn, 100, 100, 20);
    sync(&conn);
    // Two pixels: 0xFF0000 and 0x0000FF. One bitmap for each of the 24 planes, the most
    // significant first. The bit of the first pixel is bit 0 of the first byte of a row.
    let pixels = [0xFF0000u32, 0x0000FF];
    let mut data = Vec::new();
    for plane in (0..24).rev() {
        let bits = pixels
            .iter()
            .enumerate()
            .fold(0u8, |acc, (x, p)| acc | ((((p >> plane) & 1) as u8) << x));
        data.extend_from_slice(&[bits, 0, 0, 0]);
    }
    conn.put_image(ImageFormat::XY_PIXMAP, window, gc, 2, 1, 3, 4, 0, 24, &data)
        .unwrap()
        .check()
        .expect("XYPixmap is accepted");
    sync(&conn);
    assert_eq!(
        screen_pixel(&display, 103, 104),
        [0, 0, 0xFF],
        "the first pixel is red"
    );
    assert_eq!(
        screen_pixel(&display, 104, 104),
        [0xFF, 0, 0],
        "the second pixel is blue"
    );

    // With one plane (a bitmap), XYPixmap works like ZPixmap.
    let bitmap = conn.generate_id().unwrap();
    conn.create_pixmap(1, bitmap, window, 8, 1).unwrap();
    let bitmap_gc = conn.generate_id().unwrap();
    conn.create_gc(bitmap_gc, bitmap, &CreateGCAux::new())
        .unwrap();
    conn.put_image(
        ImageFormat::XY_PIXMAP,
        bitmap,
        bitmap_gc,
        8,
        1,
        0,
        0,
        0,
        1,
        &[0b0000_0101, 0, 0, 0],
    )
    .unwrap()
    .check()
    .expect("a depth 1 XYPixmap is accepted");
}

fn white_pixels(conn: &RustConnection<DefaultStream>, window: u32, size: u16) -> Vec<(i16, i16)> {
    let image = conn
        .get_image(ImageFormat::Z_PIXMAP, window, 0, 0, size, size, u32::MAX)
        .unwrap()
        .reply()
        .unwrap();
    let mut found = Vec::new();
    for y in 0..size {
        for x in 0..size {
            let at = (usize::from(y) * usize::from(size) + usize::from(x)) * 4;
            if image.data[at..at + 3] == [0xFF, 0xFF, 0xFF] {
                found.push((x as i16, y as i16));
            }
        }
    }
    found
}

#[test]
fn arcs_and_polygons_are_drawn() {
    use x11rb::protocol::xproto::{Arc, CoordMode, PolyShape};
    let path = start_server("arcs");
    let conn = connect(&path);
    let (window, gc) = mapped_window(&conn, 100, 100, 60);
    conn.change_gc(gc, &ChangeGCAux::new().foreground(0xFFFFFF))
        .unwrap();
    sync(&conn);
    let clear = |conn: &RustConnection<DefaultStream>| {
        conn.clear_area(false, window, 0, 0, 0, 0).unwrap();
    };

    // A filled circle of diameter 40: about pi * 20 * 20 = 1257 pixels, centre in, corner out.
    conn.poly_fill_arc(
        window,
        gc,
        &[Arc {
            x: 10,
            y: 10,
            width: 40,
            height: 40,
            angle1: 0,
            angle2: 360 * 64,
        }],
    )
    .unwrap();
    sync(&conn);
    let disc = white_pixels(&conn, window, 60);
    assert!(
        (1200..=1320).contains(&disc.len()),
        "a disc has about 1257 pixels, not {}",
        disc.len()
    );
    assert!(disc.contains(&(30, 30)) && !disc.contains(&(10, 10)) && !disc.contains(&(49, 49)));
    clear(&conn);

    // A pie slice of the upper half: the part above the centre line only.
    conn.poly_fill_arc(
        window,
        gc,
        &[Arc {
            x: 10,
            y: 10,
            width: 40,
            height: 40,
            angle1: 0,
            angle2: 180 * 64,
        }],
    )
    .unwrap();
    sync(&conn);
    let half = white_pixels(&conn, window, 60);
    assert!(half.contains(&(30, 20)), "above the centre");
    assert!(!half.contains(&(30, 40)), "not below the centre");
    assert!(
        (560..=700).contains(&half.len()),
        "half a disc: {}",
        half.len()
    );
    clear(&conn);

    // An outline: the middle stays empty, the edge is drawn.
    conn.poly_arc(
        window,
        gc,
        &[Arc {
            x: 10,
            y: 10,
            width: 40,
            height: 40,
            angle1: 0,
            angle2: 360 * 64,
        }],
    )
    .unwrap();
    sync(&conn);
    let ring = white_pixels(&conn, window, 60);
    assert!(!ring.contains(&(30, 30)), "the circle is not filled");
    assert!(
        ring.contains(&(49, 30)) || ring.contains(&(48, 30)),
        "the right edge is drawn"
    );
    assert!(
        ring.len() > 80 && ring.len() < 200,
        "an outline of about 120 pixels: {}",
        ring.len()
    );
    clear(&conn);

    // A triangle.
    conn.fill_poly(
        window,
        gc,
        PolyShape::COMPLEX,
        CoordMode::ORIGIN,
        &[
            Point { x: 0, y: 0 },
            Point { x: 40, y: 0 },
            Point { x: 0, y: 40 },
        ],
    )
    .unwrap();
    sync(&conn);
    let triangle = white_pixels(&conn, window, 60);
    assert!(triangle.contains(&(5, 5)) && triangle.contains(&(30, 5)));
    assert!(!triangle.contains(&(35, 35)) && !triangle.contains(&(5, 50)));
    assert!(
        (700..=900).contains(&triangle.len()),
        "half of 40x40 is 800: {}",
        triangle.len()
    );
}

#[test]
fn a_click_released_before_the_replay_reaches_the_application_in_order() {
    let (path, display) = start_server_with_display("replay-fast-click");
    let manager = connect(&path);
    let app = connect(&path);
    let root = app.setup().roots[0].root;
    let window = input_window(
        &app,
        root,
        (100, 100, 100),
        EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE,
    );
    sync(&app);
    manager
        .grab_button(
            false,
            window,
            EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE,
            GrabMode::SYNC,
            GrabMode::ASYNC,
            0u32,
            0u32,
            x11rb::protocol::xproto::ButtonIndex::M1,
            ModMask::ANY,
        )
        .unwrap()
        .check()
        .unwrap();
    pointer_to(&display, 150, 150);
    sync(&app);
    thread::sleep(Duration::from_millis(50));
    drain_events(&app);

    // The press and the release come together, before the manager answers.
    button(&display, 1, true);
    button(&display, 1, false);
    wait_for_event(&manager, |e| matches!(e, Event::ButtonPress(_)));
    sync(&app);
    assert!(
        app.poll_for_event().unwrap().is_none(),
        "the application sees nothing while the press is held"
    );

    manager
        .allow_events(
            x11rb::protocol::xproto::Allow::REPLAY_POINTER,
            x11rb::CURRENT_TIME,
        )
        .unwrap();
    manager.flush().unwrap();
    let order: Vec<&str> = (0..2)
        .map(|_| {
            match wait_for_event(&app, |e| {
                matches!(e, Event::ButtonPress(_) | Event::ButtonRelease(_))
            }) {
                Event::ButtonPress(_) => "press",
                _ => "release",
            }
        })
        .collect();
    assert_eq!(order, ["press", "release"]);

    // The button is not stuck: the pointer state has no button down.
    sync(&app);
    let state = app.query_pointer(root).unwrap().reply().unwrap().mask;
    assert_eq!(u16::from(state) & 0x0100, 0, "button 1 is up");
}

#[test]
fn a_double_click_reaches_the_application_as_two_clicks_after_the_replay() {
    let (path, display) = start_server_with_display("replay-double-click");
    let manager = connect(&path);
    let app = connect(&path);
    let root = app.setup().roots[0].root;
    let window = input_window(
        &app,
        root,
        (100, 100, 100),
        EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE,
    );
    sync(&app);
    manager
        .grab_button(
            false,
            window,
            EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE,
            GrabMode::SYNC,
            GrabMode::ASYNC,
            0u32,
            0u32,
            x11rb::protocol::xproto::ButtonIndex::M1,
            ModMask::ANY,
        )
        .unwrap()
        .check()
        .unwrap();
    pointer_to(&display, 150, 150);
    sync(&app);
    thread::sleep(Duration::from_millis(50));
    drain_events(&app);

    // Two clicks arrive before the manager answers the first press.
    for pressed in [true, false, true, false] {
        button(&display, 1, pressed);
    }
    let answer = |presses_seen: usize| {
        for _ in 0..presses_seen {
            wait_for_event(&manager, |e| matches!(e, Event::ButtonPress(_)));
        }
        manager
            .allow_events(
                x11rb::protocol::xproto::Allow::REPLAY_POINTER,
                x11rb::CURRENT_TIME,
            )
            .unwrap();
        manager.flush().unwrap();
    };
    answer(1); // the first press
    answer(1); // the second press, which waited behind the first one
    let order: Vec<&str> = (0..4)
        .map(|_| {
            match wait_for_event(&app, |e| {
                matches!(e, Event::ButtonPress(_) | Event::ButtonRelease(_))
            }) {
                Event::ButtonPress(_) => "press",
                _ => "release",
            }
        })
        .collect();
    assert_eq!(order, ["press", "release", "press", "release"]);
}

#[test]
fn a_manager_that_never_answers_does_not_freeze_the_pointer_for_ever() {
    let (path, display) = start_server_with_display("replay-timeout");
    let manager = connect(&path);
    let app = connect(&path);
    let root = app.setup().roots[0].root;
    let window = input_window(
        &app,
        root,
        (100, 100, 100),
        EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE,
    );
    sync(&app);
    manager
        .grab_button(
            false,
            window,
            EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE,
            GrabMode::SYNC,
            GrabMode::ASYNC,
            0u32,
            0u32,
            x11rb::protocol::xproto::ButtonIndex::M1,
            ModMask::ANY,
        )
        .unwrap()
        .check()
        .unwrap();
    pointer_to(&display, 150, 150);
    sync(&app);
    thread::sleep(Duration::from_millis(50));
    // The manager takes the press and never calls AllowEvents. The release waits behind it,
    // and after a while it goes to the manager, which holds the grab.
    button(&display, 1, true);
    button(&display, 1, false);
    wait_for_event(&manager, |e| matches!(e, Event::ButtonPress(_)));
    wait_for_event(&manager, |e| matches!(e, Event::ButtonRelease(_)));
}

#[test]
fn text_selected_with_the_mouse_reaches_the_browser_clipboard() {
    let (path, display) = start_server_with_display("primary-out");
    let mut page = display.subscribe();
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(&conn, root, (0, 0, 20), EventMask::NO_EVENT);
    let utf8 = atom(&conn, b"UTF8_STRING");
    let primary = u32::from(AtomEnum::PRIMARY);
    conn.set_selection_owner(window, primary, x11rb::CURRENT_TIME)
        .unwrap();
    let Event::SelectionRequest(request) =
        wait_for_event(&conn, |e| matches!(e, Event::SelectionRequest(_)))
    else {
        unreachable!()
    };
    assert_eq!((request.selection, request.target), (primary, utf8));
    conn.change_property8(
        PropMode::REPLACE,
        request.requestor,
        request.property,
        utf8,
        b"seleccionado",
    )
    .unwrap();
    let answer = x11rb::protocol::xproto::SelectionNotifyEvent {
        response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: request.time,
        requestor: request.requestor,
        selection: request.selection,
        target: request.target,
        property: request.property,
    };
    conn.send_event(false, request.requestor, EventMask::NO_EVENT, answer)
        .unwrap();
    conn.flush().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(std::time::Instant::now() < deadline, "the page got no text");
        match page.try_recv() {
            Ok(message) => {
                if let Some((source, text)) = clipboard_text(&message) {
                    assert_eq!(source, wire::ClipboardSource::Primary);
                    assert_eq!(text, "seleccionado");
                    break;
                }
            }
            Err(_) => thread::sleep(Duration::from_millis(5)),
        }
    }
}

/// One thing that an X program offers for a selection.
struct Offer {
    target: Vec<u8>,
    bytes: Vec<u8>,
    /// Send it in chunks of this size, by the INCR protocol.
    incr_chunk: Option<usize>,
}

impl Offer {
    fn new(target: &[u8], bytes: Vec<u8>) -> Self {
        Self {
            target: target.to_vec(),
            bytes,
            incr_chunk: None,
        }
    }

    fn incr(mut self, chunk: usize) -> Self {
        self.incr_chunk = Some(chunk);
        self
    }
}

/// Plays an X program that owns a selection: answers `requests` SelectionRequests from the server.
/// TARGETS lists the offers (or is refused when `answer_targets` is false), a target that is not
/// offered is refused, and an offer with a chunk size goes by INCR.
fn serve_selection(
    conn: &RustConnection<DefaultStream>,
    offers: &[Offer],
    answer_targets: bool,
    requests: usize,
) {
    use x11rb::protocol::xproto::{Property, SelectionNotifyEvent, SELECTION_NOTIFY_EVENT};
    let targets = atom(conn, b"TARGETS");
    let incr = atom(conn, b"INCR");
    for _ in 0..requests {
        let Event::SelectionRequest(request) =
            wait_for_event(conn, |e| matches!(e, Event::SelectionRequest(_)))
        else {
            unreachable!()
        };
        let offer = offers
            .iter()
            .find(|offer| atom(conn, &offer.target) == request.target);
        let answered = if request.target == targets {
            if answer_targets {
                let atoms: Vec<u32> = std::iter::once(targets)
                    .chain(offers.iter().map(|offer| atom(conn, &offer.target)))
                    .collect();
                conn.change_property32(
                    PropMode::REPLACE,
                    request.requestor,
                    request.property,
                    AtomEnum::ATOM,
                    &atoms,
                )
                .unwrap();
            }
            answer_targets
        } else if let Some(offer) = offer {
            match offer.incr_chunk {
                None => conn
                    .change_property8(
                        PropMode::REPLACE,
                        request.requestor,
                        request.property,
                        request.target,
                        &offer.bytes,
                    )
                    .unwrap(),
                Some(_) => {
                    conn.change_window_attributes(
                        request.requestor,
                        &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
                    )
                    .unwrap();
                    conn.change_property32(
                        PropMode::REPLACE,
                        request.requestor,
                        request.property,
                        incr,
                        &[offer.bytes.len() as u32],
                    )
                    .unwrap()
                }
            };
            true
        } else {
            false
        };
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: if answered { request.property } else { 0 },
        };
        conn.send_event(false, request.requestor, EventMask::NO_EVENT, notify)
            .unwrap();
        conn.flush().unwrap();
        if let (Some(offer), Some(chunk)) = (offer, offer.and_then(|o| o.incr_chunk)) {
            let next = |conn: &RustConnection<DefaultStream>| {
                wait_for_event(conn, |e| {
                    matches!(e, Event::PropertyNotify(p)
                        if p.window == request.requestor
                            && p.atom == request.property
                            && p.state == Property::DELETE)
                })
            };
            for piece in offer.bytes.chunks(chunk).chain([&[][..]]) {
                next(conn);
                conn.change_property8(
                    PropMode::REPLACE,
                    request.requestor,
                    request.property,
                    request.target,
                    piece,
                )
                .unwrap();
                conn.flush().unwrap();
            }
        }
    }
}

/// A clipboard message for the page: its source and its parts.
type PageSet = (wire::ClipboardSource, Vec<(wire::ClipboardFormat, Vec<u8>)>);

/// The sets of the clipboard that the pages got in `wait`.
fn page_sets(
    page: &mut tokio::sync::broadcast::Receiver<xsrv_server::display::TileMessage>,
    wait: Duration,
) -> Vec<PageSet> {
    let mut found = Vec::new();
    let deadline = std::time::Instant::now() + wait;
    while std::time::Instant::now() < deadline {
        match page.try_recv() {
            Ok(message) => {
                if let Ok(wire::Message::ClipboardSet { source, parts }) = wire::decode(&message) {
                    found.push((source, parts.map(|(f, b)| (f, b.to_vec())).collect()));
                }
            }
            Err(_) => thread::sleep(Duration::from_millis(5)),
        }
    }
    found
}

/// Waits until some client owns the selection.
fn wait_for_owner(conn: &RustConnection<DefaultStream>, selection: u32) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while conn
        .get_selection_owner(selection)
        .unwrap()
        .reply()
        .unwrap()
        .owner
        == 0
    {
        assert!(
            std::time::Instant::now() < deadline,
            "nobody owns the selection"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

/// The atoms of a TARGETS answer (the server writes them in little endian).
fn atoms_in(list: &[u8]) -> Vec<u32> {
    list.as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| u32::from_le_bytes(*bytes))
        .collect()
}

/// Asks the owner of `selection` for `target` and returns the bytes of the answer, or `None` when
/// it refused.
fn fetch_selection(
    conn: &RustConnection<DefaultStream>,
    window: u32,
    selection: u32,
    target: &[u8],
) -> Option<Vec<u8>> {
    let target = atom(conn, target);
    let property = atom(conn, b"FETCHED");
    conn.convert_selection(window, selection, target, property, x11rb::CURRENT_TIME)
        .unwrap();
    let Event::SelectionNotify(done) =
        wait_for_event(conn, |e| matches!(e, Event::SelectionNotify(_)))
    else {
        unreachable!()
    };
    if done.property == 0 {
        return None;
    }
    let reply = conn
        .get_property(true, window, property, AtomEnum::ANY, 0, u32::MAX / 4)
        .unwrap()
        .reply()
        .unwrap();
    Some(reply.value)
}

/// A terminal that is asked to copy with no selection owns CLIPBOARD and answers with empty text.
/// The page must not get it: copying nothing keeps the browser clipboard.
#[test]
fn empty_text_from_an_x_client_does_not_reach_the_browser_clipboard() {
    let (path, display) = start_server_with_display("empty-copy");
    let mut page = display.subscribe();
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(&conn, root, (0, 0, 20), EventMask::NO_EVENT);
    let clipboard = atom(&conn, b"CLIPBOARD");
    let copy = |text: &[u8]| {
        conn.set_selection_owner(window, clipboard, x11rb::CURRENT_TIME)
            .unwrap();
        serve_selection(&conn, &[Offer::new(b"UTF8_STRING", text.to_vec())], true, 2);
    };
    let texts = |page: &mut tokio::sync::broadcast::Receiver<xsrv_server::display::TileMessage>| {
        let mut found = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_millis(400);
        while std::time::Instant::now() < deadline {
            match page.try_recv() {
                Ok(message) => {
                    if let Some((_, text)) = clipboard_text(&message) {
                        found.push(text.to_owned());
                    }
                }
                Err(_) => thread::sleep(Duration::from_millis(5)),
            }
        }
        found
    };
    copy(b"");
    assert_eq!(
        texts(&mut page),
        Vec::<String>::new(),
        "empty text is not a copy"
    );
    thread::sleep(Duration::from_millis(5));
    copy(b"texto");
    assert_eq!(
        texts(&mut page),
        ["texto"],
        "a later real copy still arrives"
    );
}

#[test]
fn text_html_and_an_image_from_an_x_client_reach_the_page_in_one_set() {
    let (path, display) = start_server_with_display("copy-formats");
    let mut page = display.subscribe();
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(&conn, root, (0, 0, 20), EventMask::NO_EVENT);
    let clipboard = atom(&conn, b"CLIPBOARD");
    // 2 MiB in one property: more than a normal window property may hold.
    let mut png = vec![0x89, b'P', b'N', b'G'];
    png.extend(vec![7u8; 2 * 1024 * 1024]);
    conn.set_selection_owner(window, clipboard, x11rb::CURRENT_TIME)
        .unwrap();
    serve_selection(
        &conn,
        &[
            Offer::new(b"UTF8_STRING", b"hola".to_vec()),
            Offer::new(b"text/html", b"<b>hola</b>".to_vec()),
            Offer::new(b"image/png", png.clone()),
        ],
        true,
        4,
    );
    let sets = page_sets(&mut page, Duration::from_millis(1500));
    assert_eq!(sets.len(), 1, "one copy is one message");
    assert_eq!(sets[0].0, wire::ClipboardSource::Clipboard);
    assert_eq!(
        sets[0].1,
        vec![
            (wire::ClipboardFormat::Text, b"hola".to_vec()),
            (wire::ClipboardFormat::Html, b"<b>hola</b>".to_vec()),
            (wire::ClipboardFormat::Png, png),
        ]
    );
}

#[test]
fn a_long_text_that_an_x_client_sends_in_chunks_arrives_whole() {
    let (path, display) = start_server_with_display("copy-incr");
    let mut page = display.subscribe();
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(&conn, root, (0, 0, 20), EventMask::NO_EVENT);
    let clipboard = atom(&conn, b"CLIPBOARD");
    let text: Vec<u8> = (0..300_000).map(|i| b'a' + (i % 26) as u8).collect();
    conn.set_selection_owner(window, clipboard, x11rb::CURRENT_TIME)
        .unwrap();
    serve_selection(
        &conn,
        &[Offer::new(b"UTF8_STRING", text.clone()).incr(100_000)],
        true,
        2,
    );
    let sets = page_sets(&mut page, Duration::from_millis(1500));
    assert_eq!(sets.len(), 1);
    assert_eq!(sets[0].1, vec![(wire::ClipboardFormat::Text, text)]);
}

#[test]
fn an_x_client_with_only_string_is_read_as_latin_1() {
    let (path, display) = start_server_with_display("copy-string");
    let mut page = display.subscribe();
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(&conn, root, (0, 0, 20), EventMask::NO_EVENT);
    let clipboard = atom(&conn, b"CLIPBOARD");
    conn.set_selection_owner(window, clipboard, x11rb::CURRENT_TIME)
        .unwrap();
    serve_selection(
        &conn,
        &[Offer::new(b"STRING", b"caf\xe9".to_vec())],
        true,
        2,
    );
    let sets = page_sets(&mut page, Duration::from_millis(800));
    assert_eq!(
        sets[0].1,
        vec![(wire::ClipboardFormat::Text, "café".as_bytes().to_vec())]
    );
}

#[test]
fn an_x_client_that_refuses_targets_is_asked_for_text() {
    let (path, display) = start_server_with_display("copy-no-targets");
    let mut page = display.subscribe();
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(&conn, root, (0, 0, 20), EventMask::NO_EVENT);
    let clipboard = atom(&conn, b"CLIPBOARD");
    conn.set_selection_owner(window, clipboard, x11rb::CURRENT_TIME)
        .unwrap();
    serve_selection(
        &conn,
        &[Offer::new(b"UTF8_STRING", b"viejo".to_vec())],
        false,
        2,
    );
    let sets = page_sets(&mut page, Duration::from_millis(800));
    assert_eq!(
        sets[0].1,
        vec![(wire::ClipboardFormat::Text, b"viejo".to_vec())]
    );
}

#[test]
fn a_text_that_is_too_big_is_left_out_of_the_set() {
    let (path, display) = start_server_with_display("copy-too-big");
    let mut page = display.subscribe();
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(&conn, root, (0, 0, 20), EventMask::NO_EVENT);
    let clipboard = atom(&conn, b"CLIPBOARD");
    conn.set_selection_owner(window, clipboard, x11rb::CURRENT_TIME)
        .unwrap();
    // The text is over the limit, the HTML is not: the page gets the HTML only.
    let big = vec![b'a'; wire::MAX_CLIPBOARD_TEXT + 1];
    serve_selection(
        &conn,
        &[
            Offer::new(b"UTF8_STRING", big).incr(1 << 20),
            Offer::new(b"text/html", b"<p>x</p>".to_vec()),
        ],
        true,
        3,
    );
    let sets = page_sets(&mut page, Duration::from_millis(1500));
    assert_eq!(sets.len(), 1);
    assert_eq!(
        sets[0].1,
        vec![(wire::ClipboardFormat::Html, b"<p>x</p>".to_vec())]
    );
}

#[test]
fn a_set_from_the_page_is_served_with_html_and_an_image() {
    let (path, display) = start_server_with_display("paste-formats");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(&conn, root, (0, 0, 20), EventMask::NO_EVENT);
    let clipboard = atom(&conn, b"CLIPBOARD");
    let png: Vec<u8> = [0x89, b'P', b'N', b'G', 1, 2, 3].to_vec();
    display.send_clipboard_in(ClipboardData {
        text: Some("hola".into()),
        html: Some("<i>hola</i>".into()),
        png: Some(png.clone()),
    });
    wait_for_owner(&conn, clipboard);

    let list = fetch_selection(&conn, window, clipboard, b"TARGETS").unwrap();
    let offered = atoms_in(&list);
    for name in [&b"UTF8_STRING"[..], b"text/html", b"image/png"] {
        assert!(
            offered.contains(&atom(&conn, name)),
            "{:?}",
            String::from_utf8_lossy(name)
        );
    }
    assert_eq!(
        fetch_selection(&conn, window, clipboard, b"UTF8_STRING").unwrap(),
        b"hola"
    );
    assert_eq!(
        fetch_selection(&conn, window, clipboard, b"text/html").unwrap(),
        b"<i>hola</i>"
    );
    assert_eq!(
        fetch_selection(&conn, window, clipboard, b"image/png").unwrap(),
        png
    );
}

#[test]
fn an_empty_set_from_the_page_pastes_nothing_and_not_the_old_text() {
    let (path, display) = start_server_with_display("paste-nothing");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = input_window(&conn, root, (0, 0, 20), EventMask::NO_EVENT);
    let clipboard = atom(&conn, b"CLIPBOARD");
    display.send_clipboard_in(ClipboardData::text_only("viejo".into()));
    wait_for_owner(&conn, clipboard);
    assert_eq!(
        fetch_selection(&conn, window, clipboard, b"UTF8_STRING").unwrap(),
        b"viejo"
    );

    // The browser clipboard now holds something that is not text: the page sends an empty set.
    display.send_clipboard_in(ClipboardData::default());
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let text = fetch_selection(&conn, window, clipboard, b"UTF8_STRING").unwrap();
        if text.is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the old text is still served"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let list = fetch_selection(&conn, window, clipboard, b"TARGETS").unwrap();
    let offered = atoms_in(&list);
    assert!(!offered.contains(&atom(&conn, b"text/html")));
    assert!(!offered.contains(&atom(&conn, b"image/png")));
}

/// Chromium and Electron (VS Code) take a present XInputExtension to mean XI 2, and stop when
/// XIQueryDevice fails. Without the extension they use the core events.
#[test]
fn xinput_is_not_offered() {
    let conn = connect(&start_server("xinput"));
    let query = conn
        .query_extension(b"XInputExtension")
        .unwrap()
        .reply()
        .unwrap();
    assert!(!query.present);
}

/// A page: it starts from a snapshot and applies each message, like the WASM decoder.
struct PageModel {
    screen: xsrv_server::framebuffer::Framebuffer,
    copies: usize,
}

impl PageModel {
    fn new(display: &Display, full_tile: &[u8]) -> Self {
        let (width, height) = display.size();
        let mut page = Self {
            screen: xsrv_server::framebuffer::Framebuffer::new(width, height),
            copies: 0,
        };
        page.apply(full_tile);
        page
    }

    fn apply(&mut self, message: &[u8]) {
        if let Some((rect, pixels)) = tile(message) {
            self.screen.write_region(rect, &pixels).unwrap();
            return;
        }
        match wire::decode(message).unwrap() {
            wire::Message::Copy { from, to } => {
                self.screen.copy_rect(from, to).unwrap();
                self.copies += 1;
            }
            wire::Message::Screen { width, height } => self.resize(width, height),
            _ => {}
        }
    }

    /// Like `web/main.js`: the old picture stays at the top left, the rest is not black.
    fn resize(&mut self, width: u16, height: u16) {
        use xsrv_server::framebuffer::Framebuffer;
        let mut screen = Framebuffer::new(width, height);
        let unknown = vec![0xEE; usize::from(width) * usize::from(height) * 4];
        screen.write_region(screen.full_rect(), &unknown).unwrap();
        let (old_width, old_height) = self.screen.size();
        let (w, h) = (width.min(old_width), height.min(old_height));
        let kept = self.screen.read_region(0, 0, w.into(), h.into());
        let rect = wire::Rect { x: 0, y: 0, w, h };
        screen.write_region(rect, &kept).unwrap();
        self.screen = screen;
    }

    fn apply_all(
        &mut self,
        tiles: &mut tokio::sync::broadcast::Receiver<xsrv_server::display::TileMessage>,
    ) {
        while let Ok(message) = tiles.try_recv() {
            self.apply(&message);
        }
    }

    fn matches(&self, display: &Display) -> bool {
        let (width, height) = display.size();
        self.screen.pixels() == display.read_region(0, 0, width.into(), height.into())
    }
}

/// A window with 30 bands of 16 rows, each band with its own color.
fn striped_window(conn: &RustConnection<DefaultStream>, x: i16, y: i16) -> (u32, u32) {
    let root = conn.setup().roots[0].root;
    let window = conn.generate_id().unwrap();
    conn.create_window(
        24,
        window,
        root,
        x,
        y,
        200,
        480,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    conn.map_window(window).unwrap();
    let gc = conn.generate_id().unwrap();
    conn.create_gc(gc, window, &CreateGCAux::new().graphics_exposures(0))
        .unwrap();
    for band in 0..30u32 {
        conn.change_gc(gc, &ChangeGCAux::new().foreground(band * 0x0008_0402))
            .unwrap();
        let rect = Rectangle {
            x: 0,
            y: band as i16 * 16,
            width: 200,
            height: 16,
        };
        conn.poly_fill_rectangle(window, gc, &[rect]).unwrap();
    }
    (window, gc)
}

#[test]
fn a_scroll_inside_a_window_reaches_the_page_as_a_copy() {
    let (path, display) = start_server_with_display("scrollcopy");
    let conn = connect(&path);
    let (window, gc) = striped_window(&conn, 20, 0);
    sync(&conn);
    let (mut tiles, snapshot) = display.subscribe_with_snapshot();
    let mut page = PageModel::new(&display, &snapshot.1);
    for _ in 0..5 {
        conn.copy_area(window, window, gc, 0, 16, 0, 0, 200, 464)
            .unwrap();
        conn.change_gc(gc, &ChangeGCAux::new().foreground(0x00FF_0000))
            .unwrap();
        let bottom = Rectangle {
            x: 0,
            y: 464,
            width: 200,
            height: 16,
        };
        conn.poly_fill_rectangle(window, gc, &[bottom]).unwrap();
        sync(&conn);
        page.apply_all(&mut tiles);
    }
    assert_eq!(page.copies, 5);
    assert!(page.matches(&display), "the page shows the screen");
    let row = |y: i32| display.read_region(20, y, 1, 1);
    assert_eq!(row(0), [0x0A, 0x14, 0x28, 0], "band 5 is at the top");
    assert_eq!(
        window_pixels(&conn, window, gc),
        display.read_region(20, 0, 200, 480),
        "the moved screen pixels are the pixels of the window"
    );
}

#[test]
fn a_scroll_in_a_partly_covered_window_copies_the_shown_parts() {
    let (path, display) = start_server_with_display("scrollcoveredpart");
    let conn = connect(&path);
    let (window, gc) = striped_window(&conn, 20, 0);
    let root = conn.setup().roots[0].root;
    // A window over the right half of rows 100 to 160 (a clock over a terminal).
    let cover = input_window(&conn, root, (120, 100, 60), EventMask::NO_EVENT);
    conn.map_window(cover).unwrap();
    let cover_gc = conn.generate_id().unwrap();
    conn.create_gc(cover_gc, cover, &CreateGCAux::new().foreground(0x0000_FF00))
        .unwrap();
    let all = Rectangle {
        x: 0,
        y: 0,
        width: 60,
        height: 60,
    };
    conn.poly_fill_rectangle(cover, cover_gc, &[all]).unwrap();
    sync(&conn);
    let (mut tiles, snapshot) = display.subscribe_with_snapshot();
    let mut page = PageModel::new(&display, &snapshot.1);
    let mut bytes = 0;
    for _ in 0..5 {
        conn.copy_area(window, window, gc, 0, 16, 0, 0, 200, 464)
            .unwrap();
        conn.change_gc(gc, &ChangeGCAux::new().foreground(0x00FF_0000))
            .unwrap();
        let bottom = Rectangle {
            x: 0,
            y: 464,
            width: 200,
            height: 16,
        };
        conn.poly_fill_rectangle(window, gc, &[bottom]).unwrap();
        sync(&conn);
        while let Ok(message) = tiles.try_recv() {
            bytes += message.len();
            page.apply(&message);
        }
    }
    assert!(
        page.copies > 5,
        "more than one copy for each scroll: {}",
        page.copies
    );
    assert!(page.matches(&display), "the page shows the screen");
    assert_eq!(
        display.read_region(130, 110, 1, 1),
        [0, 0xFF, 0, 0],
        "the covering window is still on top"
    );
    // Without copies each scroll would send the whole shown window (about 90 000 pixels).
    assert!(bytes < 20_000, "{bytes} bytes for five scrolls");
    // The page follows the server screen; the screen must show the window as it is now, beside,
    // above and below the covering window (window at x 20, cover at 120..180 x 100..160).
    let window_now = window_pixels(&conn, window, gc);
    for (x, y) in [
        (50, 50),
        (110, 130),
        (190, 130),
        (150, 90),
        (150, 170),
        (200, 470),
    ] {
        let offset = (y * 200 + (x - 20)) * 4;
        assert_eq!(
            display.read_region(x as i32, y as i32, 1, 1),
            window_now[offset..offset + 4],
            "at {x},{y}"
        );
    }
}

#[test]
fn a_new_cursor_sends_no_tile_and_a_new_border_colour_does() {
    let (path, display) = start_server_with_display("cursor-border");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let window = conn.generate_id().unwrap();
    conn.create_window(
        24,
        window,
        root,
        10,
        10,
        300,
        200,
        2,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new().border_pixel(0x0000_00FF),
    )
    .unwrap();
    conn.map_window(window).unwrap();
    sync(&conn);
    let mut tiles = display.subscribe();
    let cursor = conn.generate_id().unwrap();
    let font = conn.generate_id().unwrap();
    conn.open_font(font, b"cursor").unwrap();
    conn.create_glyph_cursor(cursor, font, font, 68, 69, 0, 0, 0, 0xFFFF, 0xFFFF, 0xFFFF)
        .unwrap();
    // xterm does this with every line of output.
    conn.change_window_attributes(window, &ChangeWindowAttributesAux::new().cursor(cursor))
        .unwrap();
    sync(&conn);
    let sent: Vec<_> = std::iter::from_fn(|| tiles.try_recv().ok())
        .filter(|message| tile(message).is_some())
        .collect();
    assert!(sent.is_empty(), "{} tiles for a new cursor", sent.len());

    conn.change_window_attributes(
        window,
        &ChangeWindowAttributesAux::new().border_pixel(0x00FF_0000),
    )
    .unwrap();
    sync(&conn);
    let drawn =
        std::iter::from_fn(|| tiles.try_recv().ok()).any(|message| tile(&message).is_some());
    assert!(drawn, "a new border colour reaches the page");
    assert_eq!(
        screen_pixel(&display, 10, 10),
        [0, 0, 0xFF],
        "the border is red now"
    );
}

#[test]
fn a_scroll_under_a_shaped_window_leaves_the_page_right() {
    use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
    let (path, display) = start_server_with_display("scrollshaped");
    let conn = connect(&path);
    let (window, gc) = striped_window(&conn, 20, 0);
    let root = conn.setup().roots[0].root;
    // A ring over the striped window: the window shows through its middle.
    let ring = input_window(&conn, root, (100, 100, 80), EventMask::NO_EVENT);
    let edge = |x, y, width, height| Rectangle {
        x,
        y,
        width,
        height,
    };
    conn.shape_rectangles(
        SO::SET,
        SK::BOUNDING,
        ClipOrdering::UNSORTED,
        ring,
        0,
        0,
        &[
            edge(0, 0, 80, 10),
            edge(0, 70, 80, 10),
            edge(0, 10, 10, 60),
            edge(70, 10, 10, 60),
        ],
    )
    .unwrap();
    conn.map_window(ring).unwrap();
    let ring_gc = conn.generate_id().unwrap();
    conn.create_gc(ring_gc, ring, &CreateGCAux::new().foreground(0x0000_FF00))
        .unwrap();
    conn.poly_fill_rectangle(ring, ring_gc, &[edge(0, 0, 80, 80)])
        .unwrap();
    sync(&conn);
    let (mut tiles, snapshot) = display.subscribe_with_snapshot();
    let mut page = PageModel::new(&display, &snapshot.1);
    for _ in 0..3 {
        conn.copy_area(window, window, gc, 0, 16, 0, 0, 200, 464)
            .unwrap();
        sync(&conn);
        page.apply_all(&mut tiles);
    }
    assert!(page.matches(&display), "the page shows the screen");
    // The page model follows the server screen, so it cannot see a screen that is wrong in the
    // same way. Inside the ring the screen must show the window as it is now (window at x 20).
    let window_now = window_pixels(&conn, window, gc);
    for (x, y) in [(140, 140), (115, 115), (165, 165)] {
        let offset = (y * 200 + (x - 20)) * 4;
        assert_eq!(
            display.read_region(x as i32, y as i32, 1, 1),
            window_now[offset..offset + 4],
            "inside the ring at {x},{y}"
        );
    }
}

#[test]
fn a_new_root_background_around_a_window_reaches_the_page() {
    let (path, display) = start_server_with_display("rootbackground");
    let conn = connect(&path);
    let root = conn.setup().roots[0].root;
    let (window, _gc) = striped_window(&conn, 20, 0);
    sync(&conn);
    let (mut tiles, snapshot) = display.subscribe_with_snapshot();
    let mut page = PageModel::new(&display, &snapshot.1);
    let before = display.read_region(20, 0, 200, 480);
    for colour in [0x0020_4080, 0x0040_8020] {
        conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().background_pixel(colour),
        )
        .unwrap();
        conn.clear_area(false, root, 0, 0, 0, 0).unwrap();
        sync(&conn);
        page.apply_all(&mut tiles);
        assert_eq!(
            screen_pixel(&display, 300, 100),
            BLUE_GREEN_RED(colour),
            "the root around it"
        );
    }
    assert_eq!(
        display.read_region(20, 0, 200, 480),
        before,
        "the window did not change"
    );
    assert!(page.matches(&display));
    let _ = window;
}

/// The pixels of a 200x480 window as the window holds them, read through a pixmap, BGRX.
fn window_pixels(conn: &RustConnection<DefaultStream>, window: u32, gc: u32) -> Vec<u8> {
    let pixmap = conn.generate_id().unwrap();
    conn.create_pixmap(24, pixmap, window, 200, 480).unwrap();
    conn.copy_area(window, pixmap, gc, 0, 0, 0, 0, 200, 480)
        .unwrap();
    let image = conn
        .get_image(ImageFormat::Z_PIXMAP, pixmap, 0, 0, 200, 480, !0)
        .unwrap()
        .reply()
        .unwrap();
    image
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 0])
        .collect()
}

#[test]
fn a_scroll_right_after_a_screen_resize_leaves_the_page_right() {
    let (path, display) = start_server_with_display("scrollresize");
    let conn = connect(&path);
    let (window, gc) = striped_window(&conn, 20, 0);
    sync(&conn);
    let (mut tiles, snapshot) = display.subscribe_with_snapshot();
    let mut page = PageModel::new(&display, &snapshot.1);
    display.send_input(Input::Resize {
        width: 600,
        height: 500,
    });
    // The resize goes through the input channel: wait until the server applied it.
    for _ in 0..200 {
        if display.size() == (600, 500) {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    // The page keeps its old picture; the server holds drawing for a moment after a resize.
    for _ in 0..3 {
        conn.copy_area(window, window, gc, 0, 16, 0, 0, 200, 464)
            .unwrap();
        sync(&conn);
    }
    thread::sleep(Duration::from_millis(150));
    sync(&conn);
    conn.copy_area(window, window, gc, 0, 16, 0, 0, 200, 464)
        .unwrap();
    sync(&conn);
    page.apply_all(&mut tiles);
    assert_eq!(display.size(), (600, 500));
    // Under load the hold can end before the first scrolls run, so only a lower bound is fixed.
    assert!(page.copies >= 1, "the scroll after the hold is a copy");
    assert!(page.matches(&display), "the page shows the screen");
}

#[test]
fn a_scroll_under_another_window_copies_what_is_shown() {
    let (path, display) = start_server_with_display("scrollcovered");
    let conn = connect(&path);
    let (window, gc) = striped_window(&conn, 20, 0);
    let (_above, _) = striped_window(&conn, 100, 100);
    sync(&conn);
    let (mut tiles, snapshot) = display.subscribe_with_snapshot();
    let mut page = PageModel::new(&display, &snapshot.1);
    conn.copy_area(window, window, gc, 0, 16, 0, 0, 200, 464)
        .unwrap();
    sync(&conn);
    page.apply_all(&mut tiles);
    assert!(page.copies > 0, "the shown parts move as copies");
    assert!(page.matches(&display));
}

/// RENDER clients (cairo) need pixmaps of depth 8 (alpha masks) and 32 (ARGB images).
#[test]
fn pixmaps_of_depth_8_and_32_keep_their_bits() {
    let conn = connect(&start_server("deep-pixmaps"));
    let formats: Vec<(u8, u8)> = conn
        .setup()
        .pixmap_formats
        .iter()
        .map(|format| (format.depth, format.bits_per_pixel))
        .collect();
    assert_eq!(formats, [(1, 1), (8, 8), (24, 32), (32, 32)]);
    let depths: Vec<u8> = conn.setup().roots[0]
        .allowed_depths
        .iter()
        .map(|depth| depth.depth)
        .collect();
    assert_eq!(depths, [24, 1, 8, 32]);
    let root = conn.setup().roots[0].root;

    // Depth 8: one byte a pixel, rows padded to 4 bytes.
    let alpha = conn.generate_id().unwrap();
    conn.create_pixmap(8, alpha, root, 3, 2).unwrap();
    let alpha_gc = conn.generate_id().unwrap();
    conn.create_gc(alpha_gc, alpha, &CreateGCAux::new())
        .unwrap();
    let rows = [10, 20, 30, 0, 40, 50, 60, 0];
    conn.put_image(
        ImageFormat::Z_PIXMAP,
        alpha,
        alpha_gc,
        3,
        2,
        0,
        0,
        0,
        8,
        &rows,
    )
    .unwrap()
    .check()
    .unwrap();
    let image = conn
        .get_image(ImageFormat::Z_PIXMAP, alpha, 0, 0, 3, 2, !0)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!((image.depth, image.data), (8, rows.to_vec()));

    // Depth 32: the fourth byte (alpha) is kept, through PutImage, CopyArea and a fill.
    let argb = conn.generate_id().unwrap();
    conn.create_pixmap(32, argb, root, 2, 1).unwrap();
    let argb_gc = conn.generate_id().unwrap();
    conn.create_gc(argb_gc, argb, &CreateGCAux::new().foreground(0x8000_0080))
        .unwrap();
    conn.put_image(
        ImageFormat::Z_PIXMAP,
        argb,
        argb_gc,
        1,
        1,
        0,
        0,
        0,
        32,
        &[1, 2, 3, 0x40],
    )
    .unwrap()
    .check()
    .unwrap();
    conn.copy_area(argb, argb, argb_gc, 0, 0, 1, 0, 1, 1)
        .unwrap();
    let image = conn
        .get_image(ImageFormat::Z_PIXMAP, argb, 0, 0, 2, 1, !0)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(image.data, [1, 2, 3, 0x40, 1, 2, 3, 0x40]);
    let all = Rectangle {
        x: 0,
        y: 0,
        width: 2,
        height: 1,
    };
    conn.poly_fill_rectangle(argb, argb_gc, &[all]).unwrap();
    let image = conn
        .get_image(ImageFormat::Z_PIXMAP, argb, 0, 0, 1, 1, !0)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(image.data, [0x80, 0, 0, 0x80]);

    // A tiled fill copies the alpha of a depth-32 tile too.
    let tile = conn.generate_id().unwrap();
    conn.create_pixmap(32, tile, root, 1, 1).unwrap();
    conn.put_image(
        ImageFormat::Z_PIXMAP,
        tile,
        argb_gc,
        1,
        1,
        0,
        0,
        0,
        32,
        &[5, 6, 7, 0x40],
    )
    .unwrap();
    let tiled = conn.generate_id().unwrap();
    let aux = CreateGCAux::new().fill_style(FillStyle::TILED).tile(tile);
    conn.create_gc(tiled, argb, &aux).unwrap();
    conn.poly_fill_rectangle(argb, tiled, &[all]).unwrap();
    let image = conn
        .get_image(ImageFormat::Z_PIXMAP, argb, 1, 0, 1, 1, !0)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(image.data, [5, 6, 7, 0x40]);

    // A depth-24 image does not fit a depth-32 pixmap.
    let wrong = conn
        .put_image(
            ImageFormat::Z_PIXMAP,
            argb,
            argb_gc,
            1,
            1,
            0,
            0,
            0,
            24,
            &[0; 4],
        )
        .unwrap()
        .check();
    assert_eq!(error_kind(wrong), ErrorKind::Match);
}

mod render_tests {
    use super::*;
    use x11rb::protocol::render::{
        self, Color, ConnectionExt as _, CreatePictureAux, Glyphinfo, PictOp, Pictformat,
    };
    use x11rb::protocol::xproto::SubwindowMode;

    /// The id of the format of `depth` that QueryPictFormats reports.
    fn format_of(conn: &RustConnection<DefaultStream>, depth: u8) -> Pictformat {
        let formats = conn.render_query_pict_formats().unwrap().reply().unwrap();
        formats
            .formats
            .iter()
            .find(|format| format.depth == depth)
            .map(|format| format.id)
            .unwrap_or_else(|| panic!("no format of depth {depth}"))
    }

    /// A picture on a new pixmap of `depth`, filled with `colour` (premultiplied, 16 bits).
    fn filled_picture(
        conn: &RustConnection<DefaultStream>,
        depth: u8,
        size: u16,
        colour: Color,
        aux: &CreatePictureAux,
    ) -> u32 {
        let root = conn.setup().roots[0].root;
        let pixmap = conn.generate_id().unwrap();
        conn.create_pixmap(depth, pixmap, root, size, size).unwrap();
        let picture = conn.generate_id().unwrap();
        conn.render_create_picture(picture, pixmap, format_of(conn, depth), aux)
            .unwrap();
        let all = Rectangle {
            x: 0,
            y: 0,
            width: size,
            height: size,
        };
        conn.render_fill_rectangles(PictOp::SRC, picture, colour, &[all])
            .unwrap();
        // The picture keeps the pixmap alive, as cairo expects.
        conn.free_pixmap(pixmap).unwrap();
        picture
    }

    fn colour(red: u16, green: u16, blue: u16, alpha: u16) -> Color {
        Color {
            red,
            green,
            blue,
            alpha,
        }
    }

    fn window_picture(conn: &RustConnection<DefaultStream>) -> u32 {
        let (window, _) = mapped_window(conn, 10, 10, 40);
        let picture = conn.generate_id().unwrap();
        conn.render_create_picture(
            picture,
            window,
            format_of(conn, 24),
            &CreatePictureAux::new(),
        )
        .unwrap();
        let all = Rectangle {
            x: 0,
            y: 0,
            width: 40,
            height: 40,
        };
        conn.render_fill_rectangles(PictOp::SRC, picture, colour(0, 0, 0, 0xFFFF), &[all])
            .unwrap();
        picture
    }

    #[test]
    fn version_and_formats() {
        let conn = connect(&start_server("render-query"));
        let version = conn.render_query_version(0, 11).unwrap().reply().unwrap();
        assert_eq!((version.major_version, version.minor_version), (0, 1));
        let formats = conn.render_query_pict_formats().unwrap().reply().unwrap();
        let depths: Vec<u8> = formats.formats.iter().map(|format| format.depth).collect();
        assert_eq!(depths, [1, 8, 24, 32]);
        let argb = formats
            .formats
            .iter()
            .find(|format| format.depth == 32)
            .unwrap();
        assert_eq!(
            (argb.direct.alpha_shift, argb.direct.alpha_mask),
            (24, 0xFF)
        );
        let screen = &formats.screens[0];
        let visual = conn.setup().roots[0].root_visual;
        let mapped = screen
            .depths
            .iter()
            .flat_map(|depth| &depth.visuals)
            .find(|entry| entry.visual == visual)
            .map(|entry| entry.format);
        assert_eq!(mapped, Some(format_of(&conn, 24)));
    }

    #[test]
    fn fill_and_composite_reach_the_screen() {
        let (path, display) = start_server_with_display("render-composite");
        let conn = connect(&path);
        let target = window_picture(&conn);
        let red = Rectangle {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        };
        conn.render_fill_rectangles(PictOp::SRC, target, colour(0xFFFF, 0, 0, 0xFFFF), &[red])
            .unwrap();
        // Half-transparent green (premultiplied) Over the red square and the black around it.
        let green = filled_picture(
            &conn,
            32,
            4,
            colour(0, 0x8000, 0, 0x8000),
            &CreatePictureAux::new().repeat(render::Repeat::NORMAL),
        );
        conn.render_composite(PictOp::OVER, green, 0u32, target, 0, 0, 0, 0, 5, 5, 10, 10)
            .unwrap();
        sync(&conn);
        assert_eq!(
            screen_pixel(&display, 10, 10),
            [0, 0, 0xFF],
            "red, untouched"
        );
        assert_eq!(
            screen_pixel(&display, 17, 17),
            [0, 0x80, 0x7F],
            "green over red"
        );
        assert_eq!(
            screen_pixel(&display, 20, 20),
            [0, 0x80, 0],
            "green over black"
        );
        assert_eq!(
            screen_pixel(&display, 30, 30),
            [0, 0, 0],
            "outside the composite"
        );
    }

    #[test]
    fn a_clip_limits_the_destination() {
        let (path, display) = start_server_with_display("render-clip");
        let conn = connect(&path);
        let target = window_picture(&conn);
        let clip = Rectangle {
            x: 0,
            y: 0,
            width: 5,
            height: 40,
        };
        conn.render_set_picture_clip_rectangles(target, 2, 0, &[clip])
            .unwrap();
        let all = Rectangle {
            x: 0,
            y: 0,
            width: 40,
            height: 40,
        };
        conn.render_fill_rectangles(
            PictOp::SRC,
            target,
            colour(0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF),
            &[all],
        )
        .unwrap();
        sync(&conn);
        assert_eq!(
            screen_pixel(&display, 11, 20),
            [0, 0, 0],
            "left of the clip"
        );
        assert_eq!(screen_pixel(&display, 12, 20), [0xFF; 3], "inside the clip");
        assert_eq!(
            screen_pixel(&display, 17, 20),
            [0, 0, 0],
            "right of the clip"
        );
    }

    #[test]
    fn glyphs_draw_with_and_without_a_mask_format() {
        let (path, display) = start_server_with_display("render-glyphs");
        let conn = connect(&path);
        let target = window_picture(&conn);
        let white = filled_picture(
            &conn,
            32,
            1,
            colour(0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF),
            &CreatePictureAux::new().repeat(render::Repeat::NORMAL),
        );
        let set = conn.generate_id().unwrap();
        conn.render_create_glyph_set(set, format_of(&conn, 8))
            .unwrap();
        // A 2x2 glyph: opaque on the top row, half on the bottom row. Rows are padded to 4 bytes.
        let info = Glyphinfo {
            width: 2,
            height: 2,
            x: 0,
            y: 2,
            x_off: 3,
            y_off: 0,
        };
        let image = [0xFF, 0xFF, 0, 0, 0x80, 0x80, 0, 0];
        conn.render_add_glyphs(set, &[7], &[info], &image).unwrap();
        // One element: 2 glyphs at pen (5, 5), then a third one at (20, 5).
        let mut items = vec![2, 0, 0, 0, 5, 0, 5, 0, 7, 7, 0, 0];
        items.extend_from_slice(&[1, 0, 0, 0, 9, 0, 0, 0, 7, 0, 0, 0]);
        conn.render_composite_glyphs8(PictOp::OVER, white, target, 0, set, 0, 0, &items)
            .unwrap();
        conn.render_composite_glyphs8(
            PictOp::OVER,
            white,
            target,
            format_of(&conn, 8),
            set,
            0,
            0,
            &[1, 0, 0, 0, 5, 0, 25, 0, 7, 0, 0, 0],
        )
        .unwrap();
        sync(&conn);
        // The window is at (10, 10); a glyph with y = 2 starts 2 rows above the pen.
        assert_eq!(
            screen_pixel(&display, 15, 13),
            [0xFF; 3],
            "first glyph, top row"
        );
        assert_eq!(
            screen_pixel(&display, 15, 14),
            [0x80; 3],
            "first glyph, bottom row"
        );
        assert_eq!(
            screen_pixel(&display, 18, 13),
            [0xFF; 3],
            "second glyph, 3 to the right"
        );
        assert_eq!(
            screen_pixel(&display, 17, 13),
            [0, 0, 0],
            "between the glyphs"
        );
        assert_eq!(
            screen_pixel(&display, 30, 13),
            [0xFF; 3],
            "third glyph, 9 after the pen"
        );
        assert_eq!(
            screen_pixel(&display, 15, 33),
            [0xFF; 3],
            "the masked request"
        );
    }

    /// A 2x1 picture without repeat: red at x = 0, blue at x = 1.
    fn red_blue(conn: &RustConnection<DefaultStream>) -> u32 {
        let root = conn.setup().roots[0].root;
        let pixmap = conn.generate_id().unwrap();
        conn.create_pixmap(32, pixmap, root, 2, 1).unwrap();
        let gc = conn.generate_id().unwrap();
        conn.create_gc(gc, pixmap, &CreateGCAux::new()).unwrap();
        let pixels = [0, 0, 0xFF, 0xFF, 0xFF, 0, 0, 0xFF];
        conn.put_image(
            ImageFormat::Z_PIXMAP,
            pixmap,
            gc,
            2,
            1,
            0,
            0,
            0,
            32,
            &pixels,
        )
        .unwrap();
        let picture = conn.generate_id().unwrap();
        conn.render_create_picture(
            picture,
            pixmap,
            format_of(conn, 32),
            &CreatePictureAux::new(),
        )
        .unwrap();
        picture
    }

    #[test]
    fn source_offsets_pick_the_right_source_pixels() {
        let (path, display) = start_server_with_display("render-offsets");
        let conn = connect(&path);
        let target = window_picture(&conn);
        let source = red_blue(&conn);
        // Source x 1 (blue) lands at window x 3; source x 2 is outside: transparent.
        conn.render_composite(PictOp::SRC, source, 0u32, target, 1, 0, 0, 0, 3, 0, 2, 1)
            .unwrap();
        // Glyphs: the source position is relative to the first element (pen at 20, 30).
        let set = conn.generate_id().unwrap();
        conn.render_create_glyph_set(set, format_of(&conn, 8))
            .unwrap();
        let info = Glyphinfo {
            width: 2,
            height: 1,
            x: 0,
            y: 0,
            x_off: 2,
            y_off: 0,
        };
        conn.render_add_glyphs(set, &[1], &[info], &[0xFF, 0xFF, 0, 0])
            .unwrap();
        let items = [1, 0, 0, 0, 20, 0, 30, 0, 1, 0, 0, 0];
        conn.render_composite_glyphs8(PictOp::SRC, source, target, 0, set, 0, 0, &items)
            .unwrap();
        sync(&conn);
        assert_eq!(
            screen_pixel(&display, 13, 10),
            [0xFF, 0, 0],
            "blue from source x 1"
        );
        assert_eq!(
            screen_pixel(&display, 14, 10),
            [0, 0, 0],
            "transparent past the source"
        );
        assert_eq!(
            screen_pixel(&display, 30, 40),
            [0, 0, 0xFF],
            "glyph pixel 0: red"
        );
        assert_eq!(
            screen_pixel(&display, 31, 40),
            [0xFF, 0, 0],
            "glyph pixel 1: blue"
        );
    }

    #[test]
    fn the_clip_of_the_source_limits_the_composite() {
        let (path, display) = start_server_with_display("render-source-clip");
        let conn = connect(&path);
        let target = window_picture(&conn);
        let white = Rectangle {
            x: 0,
            y: 0,
            width: 40,
            height: 40,
        };
        conn.render_fill_rectangles(
            PictOp::SRC,
            target,
            colour(0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF),
            &[white],
        )
        .unwrap();
        let source = red_blue(&conn);
        let only_red = Rectangle {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
        conn.render_set_picture_clip_rectangles(source, 0, 0, &[only_red])
            .unwrap();
        conn.render_composite(PictOp::SRC, source, 0u32, target, 0, 0, 0, 0, 0, 0, 10, 10)
            .unwrap();
        sync(&conn);
        assert_eq!(
            screen_pixel(&display, 10, 10),
            [0, 0, 0xFF],
            "inside the source clip"
        );
        assert_eq!(
            screen_pixel(&display, 11, 10),
            [0xFF; 3],
            "outside: not composited"
        );
        assert_eq!(
            screen_pixel(&display, 15, 15),
            [0xFF; 3],
            "outside: not composited"
        );
    }

    #[test]
    fn a_window_that_includes_inferiors_is_read_with_its_children() {
        let conn = connect(&start_server("render-inferiors"));
        let root = conn.setup().roots[0].root;
        let (window, gc) = mapped_window(&conn, 10, 10, 20);
        conn.change_gc(gc, &ChangeGCAux::new().foreground(0xFF0000))
            .unwrap();
        let all = Rectangle {
            x: 0,
            y: 0,
            width: 20,
            height: 20,
        };
        conn.poly_fill_rectangle(window, gc, &[all]).unwrap();
        let screen = conn.generate_id().unwrap();
        let aux = CreatePictureAux::new().subwindowmode(SubwindowMode::INCLUDE_INFERIORS);
        conn.render_create_picture(screen, root, format_of(&conn, 24), &aux)
            .unwrap();
        let pixmap = conn.generate_id().unwrap();
        conn.create_pixmap(24, pixmap, root, 1, 1).unwrap();
        let copy = conn.generate_id().unwrap();
        conn.render_create_picture(copy, pixmap, format_of(&conn, 24), &CreatePictureAux::new())
            .unwrap();
        conn.render_composite(PictOp::SRC, screen, 0u32, copy, 15, 15, 0, 0, 0, 0, 1, 1)
            .unwrap();
        let image = conn
            .get_image(ImageFormat::Z_PIXMAP, pixmap, 0, 0, 1, 1, !0)
            .unwrap()
            .reply()
            .unwrap();
        assert_eq!(
            image.data,
            [0, 0, 0xFF, 0],
            "the red child, not the root background"
        );
    }

    /// Each round stores one 4 MiB glyph and frees it. If the same id twice were counted twice,
    /// the 64 MiB glyph budget would run out after about 16 rounds.
    #[test]
    fn glyphs_without_a_mask_format_match_the_masked_path() {
        let (path, display) = start_server_with_display("render-glyph-loop");
        let conn = connect(&path);
        let target = window_picture(&conn);
        // Vertical stripes, so that rows 2 to 6 and rows 17 to 21 have the same background.
        for (index, x) in (0..40).step_by(5).enumerate() {
            let level = (index as u16 + 1) * 0x1800;
            let stripe = Rectangle {
                x,
                y: 0,
                width: 5,
                height: 40,
            };
            conn.render_fill_rectangles(
                PictOp::SRC,
                target,
                colour(level, 0x4000, 0x8000, 0xFFFF),
                &[stripe],
            )
            .unwrap();
        }
        let ink = filled_picture(
            &conn,
            32,
            1,
            colour(0xC000, 0x2000, 0x6000, 0xC000),
            &CreatePictureAux::new().repeat(render::Repeat::NORMAL),
        );
        let set = conn.generate_id().unwrap();
        conn.render_create_glyph_set(set, format_of(&conn, 8))
            .unwrap();
        let info = Glyphinfo {
            width: 4,
            height: 3,
            x: 0,
            y: 0,
            x_off: 6,
            y_off: 0,
        };
        let image = [
            0x10, 0x80, 0xFF, 0x40, 0xFF, 0x00, 0x30, 0xA0, 0x55, 0xEE, 0x01, 0x99,
        ];
        conn.render_add_glyphs(set, &[3], &[info], &image).unwrap();
        // Six glyphs from pen x = -2, so the first one is cut by the left edge of the window.
        let line = |y: u8| vec![6, 0, 0, 0, 0xFE, 0xFF, y, 0, 3, 3, 3, 3, 3, 3, 0, 0];
        conn.render_composite_glyphs8(PictOp::OVER, ink, target, 0, set, 0, 0, &line(3))
            .unwrap();
        conn.render_composite_glyphs8(
            PictOp::OVER,
            ink,
            target,
            format_of(&conn, 8),
            set,
            0,
            0,
            &line(18),
        )
        .unwrap();
        sync(&conn);
        let rows = |top: usize| -> Vec<[u8; 3]> {
            (top..top + 3)
                .flat_map(|y| (10..50).map(move |x| (x, y)))
                .map(|(x, y)| screen_pixel(&display, x, y))
                .collect()
        };
        let looped = rows(13);
        assert_eq!(
            looped,
            rows(28),
            "the same pixels as one mask for all glyphs"
        );
        assert_ne!(looped[0], looped[39], "the glyphs drew something");
    }

    #[test]
    fn the_same_glyph_id_twice_counts_once() {
        let conn = connect(&start_server("render-glyph-bytes"));
        let set = conn.generate_id().unwrap();
        conn.render_create_glyph_set(set, format_of(&conn, 8))
            .unwrap();
        let info = Glyphinfo {
            width: 1024,
            height: 1024,
            x: 0,
            y: 0,
            x_off: 0,
            y_off: 0,
        };
        let image = vec![0x80u8; 2 * 1024 * 1024];
        for round in 0..24 {
            conn.render_add_glyphs(set, &[5, 5], &[info, info], &image)
                .unwrap()
                .check()
                .unwrap_or_else(|error| panic!("round {round}: {error:?}"));
            conn.render_free_glyphs(set, &[5]).unwrap().check().unwrap();
        }
    }

    /// Glyphs spread over the whole 16-bit range: the mask covers only what the destination
    /// shows, so the request is cheap and the server stays up.
    #[test]
    fn glyphs_far_apart_do_not_make_a_huge_mask() {
        let conn = connect(&start_server("render-far-glyphs"));
        let target = window_picture(&conn);
        let white = filled_picture(
            &conn,
            32,
            1,
            colour(0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF),
            &CreatePictureAux::new().repeat(render::Repeat::NORMAL),
        );
        let set = conn.generate_id().unwrap();
        conn.render_create_glyph_set(set, format_of(&conn, 8))
            .unwrap();
        let dot = Glyphinfo {
            width: 1,
            height: 1,
            x: 0,
            y: 0,
            x_off: 0,
            y_off: 0,
        };
        let jump = Glyphinfo {
            width: 0,
            height: 0,
            x: 0,
            y: 0,
            x_off: 32767,
            y_off: 32767,
        };
        conn.render_add_glyphs(set, &[1, 2], &[dot, jump], &[0xFF, 0, 0, 0])
            .unwrap();
        let mut ids = vec![1u8];
        ids.extend(std::iter::repeat_n(2u8, 250));
        ids.push(1);
        let mut items = Vec::new();
        for chunk in ids.chunks(252) {
            items.extend_from_slice(&[chunk.len() as u8, 0, 0, 0, 0, 0, 0, 0]);
            items.extend_from_slice(chunk);
            items.resize(items.len().div_ceil(4) * 4, 0);
        }
        let started = std::time::Instant::now();
        conn.render_composite_glyphs8(
            PictOp::OVER,
            white,
            target,
            format_of(&conn, 8),
            set,
            0,
            0,
            &items,
        )
        .unwrap()
        .check()
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// Empty glyphs cost memory too: one client cannot store an unlimited number.
    #[test]
    fn empty_glyphs_count_against_the_glyph_budget() {
        let conn = connect(&start_server("render-empty-glyphs"));
        let set = conn.generate_id().unwrap();
        conn.render_create_glyph_set(set, format_of(&conn, 8))
            .unwrap();
        let empty = Glyphinfo {
            width: 0,
            height: 0,
            x: 0,
            y: 0,
            x_off: 1,
            y_off: 0,
        };
        let batch = 200_000u32;
        let infos = vec![empty; batch as usize];
        let mut refused = None;
        for round in 0..4 {
            let ids: Vec<u32> = (round * batch..(round + 1) * batch).collect();
            if let Err(error) = conn
                .render_add_glyphs(set, &ids, &infos, &[])
                .unwrap()
                .check()
            {
                refused = Some((round, error));
                break;
            }
        }
        let (round, error) = refused.expect("the glyph budget must stop the client");
        assert!(round >= 1, "the first 200 000 empty glyphs fit");
        assert_eq!(error_kind::<()>(Err(error)), ErrorKind::Alloc);
    }

    #[test]
    fn errors_have_the_render_codes() {
        let conn = connect(&start_server("render-errors"));
        let base = conn
            .query_extension(b"RENDER")
            .unwrap()
            .reply()
            .unwrap()
            .first_error;
        let code = |result: Result<(), ReplyError>| match result {
            Err(ReplyError::X11Error(error)) => error.error_code,
            other => panic!("expected an error, got {other:?}"),
        };
        let root = conn.setup().roots[0].root;
        assert_eq!(
            code(conn.render_free_picture(0x1234).unwrap().check()),
            base + 1
        );
        let id = conn.generate_id().unwrap();
        assert_eq!(
            code(
                conn.render_create_picture(id, root, 0x99, &CreatePictureAux::new())
                    .unwrap()
                    .check()
            ),
            base,
            "bad format"
        );
        let wrong_depth = conn
            .render_create_picture(id, root, format_of(&conn, 32), &CreatePictureAux::new())
            .unwrap()
            .check();
        assert_eq!(error_kind(wrong_depth), ErrorKind::Match);
        conn.render_create_picture(id, root, format_of(&conn, 24), &CreatePictureAux::new())
            .unwrap()
            .check()
            .unwrap();
        let all = Rectangle {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        };
        let bad_op = conn
            .render_fill_rectangles(PictOp::from(20u8), id, colour(0, 0, 0, 0), &[all])
            .unwrap()
            .check();
        assert_eq!(error_kind(bad_op), ErrorKind::Value);
        let set = conn.generate_id().unwrap();
        conn.render_create_glyph_set(set, format_of(&conn, 8))
            .unwrap();
        assert_eq!(
            code(conn.render_free_glyphs(set, &[3]).unwrap().check()),
            base + 4
        );
        assert_eq!(
            code(conn.render_free_glyph_set(0x4321).unwrap().check()),
            base + 3
        );
        assert!(conn.get_input_focus().unwrap().reply().is_ok());
    }
}

mod move_tests {
    use super::*;

    fn move_to(conn: &RustConnection<DefaultStream>, window: u32, x: i32, y: i32) {
        conn.configure_window(window, &ConfigureWindowAux::new().x(x).y(y))
            .unwrap();
        sync(conn);
    }

    #[test]
    fn a_visible_window_moves_as_one_copy() {
        let (path, display) = start_server_with_display("move-copy");
        let conn = connect(&path);
        let (window, gc) = striped_window(&conn, 20, 0);
        sync(&conn);
        let (mut tiles, snapshot) = display.subscribe_with_snapshot();
        let mut page = PageModel::new(&display, &snapshot.1);
        move_to(&conn, window, 60, 0);
        page.apply_all(&mut tiles);
        assert_eq!(page.copies, 1);
        assert!(page.matches(&display), "the page shows the screen");
        assert_eq!(
            window_pixels(&conn, window, gc),
            display.read_region(60, 0, 200, 480),
            "the screen shows the window at its new place"
        );
        assert_eq!(
            display.read_region(20, 10, 1, 1),
            [0; 4],
            "the uncovered root is drawn"
        );
    }

    #[test]
    fn a_move_that_cannot_be_a_copy_still_draws_the_right_screen() {
        let (path, display) = start_server_with_display("move-fallback");
        let conn = connect(&path);
        let (window, _) = striped_window(&conn, 20, 0);
        let (above, _) = striped_window(&conn, 300, 0);
        sync(&conn);
        let (mut tiles, snapshot) = display.subscribe_with_snapshot();
        let mut page = PageModel::new(&display, &snapshot.1);
        // Under a higher window, partly off the screen, and with a resize: no copy.
        move_to(&conn, window, 200, 0);
        move_to(&conn, above, 500, 0);
        let aux = ConfigureWindowAux::new().x(10).width(150);
        conn.configure_window(window, &aux).unwrap();
        sync(&conn);
        page.apply_all(&mut tiles);
        assert_eq!(page.copies, 0);
        assert!(page.matches(&display), "the page shows the screen");
    }

    /// An InputOnly window paints nothing: its move must not move screen pixels.
    #[test]
    fn an_input_only_window_moves_without_moving_pixels() {
        let (path, display) = start_server_with_display("move-input-only");
        let conn = connect(&path);
        let (parent, gc) = striped_window(&conn, 20, 0);
        let catcher = conn.generate_id().unwrap();
        let aux = CreateWindowAux::new();
        conn.create_window(
            0,
            catcher,
            parent,
            10,
            10,
            50,
            50,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &aux,
        )
        .unwrap();
        conn.map_window(catcher).unwrap();
        sync(&conn);
        move_to(&conn, catcher, 60, 70);
        assert_eq!(
            display.read_region(20, 0, 200, 480),
            window_pixels(&conn, parent, gc),
            "the screen still shows the parent"
        );
    }

    /// A held scroll copy reaches the page at the end of its burst, also when a ConfigureWindow
    /// that moves nothing comes in between and no request asks for a reply.
    #[test]
    fn a_held_scroll_is_sent_after_a_configure_that_moves_nothing() {
        let (path, display) = start_server_with_display("move-noop");
        let conn = connect(&path);
        let (window, gc) = striped_window(&conn, 20, 0);
        sync(&conn);
        let (mut tiles, snapshot) = display.subscribe_with_snapshot();
        let mut page = PageModel::new(&display, &snapshot.1);
        conn.copy_area(window, window, gc, 0, 16, 0, 0, 200, 464)
            .unwrap();
        conn.configure_window(window, &ConfigureWindowAux::new().x(20).y(0))
            .unwrap();
        conn.flush().unwrap();
        thread::sleep(Duration::from_millis(200));
        page.apply_all(&mut tiles);
        assert_eq!(page.copies, 1, "the scroll reached the page");
        assert!(page.matches(&display));
    }

    #[test]
    fn a_framed_window_moves_with_its_frame() {
        let (path, display) = start_server_wm("move-frame");
        let conn = connect(&path);
        let (window, _) = mapped_window(&conn, 40, 40, 100);
        sync(&conn);
        let (mut tiles, snapshot) = display.subscribe_with_snapshot();
        let mut page = PageModel::new(&display, &snapshot.1);
        move_to(&conn, window, 120, 90);
        page.apply_all(&mut tiles);
        assert!(page.matches(&display), "the page shows the screen");
        assert_eq!(page.copies, 1, "window and frame move as one copy");
    }
}

mod shm_fd_tests {
    use super::*;
    use std::os::fd::OwnedFd;
    use x11rb::protocol::shm::ConnectionExt as _;

    /// A file with `bytes` on tmpfs (the server takes only files in memory). Its name is removed
    /// at once; the open file stays until the last descriptor closes.
    fn segment_file(name: &str, bytes: &[u8]) -> std::fs::File {
        let path = PathBuf::from(format!("/dev/shm/xsrv-test-{name}-{}", std::process::id()));
        let mut file = std::fs::File::options()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.write_all(bytes).unwrap();
        std::fs::remove_file(path).unwrap();
        file
    }

    #[test]
    fn shm_attach_fd_draws_from_a_passed_file() {
        let (path, display) = start_server_with_display("shm-fd");
        let conn = connect(&path);
        let mut bytes = vec![0u8; 4096];
        bytes[64..80].copy_from_slice(&[1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0, 10, 11, 12, 0]);
        let file = segment_file("draw", &bytes);
        let (window, gc) = mapped_window(&conn, 100, 100, 20);
        let seg = conn.generate_id().unwrap();
        conn.shm_attach_fd(seg, OwnedFd::from(file), true)
            .unwrap()
            .check()
            .unwrap();
        conn.shm_put_image(window, gc, 2, 2, 0, 0, 2, 2, 5, 6, 24, 2, false, seg, 64)
            .unwrap()
            .check()
            .unwrap();
        sync(&conn);
        assert_eq!(screen_pixel(&display, 105, 106), [1, 2, 3]);
        assert_eq!(screen_pixel(&display, 106, 107), [10, 11, 12]);
        // The right column only: both rows come from one read, cut at the right places.
        conn.shm_put_image(window, gc, 2, 2, 1, 0, 1, 2, 10, 10, 24, 2, false, seg, 64)
            .unwrap()
            .check()
            .unwrap();
        sync(&conn);
        assert_eq!(screen_pixel(&display, 110, 110), [4, 5, 6]);
        assert_eq!(screen_pixel(&display, 110, 111), [10, 11, 12]);
        conn.shm_detach(seg).unwrap().check().unwrap();
    }

    /// The client may shrink its file after the attach: the server reads with pread and answers
    /// BadValue, where a mapped file would have crashed it with SIGBUS.
    #[test]
    fn a_shrunk_file_gives_bad_value_and_no_crash() {
        let path = start_server("shm-fd-shrink");
        let conn = connect(&path);
        let file = segment_file("shrink", &[7u8; 4096]);
        let (window, gc) = mapped_window(&conn, 10, 10, 20);
        let seg = conn.generate_id().unwrap();
        conn.shm_attach_fd(seg, OwnedFd::from(file.try_clone().unwrap()), true)
            .unwrap()
            .check()
            .unwrap();
        file.set_len(0).unwrap();
        let failed = conn
            .shm_put_image(window, gc, 4, 4, 0, 0, 4, 4, 0, 0, 24, 2, false, seg, 0)
            .unwrap()
            .check();
        assert_eq!(error_kind(failed), ErrorKind::Value);
        assert!(
            conn.get_input_focus().unwrap().reply().is_ok(),
            "the server is alive"
        );
    }

    /// A malformed AttachFd uses up its own descriptor, so the next AttachFd gets its own file.
    #[test]
    fn a_malformed_attach_fd_does_not_shift_the_descriptors() {
        use x11rb::connection::RequestConnection;
        let (path, display) = start_server_with_display("shm-fd-shift");
        let conn = connect(&path);
        let file_a = segment_file("shift-a", &[0x11; 64]);
        let file_b = segment_file("shift-b", &[0x22; 64]);
        let (window, gc) = mapped_window(&conn, 10, 10, 20);

        let seg = conn.generate_id().unwrap();
        let bad = raw_attach_fd(&conn, seg, 1); // one word too many
        let refused = conn
            .send_request_without_reply(&[std::io::IoSlice::new(&bad)], vec![OwnedFd::from(file_a)])
            .unwrap()
            .check();
        assert_eq!(error_kind(refused), ErrorKind::Length);

        conn.shm_attach_fd(seg, OwnedFd::from(file_b), true)
            .unwrap()
            .check()
            .unwrap();
        conn.shm_put_image(window, gc, 4, 4, 0, 0, 4, 4, 2, 2, 24, 2, false, seg, 0)
            .unwrap()
            .check()
            .unwrap();
        sync(&conn);
        assert_eq!(screen_pixel(&display, 12, 12), [0x22, 0x22, 0x22], "file B");
    }

    /// The id of MIT-SHM and a raw ShmAttachFd request (x11rb always sends a descriptor).
    fn raw_attach_fd(conn: &RustConnection, seg: u32, extra_words: u16) -> Vec<u8> {
        use x11rb::connection::RequestConnection;
        let major = conn
            .extension_information(x11rb::protocol::shm::X11_EXTENSION_NAME)
            .unwrap()
            .expect("MIT-SHM")
            .major_opcode;
        let mut request = vec![major, 6];
        request.extend_from_slice(&(3 + extra_words).to_ne_bytes());
        request.extend_from_slice(&seg.to_ne_bytes());
        request.extend_from_slice(&[1, 0, 0, 0]);
        request.resize(request.len() + 4 * usize::from(extra_words), 0);
        request
    }

    #[test]
    fn attach_fd_without_a_descriptor_or_with_an_empty_file_is_bad_match() {
        use x11rb::connection::RequestConnection;
        let conn = connect(&start_server("shm-fd-match"));
        let seg = conn.generate_id().unwrap();
        let request = raw_attach_fd(&conn, seg, 0);
        let refused = conn
            .send_request_without_reply(&[std::io::IoSlice::new(&request)], vec![])
            .unwrap()
            .check();
        assert_eq!(error_kind(refused), ErrorKind::Match, "no descriptor");
        let file = segment_file("empty", &[]);
        let refused = conn
            .shm_attach_fd(seg, OwnedFd::from(file), true)
            .unwrap()
            .check();
        assert_eq!(error_kind(refused), ErrorKind::Match, "empty file");
    }

    #[test]
    fn too_many_waiting_descriptors_end_the_connection() {
        use x11rb::connection::RequestConnection;
        let conn = connect(&start_server("shm-fd-flood"));
        let no_operation = [127u8, 0, 1, 0];
        for _ in 0..17 {
            let fd = OwnedFd::from(std::fs::File::open("/dev/null").unwrap());
            let _ =
                conn.send_request_without_reply(&[std::io::IoSlice::new(&no_operation)], vec![fd]);
        }
        assert!(
            conn.get_input_focus()
                .map_or(true, |cookie| cookie.reply().is_err()),
            "the server closed the connection"
        );
    }

    #[test]
    fn create_segment_is_refused_as_not_implemented() {
        let conn = connect(&start_server("shm-create"));
        let seg = conn.generate_id().unwrap();
        let refused = conn.shm_create_segment(seg, 4096, false).unwrap().reply();
        assert_eq!(error_kind(refused), ErrorKind::Implementation);
    }
}
