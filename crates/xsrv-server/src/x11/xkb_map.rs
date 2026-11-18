// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! The server keymap as XKEYBOARD data: key types, the symbols and actions of each key, the
//! modifier maps, key names and indicators. It is built from `keymap.rs`, so the core view
//! (GetKeyboardMapping) and the XKB view always describe the same keys.
//!
//! One group per key. The AltGr layouts use four-level types, where Mod5 (AltGr) gives the
//! third and fourth level, as xkeyboard-config does.
use super::keymap::{self, Layout, MAX_KEYCODE, MIN_KEYCODE};
use x11rb_protocol::protocol::xkb::{
    Action, BoolCtrl, IMFlag, IMGroupsWhich, IMModsWhich, IndicatorMap, KTMapEntry, KeyModMap,
    KeySymMap, KeyType, KeyVModMap, SASetMods, SAType, SIAction, SymInterpret, SymInterpretMatch,
    SA,
};

pub const DEVICE_ID: u8 = 3;
pub const KEY_COUNT: u8 = MAX_KEYCODE - MIN_KEYCODE + 1;

const SHIFT: u8 = keymap::SHIFT_MASK as u8;
const LOCK: u8 = keymap::LOCK_MASK as u8;
const MOD1: u8 = keymap::MOD1_MASK as u8;
const MOD2: u8 = keymap::MOD2_MASK as u8;
const MOD4: u8 = keymap::MOD4_MASK as u8;
const MOD5: u8 = keymap::MOD5_MASK as u8;

const ISO_LEVEL3_SHIFT: u32 = 0xFE03;

struct TypeDef {
    name: &'static str,
    mask: u8,
    /// (modifiers, level) pairs; any other combination of the mask gives level 0.
    entries: &'static [(u8, u8)],
    levels: &'static [&'static str],
}

const ONE_LEVEL: u8 = 0;
const TWO_LEVEL: u8 = 1;
const ALPHABETIC: u8 = 2;
const KEYPAD: u8 = 3;
const FOUR_LEVEL: u8 = 4;
const FOUR_LEVEL_ALPHABETIC: u8 = 5;

const TYPES: [TypeDef; 6] = [
    TypeDef {
        name: "ONE_LEVEL",
        mask: 0,
        entries: &[],
        levels: &["Any"],
    },
    TypeDef {
        name: "TWO_LEVEL",
        mask: SHIFT,
        entries: &[(SHIFT, 1)],
        levels: &["Base", "Shift"],
    },
    TypeDef {
        name: "ALPHABETIC",
        mask: SHIFT | LOCK,
        entries: &[(SHIFT, 1), (LOCK, 1)],
        levels: &["Base", "Caps"],
    },
    TypeDef {
        name: "KEYPAD",
        mask: SHIFT | MOD2,
        entries: &[(SHIFT, 1), (MOD2, 1)],
        levels: &["Base", "Number"],
    },
    TypeDef {
        name: "FOUR_LEVEL",
        mask: SHIFT | MOD5,
        entries: &[(SHIFT, 1), (MOD5, 2), (SHIFT | MOD5, 3)],
        levels: &["Base", "Shift", "Alt Base", "Shift Alt"],
    },
    TypeDef {
        name: "FOUR_LEVEL_ALPHABETIC",
        mask: SHIFT | LOCK | MOD5,
        entries: &[
            (SHIFT, 1),
            (LOCK, 1),
            (MOD5, 2),
            (SHIFT | MOD5, 3),
            (LOCK | MOD5, 3),
            (SHIFT | LOCK | MOD5, 2),
        ],
        levels: &["Base", "Caps", "Alt Base", "Shift Alt"],
    },
];

pub fn type_count() -> u8 {
    TYPES.len() as u8
}

pub fn key_types() -> Vec<KeyType> {
    TYPES
        .iter()
        .map(|def| KeyType {
            mods_mask: def.mask.into(),
            mods_mods: def.mask.into(),
            mods_vmods: 0u16.into(),
            num_levels: def.levels.len() as u8,
            has_preserve: false,
            map: def
                .entries
                .iter()
                .map(|&(mods, level)| KTMapEntry {
                    active: true,
                    mods_mask: mods.into(),
                    level,
                    mods_mods: mods.into(),
                    mods_vmods: 0u16.into(),
                })
                .collect(),
            preserve: Vec::new(),
        })
        .collect()
}

pub fn type_names() -> impl Iterator<Item = &'static str> {
    TYPES.iter().map(|def| def.name)
}

