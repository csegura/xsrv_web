// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! The XKEYBOARD extension, version 1.0, read-only: clients read the server keymap
//! (`xkb_map.rs`) and the keyboard state, and hear when they change. Requests that change the
//! keymap are not supported. Without it, libxkbcommon clients (i3) compile a keymap of their own
//! that does not match the server's keycodes.
//!
//! The replies are encoded by `x11rb-protocol` in the byte order of this machine, so the
//! extension is offered only to clients with the same byte order (`offered_to`).
use std::collections::HashMap;

use x11rb_protocol::errors::ParseError;
use x11rb_protocol::protocol::xkb::{
    BoolCtrl, GetCompatMapReply, GetCompatMapRequest, GetControlsReply, GetDeviceInfoReply,
    GetDeviceInfoRequest, GetIndicatorMapReply, GetIndicatorMapRequest, GetIndicatorStateReply,
    GetMapMap, GetMapMapKeyActions, GetMapReply, GetMapRequest, GetNamedIndicatorReply,
    GetNamedIndicatorRequest, GetNamesReply, GetNamesRequest, GetNamesValueList,
    GetNamesValueListKTLevelNames, GetStateReply, IndicatorStateNotifyEvent, KeyName,
    MapNotifyEvent, MapPart, ModDef, NKNDetail, NameDetail, NamesNotifyEvent,
    NewKeyboardNotifyEvent, PerClientFlag, PerClientFlagsReply, PerClientFlagsRequest,
    StateNotifyEvent, StatePart, UseExtensionReply, UseExtensionRequest, XIFeature, ID,
};
use x11rb_protocol::x11_utils::{RequestHeader, Serialize};

use super::codec::{Endian, Reader, Writer};
use super::dispatch::{Reply, Request};
use super::error::XError;
use super::event::Event;
use super::extensions::{event_code, first_error, XKEYBOARD};
use super::keymap::{self, MAX_KEYCODE, MIN_KEYCODE};
use super::screen::ROOT_WINDOW;
use super::state::{ClientId, State};
use super::xkb_map::{self, DEVICE_ID, KEY_COUNT};

const USE_EXTENSION: u8 = 0;
const SELECT_EVENTS: u8 = 1;
const BELL: u8 = 3;
const GET_STATE: u8 = 4;
const LATCH_LOCK_STATE: u8 = 5;
const GET_CONTROLS: u8 = 6;
const SET_CONTROLS: u8 = 7;
const GET_MAP: u8 = 8;
const GET_COMPAT_MAP: u8 = 10;
const GET_INDICATOR_STATE: u8 = 12;
const GET_INDICATOR_MAP: u8 = 13;
const GET_NAMED_INDICATOR: u8 = 15;
const GET_NAMES: u8 = 17;
const GET_GEOMETRY: u8 = 19;
const PER_CLIENT_FLAGS: u8 = 21;
const GET_DEVICE_INFO: u8 = 24;

const KEYCODES_NAME: &str = "evdev+aliases(qwerty)";
const GEOMETRY_NAME: &str = "pc(pc105)";
const TYPES_NAME: &str = "complete";
const COMPAT_NAME: &str = "complete";
const DEVICE_NAME: &str = "xsrv-web keyboard";
const REPEAT_DELAY_MS: u16 = 660;
const REPEAT_INTERVAL_MS: u16 = 40;
const STRING_ATOM: u32 = 31;

const SUPPORTED_CLIENT_FLAGS: u32 = 0x1F;
const ALL_MAP_PARTS: u16 = 0xFF;
const ALL_STATE_PARTS: u16 = 0x3FFF;
const ALL_NKN_DETAILS: u16 = 0x7;
const MOD_PARTS: u16 = (1 << 0) | (1 << 8) | (1 << 9) | (1 << 10) | (1 << 11) | (1 << 12);

/// Event types and their detail sizes in SelectEvents, in bit order. MapNotify (bit 1) has its
/// details in the fixed part of the request.
const DETAIL_SIZES: [usize; 12] = [2, 0, 2, 4, 4, 4, 2, 1, 1, 1, 2, 2];
const NEW_KEYBOARD_NOTIFY: u16 = 1 << 0;
const MAP_NOTIFY: u16 = 1 << 1;
const STATE_NOTIFY: u16 = 1 << 2;
const INDICATOR_STATE_NOTIFY: u16 = 1 << 4;
const NAMES_NOTIFY: u16 = 1 << 6;
const KNOWN_EVENTS: u16 = (1 << DETAIL_SIZES.len()) - 1;
/// The names that a layout switch changes.
const LAYOUT_NAMES: u32 = (1 << 2) | (1 << 3) | (1 << 12);
const LOCKABLE: u8 = (keymap::LOCK_MASK | keymap::MOD2_MASK) as u8;

/// What one client selected, as detail masks; 0 means not selected.
#[derive(Clone, Copy, Default)]
struct Selection {
    new_keyboard: u16,
    map: u16,
    state: u16,
    indicator_state: u32,
    names: u16,
}

#[derive(Default)]
pub struct XkbClients {
    selections: HashMap<ClientId, Selection>,
    flags: HashMap<ClientId, u32>,
    /// The state that the last StateNotify described (or the start state).
    reported: Option<KeyboardState>,
}

impl XkbClients {
    pub fn forget(&mut self, client: ClientId) {
        self.selections.remove(&client);
        self.flags.remove(&client);
    }
}

/// The modifier and lock state, as XKB reports it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct KeyboardState {
    pub state: u16,
    pub locks: u16,
}

impl KeyboardState {
    fn mods(self) -> u8 {
        self.state as u8
    }
    fn locked(self) -> u8 {
        self.locks as u8
    }
    fn base(self) -> u8 {
        self.mods() & !self.locked()
    }
    fn buttons(self) -> u16 {
        self.state & 0xFF00
    }
}

