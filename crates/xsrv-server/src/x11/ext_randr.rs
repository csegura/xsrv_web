// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! The RANDR extension, version 1.5, for one screen with one output, one CRTC and one mode.
//! The size of the screen can change (SetScreenSize, or the page when its window is resized).
use std::mem::replace;

use super::codec::Writer;
use super::dispatch::{Reply, Request};
use super::error::XError;
use super::event::Event;
use super::extensions::{event_code, first_error, RANDR};
use super::resources::IRect;
use super::screen::ROOT_WINDOW;
use super::state::State;

/// The ids of the virtual monitors: monitor `k` has CRTC + k, OUTPUT + k and MODE + k.
const CRTC: u32 = 0x40;
const OUTPUT: u32 = 0x50;
const MODE: u32 = 0x60;
pub const MAX_MONITORS: usize = 4;
const GAMMA_SIZE: u16 = 256;
pub const MIN_SCREEN_SIZE: u16 = 64;
pub const MAX_SCREEN_SIZE: u16 = 4096;
const MM_PER_PIXEL: f32 = 25.4 / 96.0;
const ROTATE_0: u16 = 1;
const REFRESH_RATE: u16 = 60;
const STATUS_SUCCESS: u8 = 0;
const CONNECTED: u8 = 0;
const SUBPIXEL_UNKNOWN: u8 = 0;
const SELECT_SCREEN_CHANGE: u16 = 1;
const SELECT_CRTC_CHANGE: u16 = 2;
/// Screen, CRTC, output and property changes, then the 1.4 provider and resource changes and
/// the 1.6 leases. The last ones are accepted and never sent: there is no provider.
const SELECT_ALL: u16 = 0xFF;
const SCREEN_CHANGE_EVENT: u8 = 0;
const NOTIFY_EVENT: u8 = 1;
const CRTC_CHANGE: u8 = 0;

fn millimetres(pixels: u16) -> u32 {
    (f32::from(pixels) * MM_PER_PIXEL).round() as u32
}

fn mode_name(width: u16, height: u16) -> String {
    format!("{width}x{height}")
}

impl State {
    pub(super) fn randr_request(&mut self, req: &Request) -> Result<Option<Reply>, XError> {
        match req.detail {
            0 => self.randr_query_version(req).map(Some),
            4 => self.randr_select_input(req).map(|()| None),
            5 => self.randr_screen_info(req).map(Some),
            6 => self.randr_screen_size_range(req).map(Some),
            7 => self.randr_set_screen_size(req).map(|()| None),
            8 | 25 => self.randr_screen_resources(req).map(Some),
            9 => self.randr_output_info(req).map(Some),
            10 => self.randr_list_output_properties(req).map(Some),
            15 => self.randr_get_output_property(req).map(Some),
            22 => self.randr_get_crtc_gamma_size(req).map(Some),
            23 => self.randr_get_crtc_gamma(req).map(Some),
            24 => self.randr_set_crtc_gamma(req).map(|()| None),
            27 => self.randr_get_crtc_transform(req).map(Some),
            28 => self.randr_get_panning(req).map(Some),
            20 => self.randr_crtc_info(req).map(Some),
            21 => self.randr_set_crtc_config(req).map(Some),
            30 => self.randr_set_output_primary(req).map(|()| None),
            31 => self.randr_output_primary(req).map(Some),
            32 => self.randr_providers(req).map(Some),
            42 => self.randr_monitors(req).map(Some),
            _ => Err(XError::Request),
        }
    }

    fn randr_query_version(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let (major, minor) = (r.u32()?, r.u32()?);
        r.end()?;
        // The newest version that is implemented is 1.5. A client may ask for less.
        let (major, minor) = (major, minor).min((1, 5));
        let mut w = Writer::new(req.endian);
        w.u32(major);
        w.u32(minor);
        Ok(Reply::new(0, w))
    }