/// The level names of each type, in type order.
pub fn level_names() -> impl Iterator<Item = &'static [&'static str]> {
    TYPES.iter().map(|def| def.levels)
}

/// A Latin-1 lower-case letter and its upper case.
fn is_letter_pair(lower: u32, upper: u32) -> bool {
    let latin = (0x61..=0x7A).contains(&lower) || ((0xE0..=0xFE).contains(&lower) && lower != 0xF7);
    latin && upper == lower - 0x20
}

fn is_keypad(keysym: u32) -> bool {
    (0xFF80..=0xFFBD).contains(&keysym)
}

/// The type and the symbols (one per level) of a key, or `None` for a key without symbols.
fn key_symbols(layout: Layout, keycode: u8) -> Option<(u8, Vec<u32>)> {
    if keymap::held_modifier(layout, keycode) == Some(keymap::MOD5_MASK) {
        return Some((ONE_LEVEL, vec![ISO_LEVEL3_SHIFT]));
    }
    let [plain, shifted, alt, shift_alt] = keymap::keysyms(layout, keycode);
    if plain == 0 {
        return None;
    }
    if alt != 0 || shift_alt != 0 {
        // As in the core protocol, a missing Shift symbol repeats the one without Shift.
        let shift_alt = if shift_alt == 0 { alt } else { shift_alt };
        let kind = if is_letter_pair(plain, shifted) {
            FOUR_LEVEL_ALPHABETIC
        } else {
            FOUR_LEVEL
        };
        return Some((kind, vec![plain, shifted, alt, shift_alt]));
    }
    if shifted == 0 || shifted == plain {
        return Some((ONE_LEVEL, vec![plain]));
    }
    let kind = if is_keypad(shifted) {
        KEYPAD
    } else if is_letter_pair(plain, shifted) {
        ALPHABETIC
    } else {
        TWO_LEVEL
    };
    Some((kind, vec![plain, shifted]))
}

pub fn sym_map(layout: Layout, keycode: u8) -> KeySymMap {
    match key_symbols(layout, keycode) {
        Some((kind, syms)) => KeySymMap {
            kt_index: [kind, 0, 0, 0],
            group_info: 1,
            width: syms.len() as u8,
            syms,
        },
        None => KeySymMap {
            kt_index: [0; 4],
            group_info: 0,
            width: 0,
            syms: Vec::new(),
        },
    }
}

/// The modifier bits of a key in the modifier map (0 for most keys).
pub fn modmap(layout: Layout, keycode: u8) -> u8 {
    let mapping = keymap::modifier_mapping(layout);
    mapping
        .iter()
        .enumerate()
        .filter(|(_, key)| **key == keycode && keycode != 0)
        .fold(0, |mods, (slot, _)| {
            mods | 1 << (slot / usize::from(keymap::KEYCODES_PER_MODIFIER))
        })
}

pub fn modmap_keys(layout: Layout) -> Vec<KeyModMap> {
    (MIN_KEYCODE..=MAX_KEYCODE)
        .filter_map(|keycode| {
            let mods = modmap(layout, keycode);
            (mods != 0).then(|| KeyModMap {
                keycode,
                mods: mods.into(),
            })
        })
        .collect()
}

/// The actions of a key, one per level, or none. Modifier keys set their modifier while they are
/// held; Caps Lock and Num Lock lock it.
pub fn actions(layout: Layout, keycode: u8) -> Vec<Action> {
    let mods = modmap(layout, keycode);
    let width = sym_map(layout, keycode).syms.len();
    if mods == 0 || width == 0 {
        return Vec::new();
    }
    let (type_, flags) = if mods & (LOCK | MOD2) != 0 {
        (SAType::LOCK_MODS, SA::USE_MOD_MAP_MODS)
    } else {
        (SAType::SET_MODS, SA::USE_MOD_MAP_MODS | SA::CLEAR_LOCKS)
    };
    let action = SASetMods {
        type_,
        flags,
        mask: 0u8.into(),
        real_mods: 0u8.into(),
        vmods_high: 0u8.into(),
        vmods_low: 0u8.into(),
    };
    vec![Action::from(action); width]
}

/// The virtual modifiers, by index: their names and the real modifiers they stand for.
pub const VIRTUAL_MODS: [&str; 4] = ["NumLock", "Alt", "LevelThree", "Super"];

pub fn virtual_mod_real(layout: Layout, index: usize) -> u8 {
    let real = [MOD2, MOD1, MOD5, MOD4][index];
    let used = (MIN_KEYCODE..=MAX_KEYCODE).any(|key| modmap(layout, key) & real != 0);
    if used {
        real
    } else {
        0
    }
}