/// The byte order of the replies that `x11rb-protocol` writes.
fn native_endian() -> Endian {
    if cfg!(target_endian = "little") {
        Endian::Little
    } else {
        Endian::Big
    }
}

pub fn offered_to(endian: Endian) -> bool {
    endian == native_endian()
}

fn parse<T>(
    req: &Request,
    parser: fn(RequestHeader, &[u8]) -> Result<T, ParseError>,
) -> Result<T, XError> {
    let header = RequestHeader {
        major_opcode: req.opcode,
        minor_opcode: req.detail,
        remaining_length: (req.body.len() / 4) as u32,
    };
    parser(header, req.body).map_err(|_| XError::Length)
}

fn reply(bytes: impl AsRef<[u8]>) -> Reply {
    Reply::from_serialized(bytes.as_ref())
}

fn event(bytes: [u8; 32]) -> Event {
    Event::Raw {
        bytes,
        from: native_endian(),
    }
}

/// A key range of a GetMap part: everything for a full part, the asked range for a partial one,
/// nothing for a part that was not asked.
fn key_range(full: bool, partial: bool, first: u8, count: u8) -> Result<(u8, u8), XError> {
    if full {
        return Ok((MIN_KEYCODE, KEY_COUNT));
    }
    if !partial {
        return Ok((0, 0));
    }
    let end = u16::from(first) + u16::from(count);
    if count > 0 && (first < MIN_KEYCODE || end > u16::from(MAX_KEYCODE) + 1) {
        return Err(XError::Value(first.into()));
    }
    Ok((first, count))
}

fn keys(first: u8, count: u8) -> impl Iterator<Item = u8> {
    (0..count).map(move |offset| first + offset)
}

impl State {
    pub(super) fn xkb_request(&mut self, req: &Request) -> Result<Option<Reply>, XError> {
        match req.detail {
            USE_EXTENSION => self.xkb_use_extension(req).map(Some),
            SELECT_EVENTS => self.xkb_select_events(req).map(|()| None),
            BELL => self.xkb_ignored(req).map(|()| None),
            LATCH_LOCK_STATE => self.xkb_latch_lock_state(req).map(|()| None),
            SET_CONTROLS => {
                self.log_once("xkb set controls".into(), || {
                    "xsrv-web: XKB SetControls is ignored: the browser repeats keys, and the other controls do not exist here".into()
                });
                self.xkb_ignored(req).map(|()| None)
            }
            GET_STATE => self.xkb_get_state(req).map(Some),
            GET_CONTROLS => self.xkb_get_controls(req).map(Some),
            GET_MAP => self.xkb_get_map(req).map(Some),
            GET_COMPAT_MAP => self.xkb_get_compat_map(req).map(Some),
            GET_INDICATOR_STATE => self.xkb_get_indicator_state(req).map(Some),
            GET_INDICATOR_MAP => self.xkb_get_indicator_map(req).map(Some),
            GET_NAMED_INDICATOR => self.xkb_get_named_indicator(req).map(Some),
            GET_NAMES => self.xkb_get_names(req).map(Some),
            GET_GEOMETRY => self.xkb_get_geometry(req).map(Some),
            PER_CLIENT_FLAGS => self.xkb_per_client_flags(req).map(Some),
            GET_DEVICE_INFO => self.xkb_get_device_info(req).map(Some),
            _ => Err(XError::Request),
        }
    }

    fn xkb_device(&self, spec: u16) -> Result<(), XError> {
        if spec == u16::from(ID::USE_CORE_KBD) || spec == u16::from(DEVICE_ID) {
            Ok(())
        } else {
            Err(XError::Extension(first_error(XKEYBOARD), spec.into()))
        }
    }

    pub(super) fn keyboard_state(&self) -> KeyboardState {
        KeyboardState {
            state: self.input.state(),
            locks: self.input.locks(),
        }
    }

    fn xkb_use_extension(&mut self, req: &Request) -> Result<Reply, XError> {
        let request = parse(req, UseExtensionRequest::try_parse_request)?;
        Ok(reply(
            UseExtensionReply {
                supported: request.wanted_major == 1,
                sequence: 0,
                length: 0,
                server_major: 1,
                server_minor: 0,
            }
            .serialize(),
        ))
    }

    /// Requests that change nothing here (the bell, controls): the device is checked and the rest
    /// is accepted. The core Bell is silent too.
    fn xkb_ignored(&mut self, req: &Request) -> Result<(), XError> {
        let device = req.reader().u16()?;
        self.xkb_device(device)
    }

    /// Locks Caps Lock and Num Lock (numlockx does this). The page sets them again from the
    /// browser keyboard when they change there. Latches and group locks do not exist here.
    fn xkb_latch_lock_state(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let device = r.u16()?;
        let affect_locks = r.u8()?;
        let locks = r.u8()?;
        let lock_group = r.u8()?;
        r.skip(1)?;
        let affect_latches = r.u8()?;
        r.skip(2)?;
        let latch_group = r.u8()?;
        self.xkb_device(device)?;
        if affect_locks & !LOCKABLE != 0
            || affect_latches != 0
            || lock_group != 0
            || latch_group != 0
        {
            self.log_once("xkb latches".into(), || {
                "xsrv-web: XKB latches, group locks and locks of modifiers other than Caps Lock and Num Lock are ignored".into()
            });
        }
        let affect = affect_locks & LOCKABLE;
        let old = self.input.locks() as u8;
        self.input
            .set_locks(u16::from((old & !affect) | (locks & affect)));
        Ok(())
    }