    /// The old configuration request (version 1.0): one size, the screen as it is now, 60 Hz.
    fn randr_screen_info(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        self.window(r.u32()?)?;
        r.end()?;
        let (width, height) = self.screen_size();
        let mut w = Writer::new(req.endian);
        w.u32(ROOT_WINDOW);
        w.u32(self.config_time);
        w.u32(self.config_time);
        w.u16(1);
        w.u16(0);
        w.u16(ROTATE_0);
        w.u16(REFRESH_RATE);
        w.u16(2);
        w.zeros(2);
        w.u16(width);
        w.u16(height);
        w.u16(millimetres(width) as u16);
        w.u16(millimetres(height) as u16);
        w.u16(1);
        w.u16(REFRESH_RATE);
        Ok(Reply::new(ROTATE_0 as u8, w))
    }

    /// GetMonitors: one monitor for each virtual monitor, the first one primary. Every monitor
    /// has the output of its CRTC.
    fn randr_monitors(&mut self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        self.window(r.u32()?)?;
        r.u8()?;
        r.skip(3)?;
        r.end()?;
        let (_, height) = self.screen_size();
        let count = self.monitors.max(1);
        let mut w = Writer::new(req.endian);
        w.u32(self.config_time);
        w.u32(count as u32);
        w.u32(count as u32);
        w.zeros(12);
        for k in 0..count {
            let name = self.atoms.intern(format!("xsrv-{k}").as_bytes());
            let (x, width) = self.monitor_columns(k);
            w.u32(name.ok_or(XError::Alloc)?);
            w.u8(u8::from(self.primary_output == OUTPUT + k as u32));
            w.u8(1);
            w.u16(1);
            w.i16(x as i16);
            w.i16(0);
            w.u16(width);
            w.u16(height);
            w.u32(millimetres(width));
            w.u32(millimetres(height));
            w.u32(OUTPUT + k as u32);
        }
        Ok(Reply::new(0, w))
    }

    /// There is no GPU provider: GetProviders lists none.
    fn randr_providers(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        self.window(r.u32()?)?;
        r.end()?;
        let mut w = Writer::new(req.endian);
        w.u32(self.config_time);
        w.u16(0);
        w.zeros(18);
        Ok(Reply::new(0, w))
    }

    /// Another id than an output is a BadOutput error, and 0 means no primary output.
    fn randr_set_output_primary(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        self.window(r.u32()?)?;
        let output = r.u32()?;
        r.end()?;
        if output != 0 && self.monitor_of(output, OUTPUT).is_none() {
            return Err(XError::Extension(first_error(RANDR), output));
        }
        self.primary_output = output;
        Ok(())
    }

    fn randr_select_input(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let window = r.u32()?;
        let mask = r.u16()?;
        r.skip(2)?;
        r.end()?;
        self.window(window)?;
        if mask & !SELECT_ALL != 0 {
            return Err(XError::Value(mask.into()));
        }
        if mask == 0 {
            self.randr_listeners.remove(&req.client);
        } else {
            self.randr_listeners.insert(req.client, mask);
        }
        Ok(())
    }

    fn randr_screen_size_range(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        self.window(r.u32()?)?;
        r.end()?;
        let mut w = Writer::new(req.endian);
        for size in [
            MIN_SCREEN_SIZE,
            MIN_SCREEN_SIZE,
            MAX_SCREEN_SIZE,
            MAX_SCREEN_SIZE,
        ] {
            w.u16(size);
        }
        Ok(Reply::new(0, w))
    }

    fn randr_set_screen_size(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        self.window(r.u32()?)?;
        let width = r.u16()?;
        let height = r.u16()?;
        r.skip(8)?;
        r.end()?;
        self.set_screen_size(width, height)
    }