pub fn virtual_mods_mask() -> u16 {
    (1 << VIRTUAL_MODS.len()) - 1
}

pub fn vmodmap_keys(layout: Layout) -> Vec<KeyVModMap> {
    (MIN_KEYCODE..=MAX_KEYCODE)
        .filter_map(|keycode| {
            let mods = modmap(layout, keycode);
            let vmods = [MOD2, MOD1, MOD5, MOD4]
                .iter()
                .enumerate()
                .filter(|(_, real)| mods & **real != 0)
                .fold(0u16, |all, (index, _)| all | 1 << index);
            (vmods != 0).then(|| KeyVModMap {
                keycode,
                vmods: vmods.into(),
            })
        })
        .collect()
}

const NO_VIRTUAL_MOD: u8 = 0xFF;

/// The symbol interpretations of the compatibility map, as in xkeyboard-config's basic set: the
/// same actions as the key map has, for clients that rebuild a keymap from them (xkbcomp).
pub fn sym_interprets() -> Vec<SymInterpret> {
    let set = SA::USE_MOD_MAP_MODS | SA::CLEAR_LOCKS;
    let lock = SA::USE_MOD_MAP_MODS;
    let entries: [(u32, SymInterpretMatch, u8, SAType, SA); 8] = [
        (
            0xFFE5,
            SymInterpretMatch::ANY_OF_OR_NONE,
            NO_VIRTUAL_MOD,
            SAType::LOCK_MODS,
            lock,
        ),
        (
            0xFF7F,
            SymInterpretMatch::ANY_OF_OR_NONE,
            0,
            SAType::LOCK_MODS,
            lock,
        ),
        (
            ISO_LEVEL3_SHIFT,
            SymInterpretMatch::ANY_OF,
            2,
            SAType::SET_MODS,
            set,
        ),
        (0xFFE9, SymInterpretMatch::ANY_OF, 1, SAType::SET_MODS, set),
        (0xFFEA, SymInterpretMatch::ANY_OF, 1, SAType::SET_MODS, set),
        (0xFFEB, SymInterpretMatch::ANY_OF, 3, SAType::SET_MODS, set),
        (0xFFEC, SymInterpretMatch::ANY_OF, 3, SAType::SET_MODS, set),
        (
            0,
            SymInterpretMatch::ANY_OF,
            NO_VIRTUAL_MOD,
            SAType::SET_MODS,
            set,
        ),
    ];
    entries
        .into_iter()
        .map(|(sym, op, virtual_mod, type_, flags)| SymInterpret {
            sym,
            mods: 0xFFu8.into(),
            match_: u8::from(op),
            virtual_mod: virtual_mod.into(),
            flags: 0,
            action: SIAction {
                type_,
                data: [u8::from(flags), 0, 0, 0, 0, 0, 0],
            },
        })
        .collect()
}

/// Every key repeats, except the modifier keys. One bit per keycode.
pub fn per_key_repeat(layout: Layout) -> [u8; 32] {
    let mut bits = [0u8; 32];
    for keycode in MIN_KEYCODE..=MAX_KEYCODE {
        if sym_map(layout, keycode).width > 0 && modmap(layout, keycode) == 0 {
            bits[usize::from(keycode / 8)] |= 1 << (keycode % 8);
        }
    }
    bits
}

pub fn group_name(layout: Layout) -> &'static str {
    match layout {
        Layout::Us => "English (US)",
        Layout::Es => "Spanish",
        Layout::Gb => "English (UK)",
        Layout::De => "German",
    }
}

pub fn symbols_name(layout: Layout) -> &'static str {
    match layout {
        Layout::Us => "pc+us+inet(evdev)",
        Layout::Es => "pc+es+inet(evdev)",
        Layout::Gb => "pc+gb+inet(evdev)",
        Layout::De => "pc+de+inet(evdev)",
    }
}

/// Indicators: Caps Lock, Num Lock, Scroll Lock (bits 0 to 2).
pub const INDICATORS: [&str; 3] = ["Caps Lock", "Num Lock", "Scroll Lock"];
pub const INDICATOR_MASK: u32 = 0b111;

pub fn indicator_map(index: usize) -> IndicatorMap {
    let mods = match index {
        0 => LOCK,
        1 => MOD2,
        _ => 0,
    };
    IndicatorMap {
        flags: IMFlag::from(0u8),
        which_groups: IMGroupsWhich::from(0u8),
        groups: 0u8.into(),
        which_mods: if mods == 0 {
            IMModsWhich::from(0u8)
        } else {
            IMModsWhich::USE_LOCKED
        },
        mods: mods.into(),
        real_mods: mods.into(),
        vmods: 0u16.into(),
        ctrls: BoolCtrl::from(0u32),
    }
}