    fn xkb_select_events(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let device = r.u16()?;
        let affect = r.u16()?;
        if affect & !KNOWN_EVENTS != 0 {
            return Err(XError::Value(affect.into()));
        }
        let clear = r.u16()? & affect;
        let select_all = r.u16()? & affect & !clear;
        let affect_map = r.u16()?;
        let map = r.u16()?;
        self.xkb_device(device)?;
        let selection = self
            .xkb
            .selections
            .get(&req.client)
            .copied()
            .unwrap_or_default();
        let details = |bit: u16, old: u32, all: u32, r: &mut Reader| -> Result<u32, XError> {
            let size = DETAIL_SIZES[bit.trailing_zeros() as usize];
            let read = |r: &mut Reader| -> Result<u32, XError> {
                Ok(match size {
                    1 => r.u8()?.into(),
                    2 => r.u16()?.into(),
                    _ => r.u32()?,
                })
            };
            if affect & bit == 0 {
                return Ok(old);
            }
            if clear & bit != 0 {
                return Ok(0);
            }
            if select_all & bit != 0 {
                return Ok(all);
            }
            if bit == MAP_NOTIFY {
                let (mask, values) = (u32::from(affect_map), u32::from(map));
                return Ok((old & !mask) | (values & mask & all));
            }
            let (mask, values) = (read(r)?, read(r)?);
            Ok((old & !mask) | (values & mask & all))
        };
        let mut known = Selection::default();
        for index in 0..DETAIL_SIZES.len() {
            let bit = 1u16 << index;
            match bit {
                NEW_KEYBOARD_NOTIFY => {
                    known.new_keyboard = details(
                        bit,
                        selection.new_keyboard.into(),
                        ALL_NKN_DETAILS.into(),
                        &mut r,
                    )? as u16;
                }
                MAP_NOTIFY => {
                    known.map =
                        details(bit, selection.map.into(), ALL_MAP_PARTS.into(), &mut r)? as u16;
                }
                STATE_NOTIFY => {
                    known.state =
                        details(bit, selection.state.into(), ALL_STATE_PARTS.into(), &mut r)?
                            as u16;
                }
                INDICATOR_STATE_NOTIFY => {
                    known.indicator_state = details(bit, selection.indicator_state, !0, &mut r)?;
                }
                NAMES_NOTIFY => {
                    known.names = details(bit, selection.names.into(), 0x3FFF, &mut r)? as u16;
                }
                // Selected or not, these events never come; their details are only read.
                _ => {
                    details(bit, 0, !0, &mut r)?;
                }
            }
        }
        // Only the padding to a multiple of 4 bytes may follow the details.
        if r.remaining() >= 4 {
            return Err(XError::Length);
        }
        self.xkb.selections.insert(req.client, known);
        Ok(())
    }

    fn xkb_get_state(&mut self, req: &Request) -> Result<Reply, XError> {
        let request = parse(
            req,
            x11rb_protocol::protocol::xkb::GetStateRequest::try_parse_request,
        )?;
        self.xkb_device(request.device_spec)?;
        let state = self.keyboard_state();
        let mods = state.mods();
        Ok(reply(
            GetStateReply {
                device_id: DEVICE_ID,
                sequence: 0,
                length: 0,
                mods: mods.into(),
                base_mods: state.base().into(),
                latched_mods: 0u8.into(),
                locked_mods: state.locked().into(),
                group: 0u8.into(),
                locked_group: 0u8.into(),
                base_group: 0,
                latched_group: 0,
                compat_state: mods.into(),
                grab_mods: mods.into(),
                compat_grab_mods: mods.into(),
                lookup_mods: mods.into(),
                compat_lookup_mods: mods.into(),
                ptr_btn_state: state.buttons().into(),
            }
            .serialize(),
        ))
    }

    fn xkb_get_controls(&mut self, req: &Request) -> Result<Reply, XError> {
        let device = req.reader().u16()?;
        self.xkb_device(device)?;
        Ok(reply(
            GetControlsReply {
                device_id: DEVICE_ID,
                sequence: 0,
                length: 0,
                mouse_keys_dflt_btn: 1,
                num_groups: 1,
                groups_wrap: 0,
                internal_mods_mask: 0u8.into(),
                ignore_lock_mods_mask: 0u8.into(),
                internal_mods_real_mods: 0u8.into(),
                ignore_lock_mods_real_mods: 0u8.into(),
                internal_mods_vmods: 0u16.into(),
                ignore_lock_mods_vmods: 0u16.into(),
                repeat_delay: REPEAT_DELAY_MS,
                repeat_interval: REPEAT_INTERVAL_MS,
                slow_keys_delay: 0,
                debounce_delay: 0,
                mouse_keys_delay: 0,
                mouse_keys_interval: 0,
                mouse_keys_time_to_max: 0,
                mouse_keys_max_speed: 0,
                mouse_keys_curve: 0,
                access_x_option: 0u16.into(),
                access_x_timeout: 0,
                access_x_timeout_options_mask: 0u16.into(),
                access_x_timeout_options_values: 0u16.into(),
                access_x_timeout_mask: 0u32.into(),
                access_x_timeout_values: 0u32.into(),
                enabled_controls: BoolCtrl::REPEAT_KEYS,
                per_key_repeat: xkb_map::per_key_repeat(self.input.layout),
            }
            .serialize(),
        ))
    }