    /// The monitors side by side: the screen is cut into `monitors` columns of equal width, and the
    /// last one takes what is left. Returns (x, width) of monitor `index`.
    pub(super) fn monitor_columns(&self, index: usize) -> (u16, u16) {
        let (width, _) = self.screen_size();
        let count = self.monitors.max(1) as u16;
        let each = width / count;
        let x = each * index as u16;
        let last = index + 1 == usize::from(count);
        (x, if last { width - x } else { each })
    }

    /// The monitor that an id of a CRTC, an output or a mode belongs to.
    fn monitor_of(&self, id: u32, base: u32) -> Option<usize> {
        let index = usize::try_from(id.checked_sub(base)?).ok()?;
        (index < self.monitors.max(1)).then_some(index)
    }

    fn randr_screen_resources(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        self.window(r.u32()?)?;
        r.end()?;
        let (_, height) = self.screen_size();
        let count = self.monitors.max(1);
        let names: Vec<String> = (0..count)
            .map(|k| mode_name(self.monitor_columns(k).1, height))
            .collect();
        let mut w = Writer::new(req.endian);
        w.u32(self.config_time);
        w.u32(self.config_time);
        w.u16(count as u16);
        w.u16(count as u16);
        w.u16(count as u16);
        w.u16(names.iter().map(String::len).sum::<usize>() as u16);
        w.zeros(8);
        for k in 0..count as u32 {
            w.u32(CRTC + k);
        }
        for k in 0..count as u32 {
            w.u32(OUTPUT + k);
        }
        for (k, name) in names.iter().enumerate() {
            write_mode_info(
                &mut w,
                MODE + k as u32,
                self.monitor_columns(k).1,
                height,
                name.len(),
            );
        }
        for name in &names {
            w.bytes(name.as_bytes());
        }
        Ok(Reply::new(0, w))
    }

    fn randr_output_info(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let output = r.u32()?;
        r.skip(4)?;
        r.end()?;
        let Some(k) = self.monitor_of(output, OUTPUT) else {
            return Err(XError::Extension(first_error(RANDR), output));
        };
        let (_, height) = self.screen_size();
        let width = self.monitor_columns(k).1;
        let name = format!("xsrv-{k}");
        let mut w = Writer::new(req.endian);
        w.u32(self.config_time);
        w.u32(CRTC + k as u32);
        w.u32(millimetres(width));
        w.u32(millimetres(height));
        w.u8(CONNECTED);
        w.u8(SUBPIXEL_UNKNOWN);
        w.u16(1);
        w.u16(1);
        w.u16(1);
        w.u16(0);
        w.u16(name.len() as u16);
        w.u32(CRTC + k as u32);
        w.u32(MODE + k as u32);
        w.bytes(name.as_bytes());
        Ok(Reply::new(STATUS_SUCCESS, w))
    }

    fn randr_crtc_info(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let crtc = r.u32()?;
        r.skip(4)?;
        r.end()?;
        let Some(k) = self.monitor_of(crtc, CRTC) else {
            return Err(XError::Extension(first_error(RANDR) + 1, crtc));
        };
        let (_, height) = self.screen_size();
        let (x, width) = self.monitor_columns(k);
        let mut w = Writer::new(req.endian);
        w.u32(self.config_time);
        w.i16(x as i16);
        w.i16(0);
        w.u16(width);
        w.u16(height);
        w.u32(MODE + k as u32);
        w.u16(ROTATE_0);
        w.u16(ROTATE_0);
        w.u16(1);
        w.u16(1);
        w.u32(OUTPUT + k as u32);
        w.u32(OUTPUT + k as u32);
        Ok(Reply::new(STATUS_SUCCESS, w))
    }