/// The indicators that are on for these locked modifiers.
pub fn indicator_state(locked: u8) -> u32 {
    u32::from(locked & LOCK != 0) | u32::from(locked & MOD2 != 0) << 1
}

const KEY_NAMES: &[(u8, &str)] = &[
    (9, "ESC"),
    (22, "BKSP"),
    (23, "TAB"),
    (36, "RTRN"),
    (37, "LCTL"),
    (49, "TLDE"),
    (50, "LFSH"),
    (51, "BKSL"),
    (62, "RTSH"),
    (63, "KPMU"),
    (64, "LALT"),
    (65, "SPCE"),
    (66, "CAPS"),
    (77, "NMLK"),
    (78, "SCLK"),
    (79, "KP7"),
    (80, "KP8"),
    (81, "KP9"),
    (82, "KPSU"),
    (83, "KP4"),
    (84, "KP5"),
    (85, "KP6"),
    (86, "KPAD"),
    (87, "KP1"),
    (88, "KP2"),
    (89, "KP3"),
    (90, "KP0"),
    (91, "KPDL"),
    (92, "LVL3"),
    (94, "LSGT"),
    (95, "FK11"),
    (96, "FK12"),
    (104, "KPEN"),
    (105, "RCTL"),
    (106, "KPDV"),
    (107, "PRSC"),
    (108, "RALT"),
    (109, "LNFD"),
    (110, "HOME"),
    (111, "UP"),
    (112, "PGUP"),
    (113, "LEFT"),
    (114, "RGHT"),
    (115, "END"),
    (116, "DOWN"),
    (117, "PGDN"),
    (118, "INS"),
    (119, "DELE"),
    (121, "MUTE"),
    (122, "VOL-"),
    (123, "VOL+"),
    (124, "POWR"),
    (125, "KPEQ"),
    (127, "PAUS"),
    (133, "LWIN"),
    (134, "RWIN"),
    (135, "COMP"),
];

fn name_bytes(name: &str) -> [u8; 4] {
    let mut out = [0; 4];
    for (slot, byte) in out.iter_mut().zip(name.bytes()) {
        *slot = byte;
    }
    out
}