    fn xkb_get_map(&mut self, req: &Request) -> Result<Reply, XError> {
        let request = parse(req, GetMapRequest::try_parse_request)?;
        self.xkb_device(request.device_spec)?;
        let layout = self.input.layout;
        let full = u16::from(request.full);
        let partial = u16::from(request.partial) & !full;
        let part = |bit: MapPart| (full & u16::from(bit) != 0, partial & u16::from(bit) != 0);
        let wants = |bit: MapPart| {
            let (f, p) = part(bit);
            f || p
        };

        let type_count = xkb_map::type_count();
        let (first_type, n_types) = match part(MapPart::KEY_TYPES) {
            (true, _) => (0, type_count),
            (false, true) => {
                if u16::from(request.first_type) + u16::from(request.n_types) > type_count.into() {
                    return Err(XError::Value(request.first_type.into()));
                }
                (request.first_type, request.n_types)
            }
            _ => (0, 0),
        };
        let range = |bit: MapPart, first: u8, count: u8| {
            let (f, p) = part(bit);
            key_range(f, p, first, count)
        };
        let syms = range(MapPart::KEY_SYMS, request.first_key_sym, request.n_key_syms)?;
        let acts = range(
            MapPart::KEY_ACTIONS,
            request.first_key_action,
            request.n_key_actions,
        )?;
        let behaviors = range(
            MapPart::KEY_BEHAVIORS,
            request.first_key_behavior,
            request.n_key_behaviors,
        )?;
        let explicit = range(
            MapPart::EXPLICIT_COMPONENTS,
            request.first_key_explicit,
            request.n_key_explicit,
        )?;
        let modmap = range(
            MapPart::MODIFIER_MAP,
            request.first_mod_map_key,
            request.n_mod_map_keys,
        )?;
        let vmodmap = range(
            MapPart::VIRTUAL_MOD_MAP,
            request.first_v_mod_map_key,
            request.n_v_mod_map_keys,
        )?;
        let vmods = match part(MapPart::VIRTUAL_MODS) {
            (true, _) => xkb_map::virtual_mods_mask(),
            (false, true) => u16::from(request.virtual_mods) & xkb_map::virtual_mods_mask(),
            _ => 0,
        };

        let sym_maps: Vec<_> = keys(syms.0, syms.1)
            .map(|key| xkb_map::sym_map(layout, key))
            .collect();
        let key_actions: Vec<_> = keys(acts.0, acts.1)
            .map(|key| xkb_map::actions(layout, key))
            .collect();
        let in_range = |(first, count): (u8, u8), key: u8| {
            key >= first && u16::from(key) < u16::from(first) + u16::from(count)
        };
        let modmap_keys: Vec<_> = xkb_map::modmap_keys(layout)
            .into_iter()
            .filter(|entry| in_range(modmap, entry.keycode))
            .collect();
        let vmodmap_keys: Vec<_> = xkb_map::vmodmap_keys(layout)
            .into_iter()
            .filter(|entry| in_range(vmodmap, entry.keycode))
            .collect();
        let vmods_rtrn: Vec<_> = (0..xkb_map::VIRTUAL_MODS.len())
            .filter(|index| vmods & 1 << index != 0)
            .map(|index| xkb_map::virtual_mod_real(layout, index).into())
            .collect();

        let map = GetMapMap {
            types_rtrn: wants(MapPart::KEY_TYPES).then(|| {
                xkb_map::key_types()
                    .into_iter()
                    .skip(first_type.into())
                    .take(n_types.into())
                    .collect()
            }),
            syms_rtrn: wants(MapPart::KEY_SYMS).then(|| sym_maps.clone()),
            key_actions: wants(MapPart::KEY_ACTIONS).then(|| GetMapMapKeyActions {
                acts_rtrn_count: key_actions.iter().map(|a| a.len() as u8).collect(),
                acts_rtrn_acts: key_actions.iter().flatten().copied().collect(),
            }),
            behaviors_rtrn: wants(MapPart::KEY_BEHAVIORS).then(Vec::new),
            vmods_rtrn: wants(MapPart::VIRTUAL_MODS).then_some(vmods_rtrn),
            explicit_rtrn: wants(MapPart::EXPLICIT_COMPONENTS).then(Vec::new),
            modmap_rtrn: wants(MapPart::MODIFIER_MAP).then(|| modmap_keys.clone()),
            vmodmap_rtrn: wants(MapPart::VIRTUAL_MOD_MAP).then(|| vmodmap_keys.clone()),
        };
        let total_syms = sym_maps.iter().map(|m| m.syms.len()).sum::<usize>();
        let total_actions = key_actions.iter().map(Vec::len).sum::<usize>();
        Ok(reply(
            GetMapReply {
                device_id: DEVICE_ID,
                sequence: 0,
                length: 0,
                min_key_code: MIN_KEYCODE,
                max_key_code: MAX_KEYCODE,
                first_type,
                n_types,
                total_types: type_count,
                first_key_sym: syms.0,
                total_syms: total_syms as u16,
                n_key_syms: syms.1,
                first_key_action: acts.0,
                total_actions: total_actions as u16,
                n_key_actions: acts.1,
                first_key_behavior: behaviors.0,
                n_key_behaviors: behaviors.1,
                total_key_behaviors: 0,
                first_key_explicit: explicit.0,
                n_key_explicit: explicit.1,
                total_key_explicit: 0,
                first_mod_map_key: modmap.0,
                n_mod_map_keys: modmap.1,
                total_mod_map_keys: modmap_keys.len() as u8,
                first_v_mod_map_key: vmodmap.0,
                n_v_mod_map_keys: vmodmap.1,
                total_v_mod_map_keys: vmodmap_keys.len() as u8,
                virtual_mods: vmods.into(),
                map,
            }
            .serialize(),
        ))
    }

