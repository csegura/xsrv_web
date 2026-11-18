// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! XKEYBOARD over a real socket, with an x11rb client.
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tokio::runtime::Builder;
use tokio::task::LocalSet;
use wire::Input;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::errors::ReplyError;
use x11rb::protocol::xkb::{
    self, ConnectionExt as _, EventType, MapPart, NameDetail, SelectEventsAux, ID,
};
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::protocol::Event;
use x11rb::rust_connection::{DefaultStream, RustConnection};
use xsrv_server::display::Display;
use xsrv_server::x11;

const CORE_KBD: u16 = 0x100;
const SEMICOLON: u8 = 39 + 8;
const KEY_A: u8 = 30 + 8;
const SHIFT_L: u8 = 42 + 8;

fn start(name: &str, xkb: bool) -> (PathBuf, Display) {
    let dir = std::env::temp_dir().join(format!("xsrv-xkb-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let display = Display::new(x11::SCREEN_WIDTH, x11::SCREEN_HEIGHT);
    let (ready_tx, ready_rx) = mpsc::channel();
    let (server_path, server_display) = (path.clone(), display.clone());
    let options = x11::Options {
        xkb,
        ..x11::Options::default()
    };
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
    let conn = RustConnection::connect_to_stream(stream, 0).unwrap();
    if conn
        .extension_information(xkb::X11_EXTENSION_NAME)
        .unwrap()
        .is_some()
    {
        let reply = conn.xkb_use_extension(1, 0).unwrap().reply().unwrap();
        assert!(reply.supported);
    }
    conn
}

fn full_map(conn: &RustConnection) -> xkb::GetMapReply {
    let all = MapPart::KEY_TYPES
        | MapPart::KEY_SYMS
        | MapPart::MODIFIER_MAP
        | MapPart::EXPLICIT_COMPONENTS
        | MapPart::KEY_ACTIONS
        | MapPart::KEY_BEHAVIORS
        | MapPart::VIRTUAL_MODS
        | MapPart::VIRTUAL_MOD_MAP;
    conn.xkb_get_map(
        CORE_KBD,
        all,
        MapPart::from(0u16),
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0u16.into(),
        0,
        0,
        0,
        0,
        0,
        0,
    )
    .unwrap()
    .reply()
    .unwrap()
}

fn symbols_of(map: &xkb::GetMapReply, keycode: u8) -> Vec<u32> {
    let syms = map.map.syms_rtrn.as_ref().unwrap();
    syms[usize::from(keycode - map.first_key_sym)].syms.clone()
}

fn atom_name(conn: &RustConnection, atom: u32) -> String {
    let reply = conn.get_atom_name(atom).unwrap().reply().unwrap();
    String::from_utf8(reply.name).unwrap()
}

#[test]
fn the_extension_is_off_unless_asked_for() {
    let (path, _display) = start("off", false);
    let conn = connect(&path);
    assert!(conn
        .extension_information(xkb::X11_EXTENSION_NAME)
        .unwrap()
        .is_none());
    let names = conn.list_extensions().unwrap().reply().unwrap().names;
    assert!(names.iter().all(|name| name.name != b"XKEYBOARD"));
}

#[test]
fn the_keymap_says_what_the_core_mapping_says() {
    let (path, _display) = start("map", true);
    let conn = connect(&path);
    let map = full_map(&conn);
    assert_eq!((map.min_key_code, map.max_key_code), (8, 255));
    assert_eq!(map.first_key_sym, 8);
    assert_eq!(map.n_key_syms, 248);
    let types = map.map.types_rtrn.as_ref().unwrap();
    assert_eq!(types.len(), usize::from(map.total_types));

    let core = conn.get_keyboard_mapping(8, 248).unwrap().reply().unwrap();
    let per = usize::from(core.keysyms_per_keycode);
    for keycode in [KEY_A, SEMICOLON, SHIFT_L, 9u8] {
        let index = usize::from(keycode - 8) * per;
        // The core map repeats a symbol for Shift where XKB has a one-level key.
        let mut core_syms: Vec<u32> = core.keysyms[index..index + per]
            .iter()
            .copied()
            .filter(|sym| *sym != 0)
            .collect();
        core_syms.dedup();
        assert_eq!(symbols_of(&map, keycode), core_syms, "keycode {keycode}");
    }

    let actions = map.map.key_actions.as_ref().unwrap();
    assert_eq!(actions.acts_rtrn_count.len(), 248);
    assert_eq!(actions.acts_rtrn_count[usize::from(SHIFT_L - 8)], 1);
    assert_eq!(actions.acts_rtrn_count[usize::from(KEY_A - 8)], 0);
    let modmap = map.map.modmap_rtrn.as_ref().unwrap();
    assert!(modmap
        .iter()
        .any(|entry| entry.keycode == SHIFT_L && u16::from(entry.mods) == 1));
    assert_eq!(map.map.vmods_rtrn.as_ref().unwrap().len(), 4);

    // A partial request: one key of symbols only.
    let one = conn
        .xkb_get_map(
            CORE_KBD,
            MapPart::from(0u16),
            MapPart::KEY_SYMS,
            0,
            0,
            KEY_A,
            1,
            0,
            0,
            0,
            0,
            0u16.into(),
            0,
            0,
            0,
            0,
            0,
            0,
        )
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!((one.first_key_sym, one.n_key_syms), (KEY_A, 1));
    assert_eq!(one.map.syms_rtrn.unwrap()[0].syms, [0x61, 0x41]);
    assert!(one.map.types_rtrn.is_none());
}

#[test]
fn names_controls_and_indicators_describe_one_keyboard() {
    let (path, _display) = start("names", true);
    let conn = connect(&path);
    let names = conn
        .xkb_get_names(CORE_KBD, NameDetail::from(0x3FFFu32))
        .unwrap()
        .reply()
        .unwrap();
    let key_names = names.value_list.key_names.as_ref().unwrap();
    assert_eq!(key_names.len(), 248);
    assert_eq!(&key_names[usize::from(KEY_A - 8)].name, b"AC01");
    let groups = names.value_list.groups.as_ref().unwrap();
    assert_eq!(atom_name(&conn, groups[0]), "English (US)");
    let symbols = names.value_list.symbols_name.unwrap();
    assert_eq!(atom_name(&conn, symbols), "pc+us+inet(evdev)");
    let type_names = names.value_list.type_names.as_ref().unwrap();
    assert_eq!(atom_name(&conn, type_names[0]), "ONE_LEVEL");

    let controls = conn.xkb_get_controls(CORE_KBD).unwrap().reply().unwrap();
    assert_eq!(controls.num_groups, 1);
    let device = conn
        .xkb_get_device_info(CORE_KBD, 0u16.into(), false, 0, 0, 0u16.into(), 0u16)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(device.device_id, 3);
    let indicators = conn
        .xkb_get_indicator_map(CORE_KBD, 0b111)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(indicators.maps.len(), 3);
    let compat = conn
        .xkb_get_compat_map(CORE_KBD, 0u8.into(), true, 0, 0)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(compat.si_rtrn.len(), usize::from(compat.n_total_si));
    assert!(compat.n_total_si > 0);
}

#[test]
fn another_device_is_refused() {
    let (path, _display) = start("device", true);
    let conn = connect(&path);
    let result = conn.xkb_get_state(7).unwrap().reply();
    assert!(
        matches!(result, Err(ReplyError::X11Error(ref error)) if error.error_code == 139),
        "{result:?}"
    );
    assert!(conn
        .xkb_get_state(ID::USE_CORE_KBD.into())
        .unwrap()
        .reply()
        .is_ok());
}

fn select_all(conn: &RustConnection, events: EventType) {
    conn.xkb_select_events(
        CORE_KBD,
        0u16.into(),
        events,
        0u16.into(),
        0u16.into(),
        &SelectEventsAux::new(),
    )
    .unwrap();
    conn.flush().unwrap();
}

fn next_xkb_event(conn: &RustConnection) -> Event {
    loop {
        if let event @ (Event::XkbNewKeyboardNotify(_)
        | Event::XkbMapNotify(_)
        | Event::XkbNamesNotify(_)
        | Event::XkbStateNotify(_)
        | Event::XkbIndicatorStateNotify(_)) = conn.wait_for_event().unwrap()
        {
            return event;
        }
    }
}

#[test]
fn a_layout_switch_tells_xkb_clients_to_reload() {
    let (path, display) = start("switch", true);
    let conn = connect(&path);
    select_all(
        &conn,
        EventType::NEW_KEYBOARD_NOTIFY | EventType::MAP_NOTIFY | EventType::NAMES_NOTIFY,
    );
    conn.get_input_focus().unwrap().reply().unwrap();

    display.send_input(Input::Layout { layout: 1 });
    let first = next_xkb_event(&conn);
    let second = next_xkb_event(&conn);
    let third = next_xkb_event(&conn);
    assert!(matches!(first, Event::XkbNewKeyboardNotify(_)), "{first:?}");
    let Event::XkbNamesNotify(names_event) = second else {
        panic!("{second:?}");
    };
    assert_ne!(
        u32::from(names_event.changed) & u32::from(NameDetail::GROUP_NAMES),
        0
    );
    let Event::XkbMapNotify(map_event) = third else {
        panic!("{third:?}");
    };
    assert_eq!(map_event.n_key_syms, 248);

    let map = full_map(&conn);
    assert_eq!(
        symbols_of(&map, SEMICOLON),
        [0xF1, 0xD1],
        "es: n with tilde"
    );
    let names = conn
        .xkb_get_names(CORE_KBD, NameDetail::GROUP_NAMES)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(
        atom_name(&conn, names.value_list.groups.unwrap()[0]),
        "Spanish"
    );

    let rules = conn
        .intern_atom(true, b"_XKB_RULES_NAMES")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    let root = conn.setup().roots[0].root;
    let property = conn
        .get_property(false, root, rules, 31u32, 0, 64)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(property.value, b"evdev\0pc105\0es\0\0\0");
}

#[test]
fn modifier_and_lock_changes_send_state_events() {
    let (path, display) = start("state", true);
    let conn = connect(&path);
    select_all(
        &conn,
        EventType::STATE_NOTIFY | EventType::INDICATOR_STATE_NOTIFY,
    );
    conn.get_input_focus().unwrap().reply().unwrap();

    display.send_input(Input::Key {
        keycode: SHIFT_L,
        pressed: true,
    });
    let Event::XkbStateNotify(state) = next_xkb_event(&conn) else {
        panic!("a state event first");
    };
    assert_eq!(u16::from(state.mods), 1, "Shift");
    assert_eq!(state.keycode, SHIFT_L);
    display.send_input(Input::Key {
        keycode: SHIFT_L,
        pressed: false,
    });
    let Event::XkbStateNotify(state) = next_xkb_event(&conn) else {
        panic!("a state event for the release");
    };
    assert_eq!(u16::from(state.mods), 0);

    display.send_input(Input::Locks {
        caps: true,
        num: false,
    });
    let mut saw_lights = false;
    let mut saw_lock = false;
    while !(saw_lights && saw_lock) {
        match next_xkb_event(&conn) {
            Event::XkbStateNotify(state) => {
                assert_eq!(u16::from(state.locked_mods), 2);
                saw_lock = true;
            }
            Event::XkbIndicatorStateNotify(lights) => {
                assert_eq!(lights.state, 1, "Caps Lock light");
                saw_lights = true;
            }
            other => panic!("{other:?}"),
        }
    }
    let now = conn.xkb_get_state(CORE_KBD).unwrap().reply().unwrap();
    assert_eq!(u16::from(now.locked_mods), 2);
    let lights = conn
        .xkb_get_indicator_state(CORE_KBD)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!(lights.state, 1);
}

#[test]
fn unsupported_requests_fail_cleanly() {
    let (path, _display) = start("unsupported", true);
    let conn = connect(&path);
    let result = conn.xkb_list_components(CORE_KBD, 10).unwrap().reply();
    assert!(matches!(result, Err(ReplyError::X11Error(_))), "{result:?}");
    // The connection still works.
    conn.xkb_get_state(CORE_KBD).unwrap().reply().unwrap();
}