    /// The monitors are fixed, so the only configuration that can be set is the current one. Any
    /// other asks for a change that cannot be made, and gets status Failed.
    fn randr_set_crtc_config(&mut self, req: &Request) -> Result<Reply, XError> {
        const STATUS_FAILED: u8 = 3;
        let mut r = req.reader();
        let crtc = r.u32()?;
        r.skip(8)?;
        let (x, y) = (r.i16()?, r.i16()?);
        let mode = r.u32()?;
        let rotation = r.u16()?;
        r.skip(2)?;
        let mut outputs = Vec::new();
        while r.remaining() > 0 {
            outputs.push(r.u32()?);
        }
        let Some(k) = self.monitor_of(crtc, CRTC) else {
            return Err(XError::Extension(first_error(RANDR) + 1, crtc));
        };
        let column_x = self.monitor_columns(k).0 as i16;
        let current = (x, y, mode, rotation) == (column_x, 0, MODE + k as u32, ROTATE_0)
            && outputs == [OUTPUT + k as u32];
        let mut w = Writer::new(req.endian);
        w.u32(self.config_time);
        if current {
            return Ok(Reply::new(STATUS_SUCCESS, w));
        }
        self.log_once("randr set config".into(), || {
            "xsrv-web: RANDR can only keep the current configuration: fixed monitors, no rotation"
                .into()
        });
        Ok(Reply::new(STATUS_FAILED, w))
    }

    /// Outputs have no properties. The tools ask anyway (EDID, backlight).
    fn randr_list_output_properties(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let output = r.u32()?;
        r.end()?;
        self.monitor_of(output, OUTPUT)
            .ok_or(XError::Extension(first_error(RANDR), output))?;
        let mut w = Writer::new(req.endian);
        w.u16(0);
        Ok(Reply::new(0, w))
    }

    fn randr_get_output_property(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let output = r.u32()?;
        r.skip(r.remaining())?;
        self.monitor_of(output, OUTPUT)
            .ok_or(XError::Extension(first_error(RANDR), output))?;
        let mut w = Writer::new(req.endian);
        // No such property: type None, nothing left, no items.
        w.u32(0);
        w.u32(0);
        w.u32(0);
        Ok(Reply::new(0, w))
    }

    /// The screen has no real gamma. The table is the identity and a change is accepted and ignored.
    fn randr_get_crtc_gamma(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let crtc = r.u32()?;
        r.end()?;
        self.monitor_of(crtc, CRTC)
            .ok_or(XError::Extension(first_error(RANDR) + 1, crtc))?;
        let mut w = Writer::new(req.endian);
        w.u16(GAMMA_SIZE);
        w.zeros(22);
        for _channel in 0..3 {
            for step in 0..GAMMA_SIZE {
                w.u16(step * 257);
            }
        }
        Ok(Reply::new(0, w))
    }

    fn randr_set_crtc_gamma(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let crtc = r.u32()?;
        self.monitor_of(crtc, CRTC)
            .ok_or(XError::Extension(first_error(RANDR) + 1, crtc))?;
        let size = r.u16()?;
        if size != GAMMA_SIZE {
            return Err(XError::Value(size.into()));
        }
        Ok(())
    }

    fn randr_get_crtc_gamma_size(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let crtc = r.u32()?;
        r.end()?;
        self.monitor_of(crtc, CRTC)
            .ok_or(XError::Extension(first_error(RANDR) + 1, crtc))?;
        let mut w = Writer::new(req.endian);
        w.u16(GAMMA_SIZE);
        Ok(Reply::new(0, w))
    }

    /// No transforms: both matrices are the identity, and there is no filter.
    fn randr_get_crtc_transform(&self, req: &Request) -> Result<Reply, XError> {
        const ONE: u32 = 0x1_0000;
        let mut r = req.reader();
        let crtc = r.u32()?;
        r.end()?;
        self.monitor_of(crtc, CRTC)
            .ok_or(XError::Extension(first_error(RANDR) + 1, crtc))?;
        let identity = |w: &mut Writer| {
            for cell in 0..9 {
                w.u32(if cell % 4 == 0 { ONE } else { 0 });
            }
        };
        let mut w = Writer::new(req.endian);
        identity(&mut w);
        w.u8(0);
        w.zeros(3);
        identity(&mut w);
        w.zeros(4);
        for _ in 0..4 {
            w.u16(0);
        }
        Ok(Reply::new(0, w))
    }