    fn xkb_get_compat_map(&mut self, req: &Request) -> Result<Reply, XError> {
        let request = parse(req, GetCompatMapRequest::try_parse_request)?;
        self.xkb_device(request.device_spec)?;
        let all = xkb_map::sym_interprets();
        let total = all.len() as u16;
        let (first, count) = if request.get_all_si {
            (0, total)
        } else {
            (request.first_si, request.n_si)
        };
        if u32::from(first) + u32::from(count) > u32::from(total) {
            return Err(XError::Value(first.into()));
        }
        let groups = u8::from(request.groups) & 0x0F;
        let group_rtrn = (0..4)
            .filter(|bit| groups & 1 << bit != 0)
            .map(|_| ModDef {
                mask: 0u8.into(),
                real_mods: 0u8.into(),
                vmods: 0u16.into(),
            })
            .collect();
        Ok(reply(
            GetCompatMapReply {
                device_id: DEVICE_ID,
                sequence: 0,
                length: 0,
                groups_rtrn: groups.into(),
                first_si_rtrn: first,
                n_total_si: total,
                si_rtrn: all
                    .into_iter()
                    .skip(first.into())
                    .take(count.into())
                    .collect(),
                group_rtrn,
            }
            .serialize(),
        ))
    }

    fn xkb_get_indicator_state(&mut self, req: &Request) -> Result<Reply, XError> {
        let device = req.reader().u16()?;
        self.xkb_device(device)?;
        Ok(reply(
            GetIndicatorStateReply {
                device_id: DEVICE_ID,
                sequence: 0,
                length: 0,
                state: xkb_map::indicator_state(self.keyboard_state().locked()),
            }
            .serialize(),
        ))
    }

    fn xkb_get_indicator_map(&mut self, req: &Request) -> Result<Reply, XError> {
        let request = parse(req, GetIndicatorMapRequest::try_parse_request)?;
        self.xkb_device(request.device_spec)?;
        let maps = (0..32)
            .filter(|bit| request.which & 1 << bit != 0)
            .map(|bit| xkb_map::indicator_map(bit as usize))
            .collect();
        Ok(reply(
            GetIndicatorMapReply {
                device_id: DEVICE_ID,
                sequence: 0,
                length: 0,
                which: request.which,
                real_indicators: xkb_map::INDICATOR_MASK,
                n_indicators: xkb_map::INDICATORS.len() as u8,
                maps,
            }
            .serialize(),
        ))
    }

    fn xkb_get_named_indicator(&mut self, req: &Request) -> Result<Reply, XError> {
        let request = parse(req, GetNamedIndicatorRequest::try_parse_request)?;
        self.xkb_device(request.device_spec)?;
        self.require_atom(request.indicator)?;
        let name = self.atoms.name(request.indicator).map(<[u8]>::to_vec);
        let index = xkb_map::INDICATORS
            .iter()
            .position(|known| name.as_deref() == Some(known.as_bytes()));
        let map = xkb_map::indicator_map(index.unwrap_or(xkb_map::INDICATORS.len()));
        let on = index.is_some_and(|index| {
            xkb_map::indicator_state(self.keyboard_state().locked()) & 1 << index != 0
        });
        Ok(reply(
            GetNamedIndicatorReply {
                device_id: DEVICE_ID,
                sequence: 0,
                length: 0,
                indicator: request.indicator,
                found: index.is_some(),
                on,
                real_indicator: index.is_some(),
                ndx: index.unwrap_or(0) as u8,
                map_flags: map.flags,
                map_which_groups: map.which_groups,
                map_groups: 0u8.into(),
                map_which_mods: map.which_mods,
                map_mods: map.mods,
                map_real_mods: map.real_mods,
                map_vmod: map.vmods,
                map_ctrls: map.ctrls,
                supported: true,
            }
            .serialize(),
        ))
    }

    fn intern(&mut self, name: &str) -> Result<u32, XError> {
        self.atoms.intern(name.as_bytes()).ok_or(XError::Alloc)
    }

    fn intern_all<'a>(
        &mut self,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<Vec<u32>, XError> {
        names.into_iter().map(|name| self.intern(name)).collect()
    }

    fn xkb_get_names(&mut self, req: &Request) -> Result<Reply, XError> {
        let request = parse(req, GetNamesRequest::try_parse_request)?;
        self.xkb_device(request.device_spec)?;
        let layout = self.input.layout;
        let which = u32::from(request.which) & !u32::from(NameDetail::RG_NAMES);
        let wants = |bit: NameDetail| which & u32::from(bit) != 0;
        let level_names: Vec<&[&str]> = xkb_map::level_names().collect();
        let one = |this: &mut Self, bit: NameDetail, name: &str| -> Result<Option<u32>, XError> {
            if wants(bit) {
                this.intern(name).map(Some)
            } else {
                Ok(None)
            }
        };
        let value_list = GetNamesValueList {
            keycodes_name: one(self, NameDetail::KEYCODES, KEYCODES_NAME)?,
            geometry_name: one(self, NameDetail::GEOMETRY, GEOMETRY_NAME)?,
            symbols_name: one(self, NameDetail::SYMBOLS, xkb_map::symbols_name(layout))?,
            phys_symbols_name: one(
                self,
                NameDetail::PHYS_SYMBOLS,
                xkb_map::symbols_name(layout),
            )?,
            types_name: one(self, NameDetail::TYPES, TYPES_NAME)?,
            compat_name: one(self, NameDetail::COMPAT, COMPAT_NAME)?,
            type_names: match wants(NameDetail::KEY_TYPE_NAMES) {
                true => Some(self.intern_all(xkb_map::type_names())?),
                false => None,
            },
            kt_level_names: match wants(NameDetail::KT_LEVEL_NAMES) {
                true => Some(GetNamesValueListKTLevelNames {
                    n_levels_per_type: level_names.iter().map(|l| l.len() as u8).collect(),
                    kt_level_names: self
                        .intern_all(level_names.iter().flat_map(|l| l.iter().copied()))?,
                }),
                false => None,
            },
            indicator_names: match wants(NameDetail::INDICATOR_NAMES) {
                true => Some(self.intern_all(xkb_map::INDICATORS)?),
                false => None,
            },
            virtual_mod_names: match wants(NameDetail::VIRTUAL_MOD_NAMES) {
                true => Some(self.intern_all(xkb_map::VIRTUAL_MODS)?),
                false => None,
            },
            groups: match wants(NameDetail::GROUP_NAMES) {
                true => Some(vec![self.intern(xkb_map::group_name(layout))?]),
                false => None,
            },
            key_names: wants(NameDetail::KEY_NAMES).then(|| {
                keys(MIN_KEYCODE, KEY_COUNT)
                    .map(|key| KeyName {
                        name: xkb_map::key_name(key),
                    })
                    .collect()
            }),
            key_aliases: wants(NameDetail::KEY_ALIASES).then(Vec::new),
            radio_group_names: None,
        };
        let n_kt_levels = level_names.iter().map(|l| l.len()).sum::<usize>();
        Ok(reply(
            GetNamesReply {
                device_id: DEVICE_ID,
                sequence: 0,
                length: 0,
                min_key_code: MIN_KEYCODE,
                max_key_code: MAX_KEYCODE,
                n_types: xkb_map::type_count(),
                group_names: 1u8.into(),
                virtual_mods: xkb_map::virtual_mods_mask().into(),
                first_key: MIN_KEYCODE,
                n_keys: KEY_COUNT,
                indicators: xkb_map::INDICATOR_MASK,
                n_radio_groups: 0,
                n_key_aliases: 0,
                n_kt_levels: n_kt_levels as u16,
                value_list,
            }
            .serialize(),
        ))
    }