/// The XKB name of a key, as xkeyboard-config names the evdev keycodes.
pub fn key_name(keycode: u8) -> [u8; 4] {
    let numbered =
        |prefix: &str, first: u8| name_bytes(&format!("{prefix}{:02}", keycode - first + 1));
    match keycode {
        10..=21 => numbered("AE", 10),
        24..=35 => numbered("AD", 24),
        38..=48 => numbered("AC", 38),
        52..=61 => numbered("AB", 52),
        67..=76 => numbered("FK", 67),
        191..=202 => numbered("FK", 179),
        _ => match KEY_NAMES.iter().find(|(code, _)| *code == keycode) {
            Some((_, name)) => name_bytes(name),
            None => name_bytes(&format!("I{keycode}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keymap::KEYCODE_OFFSET;
    use x11rb_protocol::x11_utils::Serialize;

    const A: u8 = 30 + KEYCODE_OFFSET;
    const E: u8 = 18 + KEYCODE_OFFSET;
    const TWO: u8 = 3 + KEYCODE_OFFSET;
    const ALTGR: u8 = 100 + KEYCODE_OFFSET;
    const SPACE: u8 = 57 + KEYCODE_OFFSET;
    const KP_7: u8 = 71 + KEYCODE_OFFSET;
    const CAPS: u8 = 58 + KEYCODE_OFFSET;

    #[test]
    fn keys_get_the_type_that_their_symbols_need() {
        assert_eq!(sym_map(Layout::Us, A).kt_index[0], ALPHABETIC);
        assert_eq!(sym_map(Layout::Us, A).syms, [0x61, 0x41]);
        assert_eq!(sym_map(Layout::Us, TWO).kt_index[0], TWO_LEVEL);
        assert_eq!(sym_map(Layout::Us, SPACE).kt_index[0], ONE_LEVEL);
        assert_eq!(sym_map(Layout::Us, KP_7).kt_index[0], KEYPAD);
        assert_eq!(sym_map(Layout::Us, 8).width, 0, "no key");
        assert_eq!(sym_map(Layout::Us, 8).group_info, 0);
    }

    #[test]
    fn altgr_layouts_use_four_levels() {
        let two = sym_map(Layout::Es, TWO);
        assert_eq!(two.kt_index[0], FOUR_LEVEL);
        assert_eq!(
            two.syms,
            [0x32, 0x22, 0x40, 0x40],
            "2, double quote, at sign"
        );
        assert_eq!(
            sym_map(Layout::Es, E).kt_index[0],
            FOUR_LEVEL_ALPHABETIC,
            "e, E, euro"
        );
        assert_eq!(sym_map(Layout::Es, ALTGR).syms, [ISO_LEVEL3_SHIFT]);
        assert_eq!(modmap(Layout::Es, ALTGR), MOD5);
        assert_eq!(virtual_mod_real(Layout::Es, 2), MOD5);
        assert_eq!(
            virtual_mod_real(Layout::Us, 2),
            0,
            "no AltGr in the US layout"
        );
    }

    #[test]
    fn the_modifier_map_matches_the_core_one() {
        for layout in [Layout::Us, Layout::Es, Layout::Gb, Layout::De] {
            let mapping = keymap::modifier_mapping(layout);
            for (slot, key) in mapping.iter().enumerate().filter(|(_, key)| **key != 0) {
                let bit = 1 << (slot / 2);
                assert_ne!(modmap(layout, *key) & bit, 0, "{layout:?} keycode {key}");
            }
            let total: u32 = modmap_keys(layout)
                .iter()
                .map(|k| u16::from(k.mods).count_ones())
                .sum();
            assert_eq!(total as usize, mapping.iter().filter(|k| **k != 0).count());
        }
    }

    #[test]
    fn every_level_of_every_key_fits_its_type() {
        let types = key_types();
        for layout in [Layout::Us, Layout::Es, Layout::Gb, Layout::De] {
            for keycode in MIN_KEYCODE..=MAX_KEYCODE {
                let map = sym_map(layout, keycode);
                if map.width > 0 {
                    let levels = types[usize::from(map.kt_index[0])].num_levels;
                    assert_eq!(map.width, levels, "{layout:?} {keycode}");
                }
                let count = actions(layout, keycode).len();
                assert!(count == 0 || count == map.syms.len());
            }
        }
        for def in &TYPES {
            assert!(def.entries.iter().all(
                |(mods, level)| mods & !def.mask == 0 && usize::from(*level) < def.levels.len()
            ));
        }
    }

    #[test]
    fn no_level_of_a_four_level_key_is_empty() {
        // Shift+AltGr and Caps Lock+AltGr must type what AltGr types, as with the core keymap.
        for layout in [Layout::Es, Layout::Gb, Layout::De] {
            for keycode in MIN_KEYCODE..=MAX_KEYCODE {
                let map = sym_map(layout, keycode);
                if map.width == 4 {
                    assert!(map.syms.iter().all(|sym| *sym != 0), "{layout:?} {keycode}");
                }
            }
        }
    }

    #[test]
    fn lock_keys_lock_and_others_set() {
        let caps = actions(Layout::Us, CAPS);
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].serialize()[0], u8::from(SAType::LOCK_MODS));
        let shift = actions(Layout::Us, 50);
        assert_eq!(shift[0].serialize()[0], u8::from(SAType::SET_MODS));
        assert!(actions(Layout::Us, A).is_empty());
    }

    #[test]
    fn key_names_follow_xkeyboard_config() {
        assert_eq!(&key_name(A), b"AC01");
        assert_eq!(&key_name(TWO), b"AE02");
        assert_eq!(&key_name(9), b"ESC\0");
        assert_eq!(&key_name(191), b"FK13");
        assert_eq!(&key_name(ALTGR), b"RALT");
        assert_eq!(&key_name(250), b"I250");
        let names: std::collections::HashSet<[u8; 4]> =
            (MIN_KEYCODE..=MAX_KEYCODE).map(key_name).collect();
        assert_eq!(names.len(), usize::from(KEY_COUNT), "names are unique");
    }

    #[test]
    fn modifier_keys_do_not_repeat() {
        let bits = per_key_repeat(Layout::Us);
        let repeats = |key: u8| bits[usize::from(key / 8)] >> (key % 8) & 1 == 1;
        assert!(repeats(A));
        assert!(!repeats(50), "Shift");
        assert!(!repeats(8), "no key");
    }

    #[test]
    fn indicators_follow_the_locks() {
        assert_eq!(indicator_state(LOCK), 1);
        assert_eq!(indicator_state(MOD2), 2);
        assert_eq!(indicator_state(LOCK | MOD2 | SHIFT), 3);
    }
}