    /// Panning is not offered: all zeros means that it is off.
    fn randr_get_panning(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let crtc = r.u32()?;
        r.end()?;
        self.monitor_of(crtc, CRTC)
            .ok_or(XError::Extension(first_error(RANDR) + 1, crtc))?;
        let mut w = Writer::new(req.endian);
        w.u32(self.config_time);
        w.zeros(24);
        Ok(Reply::new(STATUS_SUCCESS, w))
    }

    fn randr_output_primary(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        self.window(r.u32()?)?;
        r.end()?;
        let mut w = Writer::new(req.endian);
        w.u32(self.primary_output);
        Ok(Reply::new(0, w))
    }

    /// Gives the screen a new size.
    pub(super) fn set_screen_size(&mut self, width: u16, height: u16) -> Result<(), XError> {
        if !(MIN_SCREEN_SIZE..=MAX_SCREEN_SIZE).contains(&width)
            || !(MIN_SCREEN_SIZE..=MAX_SCREEN_SIZE).contains(&height)
        {
            return Err(XError::Value(u32::from(width) << 16 | u32::from(height)));
        }
        if self.screen_size() == (width, height) {
            return Ok(());
        }
        let surface = self.alloc_surface(width, height)?;
        let root = self.window_mut(ROOT_WINDOW)?;
        let old = replace(&mut root.surface, surface);
        root.width = width;
        root.height = height;
        self.release_surface(&old);
        self.fill_background(ROOT_WINDOW, IRect::new(0, 0, width.into(), height.into()))?;
        self.input.pointer = (
            self.input.pointer.0.min(i32::from(width) - 1),
            self.input.pointer.1.min(i32::from(height) - 1),
        );
        self.display.resize(width, height);
        self.dirty.clear();
        self.mark_dirty(IRect::new(0, 0, width.into(), height.into()));
        self.refit_maximized();
        self.config_time = self.timestamp();
        self.notify_configure(ROOT_WINDOW);
        self.notify_screen_change(width, height);
        self.hold_flush_until =
            Some(std::time::Instant::now() + super::compose::SCREEN_CHANGE_HOLD);
        Ok(())
    }

    fn notify_screen_change(&self, width: u16, height: u16) {
        let _ = width;
        let time = self.timestamp();
        let (mm_width, mm_height) = (millimetres(width) as u16, millimetres(height) as u16);
        for (&client, &mask) in &self.randr_listeners {
            if mask & SELECT_SCREEN_CHANGE != 0 {
                let event = Event::RandrScreenChange {
                    code: event_code(RANDR, SCREEN_CHANGE_EVENT).unwrap_or(0),
                    time,
                    config_time: self.config_time,
                    width,
                    height,
                    mm_width,
                    mm_height,
                };
                self.send_event_to(client, &event);
            }
            if mask & SELECT_CRTC_CHANGE != 0 {
                for k in 0..self.monitors.max(1) {
                    let event = Event::RandrCrtcChange {
                        code: event_code(RANDR, NOTIFY_EVENT).unwrap_or(0),
                        sub_code: CRTC_CHANGE,
                        time,
                        crtc: CRTC + k as u32,
                        mode: MODE + k as u32,
                        width: self.monitor_columns(k).1,
                        height,
                    };
                    self.send_event_to(client, &event);
                }
            }
        }
    }
}

fn write_mode_info(w: &mut Writer, id: u32, width: u16, height: u16, name_len: usize) {
    w.u32(id);
    w.u16(width);
    w.u16(height);
    w.u32(u32::from(width) * u32::from(height) * 60);
    for field in [width, width, width, 0, height, height, height] {
        w.u16(field);
    }
    w.u16(name_len as u16);
    w.u32(0);
}