    /// There is no geometry: the reply says "not found", which libX11 reports as BadName to its
    /// caller, as for a server without geometry data. (xcb-proto has no GetGeometry, so the
    /// reply is written here.)
    fn xkb_get_geometry(&mut self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let device = r.u16()?;
        r.skip(2)?;
        let name = r.u32()?;
        r.end()?;
        self.xkb_device(device)?;
        let mut w = Writer::new(req.endian);
        w.u32(name);
        w.u8(0);
        w.zeros(19);
        Ok(Reply::new(DEVICE_ID, w))
    }

    /// The flags are kept for the client. Key repeat already sends presses without releases, which
    /// is what DetectableAutoRepeat asks for.
    fn xkb_per_client_flags(&mut self, req: &Request) -> Result<Reply, XError> {
        let request = parse(req, PerClientFlagsRequest::try_parse_request)?;
        self.xkb_device(request.device_spec)?;
        let change = u32::from(request.change) & SUPPORTED_CLIENT_FLAGS;
        let old = self.xkb.flags.get(&req.client).copied().unwrap_or(0);
        let value = (old & !change) | (u32::from(request.value) & change);
        self.xkb.flags.insert(req.client, value);
        Ok(reply(
            PerClientFlagsReply {
                device_id: DEVICE_ID,
                sequence: 0,
                length: 0,
                supported: PerClientFlag::from(SUPPORTED_CLIENT_FLAGS),
                value: PerClientFlag::from(value),
                auto_ctrls: 0u32.into(),
                auto_ctrls_values: 0u32.into(),
            }
            .serialize(),
        ))
    }

    /// The core keyboard, without buttons or LED feedbacks.
    fn xkb_get_device_info(&mut self, req: &Request) -> Result<Reply, XError> {
        let request = parse(req, GetDeviceInfoRequest::try_parse_request)?;
        self.xkb_device(request.device_spec)?;
        let dev_type = self.intern("KEYBOARD")?;
        let supported = u16::from(XIFeature::KEYBOARDS);
        Ok(reply(
            GetDeviceInfoReply {
                device_id: DEVICE_ID,
                sequence: 0,
                length: 0,
                present: 0u16.into(),
                supported: supported.into(),
                unsupported: (u16::from(request.wanted) & !supported).into(),
                first_btn_wanted: 0,
                n_btns_wanted: 0,
                first_btn_rtrn: 0,
                total_btns: 0,
                has_own_state: false,
                dflt_kbd_fb: 0,
                dflt_led_fb: 0,
                dev_type,
                name: DEVICE_NAME.as_bytes().to_vec(),
                btn_actions: Vec::new(),
                leds: Vec::new(),
            }
            .serialize(),
        ))
    }

    /// The root property `_XKB_RULES_NAMES` (rules, model, layout, variant, options), which an
    /// XKB server sets and tools such as `setxkbmap -query` read.
    pub(super) fn xkb_publish_rules_names(&mut self) {
        if !self.xkb_enabled {
            return;
        }
        let Some(property) = self.atoms.intern(b"_XKB_RULES_NAMES") else {
            return;
        };
        let layout = wire::LAYOUTS[usize::from(self.input.layout.index())];
        let data = format!("evdev\0pc105\0{layout}\0\0\0").into_bytes();
        if !self.store_property(ROOT_WINDOW, property, STRING_ATOM, 8, data) {
            self.log_once("xkb rules names".into(), || {
                "xsrv-web: _XKB_RULES_NAMES was not updated: the property space is full".into()
            });
        }
    }

    /// After a layout switch: the clients that listen reload the keymap.
    pub(super) fn xkb_keymap_changed(&mut self) {
        self.xkb_publish_rules_names();
        let Some(code) = event_code(XKEYBOARD, 0) else {
            return;
        };
        let time = self.timestamp();
        for (client, selection) in &self.xkb.selections {
            if selection.new_keyboard & u16::from(NKNDetail::KEYCODES) != 0 {
                let event = NewKeyboardNotifyEvent {
                    response_type: code,
                    xkb_type: 0,
                    sequence: 0,
                    time,
                    device_id: DEVICE_ID,
                    old_device_id: DEVICE_ID,
                    min_key_code: MIN_KEYCODE,
                    max_key_code: MAX_KEYCODE,
                    old_min_key_code: MIN_KEYCODE,
                    old_max_key_code: MAX_KEYCODE,
                    request_major: 0,
                    request_minor: 0,
                    changed: NKNDetail::KEYCODES,
                };
                self.send_event_to(*client, &event_from(&event));
            }
            let names = u32::from(selection.names) & LAYOUT_NAMES;
            if names != 0 {
                let event = NamesNotifyEvent {
                    response_type: code,
                    xkb_type: 6,
                    sequence: 0,
                    time,
                    device_id: DEVICE_ID,
                    changed: (names as u16).into(),
                    first_type: 0,
                    n_types: 0,
                    first_level_name: 0,
                    n_level_names: 0,
                    n_radio_groups: 0,
                    n_key_aliases: 0,
                    changed_group_names: 1u8.into(),
                    changed_virtual_mods: 0u16.into(),
                    first_key: 0,
                    n_keys: 0,
                    changed_indicators: 0,
                };
                self.send_event_to(*client, &event_from(&event));
            }
            let changed = selection.map & ALL_MAP_PARTS;
            if changed != 0 {
                let event = MapNotifyEvent {
                    response_type: code,
                    xkb_type: 1,
                    sequence: 0,
                    time,
                    device_id: DEVICE_ID,
                    ptr_btn_actions: 0,
                    changed: changed.into(),
                    min_key_code: MIN_KEYCODE,
                    max_key_code: MAX_KEYCODE,
                    first_type: 0,
                    n_types: xkb_map::type_count(),
                    first_key_sym: MIN_KEYCODE,
                    n_key_syms: KEY_COUNT,
                    first_key_act: MIN_KEYCODE,
                    n_key_acts: KEY_COUNT,
                    first_key_behavior: MIN_KEYCODE,
                    n_key_behavior: KEY_COUNT,
                    first_key_explicit: MIN_KEYCODE,
                    n_key_explicit: KEY_COUNT,
                    first_mod_map_key: MIN_KEYCODE,
                    n_mod_map_keys: KEY_COUNT,
                    first_v_mod_map_key: MIN_KEYCODE,
                    n_v_mod_map_keys: KEY_COUNT,
                    virtual_mods: xkb_map::virtual_mods_mask().into(),
                };
                self.send_event_to(*client, &event_from(&event));
            }
        }
    }

    /// Tells the clients that listen how the modifiers, the locks or the buttons changed since the
    /// last report. `keycode` and `event_type` name the input that changed them, or are 0.
    pub(super) fn xkb_report_state(&mut self, keycode: u8, event_type: u8) {
        let after = self.keyboard_state();
        let Some(before) = self.xkb.reported.replace(after) else {
            return;
        };
        if before == after || self.xkb.selections.is_empty() {
            return;
        }
        let Some(code) = event_code(XKEYBOARD, 0) else {
            return;
        };
        let mut changed = 0u16;
        if before.mods() != after.mods() {
            changed |= MOD_PARTS;
        }
        if before.base() != after.base() {
            changed |= u16::from(StatePart::MODIFIER_BASE);
        }
        if before.locked() != after.locked() {
            changed |= u16::from(StatePart::MODIFIER_LOCK);
        }
        if before.buttons() != after.buttons() {
            changed |= u16::from(StatePart::POINTER_BUTTONS);
        }
        let lights = |state: KeyboardState| xkb_map::indicator_state(state.locked());
        let lights_changed = lights(before) ^ lights(after);
        let time = self.timestamp();
        let mods = after.mods();
        let state_event = StateNotifyEvent {
            response_type: code,
            xkb_type: 2,
            sequence: 0,
            time,
            device_id: DEVICE_ID,
            mods: mods.into(),
            base_mods: after.base().into(),
            latched_mods: 0u8.into(),
            locked_mods: after.locked().into(),
            group: 0u8.into(),
            base_group: 0,
            latched_group: 0,
            locked_group: 0u8.into(),
            compat_state: mods.into(),
            grab_mods: mods.into(),
            compat_grab_mods: mods.into(),
            lookup_mods: mods.into(),
            compat_loockup_mods: mods.into(),
            ptr_btn_state: after.buttons().into(),
            changed: changed.into(),
            keycode,
            event_type,
            request_major: 0,
            request_minor: 0,
        };
        let lights_event = IndicatorStateNotifyEvent {
            response_type: code,
            xkb_type: 4,
            sequence: 0,
            time,
            device_id: DEVICE_ID,
            state: lights(after),
            state_changed: lights_changed,
        };
        for (client, selection) in &self.xkb.selections {
            if selection.state & changed != 0 {
                self.send_event_to(*client, &event_from(&state_event));
            }
            if selection.indicator_state & lights_changed != 0 {
                self.send_event_to(*client, &event_from(&lights_event));
            }
        }
    }
}

fn event_from<T>(value: &T) -> Event
where
    for<'a> &'a T: Into<[u8; 32]>,
{
    event(value.into())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
    use tokio::sync::Notify;

    use super::*;
    use crate::display::Display;

    fn state_with_client() -> (State, ClientId, UnboundedReceiver<Vec<u8>>) {
        let mut state = State::new(Display::new(640, 480));
        state.set_xkb(true);
        let (tx, rx) = unbounded_channel();
        let (client, _) = state
            .add_client(
                Endian::Little,
                tx,
                Rc::new(Cell::new(0)),
                Rc::new(Notify::new()),
                Rc::new(Cell::new(false)),
            )
            .unwrap();
        (state, client, rx)
    }

    fn words(values: &[u16]) -> Vec<u8> {
        let mut body: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        body.resize(body.len().next_multiple_of(4), 0);
        body
    }

    const CORE_KBD: u16 = 0x100;

    #[test]
    fn a_map_selection_by_affect_map_alone_is_kept() {
        // What libX11 sends at start: XkbSelectEventDetails(MapNotify, AllClientInfo, AllClientInfo).
        let (mut state, client, _rx) = state_with_client();
        let body = words(&[CORE_KBD, MAP_NOTIFY, 0, 0, 0x7, 0x7]);
        state.process(client, XKEYBOARD, SELECT_EVENTS, &body);
        assert_eq!(state.xkb.selections[&client].map, 0x7);

        let body = words(&[CORE_KBD, MAP_NOTIFY, 0, 0, 0x1, 0]);
        state.process(client, XKEYBOARD, SELECT_EVENTS, &body);
        assert_eq!(
            state.xkb.selections[&client].map, 0x6,
            "only the affected part goes"
        );

        let body = words(&[CORE_KBD, MAP_NOTIFY, MAP_NOTIFY, 0, 0, 0]);
        state.process(client, XKEYBOARD, SELECT_EVENTS, &body);
        assert_eq!(state.xkb.selections[&client].map, 0, "cleared");
    }

    #[test]
    fn event_details_are_read_in_bit_order() {
        let (mut state, client, _rx) = state_with_client();
        // New keyboard (2 x u16), then state (2 x u16), then indicator state (2 x u32).
        let mut body = words(&[
            CORE_KBD,
            NEW_KEYBOARD_NOTIFY | STATE_NOTIFY | INDICATOR_STATE_NOTIFY,
            0,
            0,
            0,
            0,
            0x7,
            0x1,
            0xFFFF,
            0x9,
        ]);
        body.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0x2, 0, 0, 0]);
        state.process(client, XKEYBOARD, SELECT_EVENTS, &body);
        let selection = state.xkb.selections[&client];
        assert_eq!(selection.new_keyboard, 0x1);
        assert_eq!(selection.state, 0x9);
        assert_eq!(selection.indicator_state, 0x2);
    }

    #[test]
    fn a_selection_goes_with_its_client() {
        let (mut state, client, _rx) = state_with_client();
        let body = words(&[CORE_KBD, STATE_NOTIFY, 0, STATE_NOTIFY, 0, 0]);
        state.process(client, XKEYBOARD, SELECT_EVENTS, &body);
        assert_eq!(state.xkb.selections[&client].state, ALL_STATE_PARTS);
        state.remove_client(client);
        assert!(state.xkb.selections.is_empty());
    }

    #[test]
    fn numlockx_can_lock_num_lock_and_clients_hear_it() {
        let (mut state, client, mut rx) = state_with_client();
        let select = words(&[CORE_KBD, STATE_NOTIFY, 0, STATE_NOTIFY, 0, 0]);
        state.process(client, XKEYBOARD, SELECT_EVENTS, &select);
        while rx.try_recv().is_ok() {}
        // LatchLockState: affectModLocks = Mod2, modLocks = Mod2, nothing else.
        let lock = [0x00, 0x01, 0x10, 0x10, 0, 0, 0, 0, 0, 0, 0, 0];
        state.process(client, XKEYBOARD, LATCH_LOCK_STATE, &lock);
        assert_eq!(state.keyboard_state().locks, keymap::MOD2_MASK);
        let event = rx.try_recv().expect("a StateNotify");
        assert_eq!((event[0], event[1]), (70, 2), "XKB event, StateNotify");
    }

    #[test]
    fn select_events_refuses_unknown_events_and_extra_bytes() {
        let (mut state, client, mut rx) = state_with_client();
        state.process(
            client,
            XKEYBOARD,
            SELECT_EVENTS,
            &words(&[CORE_KBD, 1 << 12, 0, 0, 0, 0]),
        );
        let error = rx.try_recv().expect("an error");
        assert_eq!((error[0], error[1]), (0, 2), "BadValue");
        let long = words(&[CORE_KBD, STATE_NOTIFY, 0, STATE_NOTIFY, 0, 0, 0, 0]);
        state.process(client, XKEYBOARD, SELECT_EVENTS, &long);
        let error = rx.try_recv().expect("an error");
        assert_eq!((error[0], error[1]), (0, 16), "BadLength");
    }

    #[test]
    fn a_named_indicator_needs_an_atom() {
        let (mut state, client, mut rx) = state_with_client();
        let body = [0x00, 0x01, 0x00, 0x01, 0x00, 0x05, 0, 0, 0xFF, 0xFF, 0, 0];
        state.process(client, XKEYBOARD, GET_NAMED_INDICATOR, &body);
        let error = rx.try_recv().expect("an error");
        assert_eq!((error[0], error[1]), (0, 5), "BadAtom");
    }

    #[test]
    fn get_map_refuses_a_key_range_outside_the_keycodes() {
        assert_eq!(key_range(false, true, 8, 248), Ok((8, 248)));
        assert_eq!(key_range(false, true, 7, 1), Err(XError::Value(7)));
        assert_eq!(key_range(false, true, 200, 60), Err(XError::Value(200)));
        assert_eq!(key_range(true, false, 0, 0), Ok((MIN_KEYCODE, KEY_COUNT)));
        assert_eq!(key_range(false, false, 9, 9), Ok((0, 0)));
    }

    #[test]
    fn other_byte_orders_do_not_get_the_extension() {
        assert!(offered_to(native_endian()));
        let other = match native_endian() {
            Endian::Little => Endian::Big,
            Endian::Big => Endian::Little,
        };
        assert!(!offered_to(other));
    }
}
