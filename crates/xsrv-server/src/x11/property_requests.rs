// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use super::atoms::MAX_ATOM_NAME;
use super::clipboard::CLIPBOARD_WINDOW;
use super::codec::{padding, Writer};
use super::dispatch::{Reply, Request};
use super::error::XError;
use super::event::{Event, PROPERTY_CHANGE};
use super::state::{Property, State};

const MAX_PROPERTY_BYTES: usize = 1 << 20;
const MODE_REPLACE: u8 = 0;
const MODE_PREPEND: u8 = 1;
const MODE_APPEND: u8 = 2;
const ANY_PROPERTY_TYPE: u32 = 0;

impl State {
    pub fn intern_atom(&mut self, req: &Request) -> Result<Reply, XError> {
        let only_if_exists = req.detail != 0;
        let mut r = req.reader();
        let len = r.u16()? as usize;
        r.skip(2)?;
        let name = r.bytes(len)?;
        r.skip(padding(len))?;
        r.end()?;
        if len > MAX_ATOM_NAME {
            return Err(XError::Value(len as u32));
        }
        let atom = if only_if_exists {
            self.atoms.lookup(name).unwrap_or(0)
        } else {
            self.atoms.intern(name).ok_or(XError::Alloc)?
        };
        let mut w = Writer::new(req.endian);
        w.u32(atom);
        Ok(Reply::new(0, w))
    }

    pub fn get_atom_name(&self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let atom = r.u32()?;
        r.end()?;
        let name = self.atoms.name(atom).ok_or(XError::Atom(atom))?;
        let mut w = Writer::new(req.endian);
        w.u16(name.len() as u16);
        w.zeros(22);
        w.bytes(name);
        Ok(Reply::new(0, w))
    }

    pub fn change_property(&mut self, req: &Request) -> Result<(), XError> {
        let mode = req.detail;
        let mut r = req.reader();
        let window = r.u32()?;
        let property = r.u32()?;
        let type_ = r.u32()?;
        let format = r.u8()?;
        r.skip(3)?;
        let units = r.u32()? as usize;
        if mode > MODE_APPEND {
            return Err(XError::Value(mode.into()));
        }
        if !matches!(format, 8 | 16 | 32) {
            return Err(XError::Value(format.into()));
        }
        let byte_len = units
            .checked_mul(usize::from(format / 8))
            .ok_or(XError::Length)?;
        let data = r.bytes(byte_len)?;
        r.skip(padding(byte_len))?;
        r.end()?;

        self.window(window)?;
        self.require_atom(property)?;
        self.require_atom(type_)?;
        // The hidden clipboard window takes whole images and long texts from X owners.
        let limit = if window == CLIPBOARD_WINDOW {
            wire::MAX_CLIPBOARD_PNG
        } else {
            MAX_PROPERTY_BYTES
        };
        if byte_len > limit || byte_len > self.property_quota_left() {
            return Err(XError::Alloc);
        }

        let target = self.window_mut(window)?;
        match target.properties.get_mut(&property) {
            Some(existing) if mode != MODE_REPLACE => {
                if existing.type_ != type_ || existing.format != format {
                    return Err(XError::Match);
                }
                if existing.data.len() + byte_len > limit {
                    return Err(XError::Alloc);
                }
                if mode == MODE_PREPEND {
                    existing.data.splice(0..0, data.iter().copied());
                } else {
                    existing.data.extend_from_slice(data);
                }
            }
            _ => {
                target.properties.insert(
                    property,
                    Property {
                        type_,
                        format,
                        data: data.to_vec(),
                    },
                );
            }
        }
        self.notify_property(window, property, false);
        self.mark_window_dirty(window);
        if window == CLIPBOARD_WINDOW {
            self.clipboard_property_written(req.client, property);
        }
        Ok(())
    }

    pub fn delete_property(&mut self, req: &Request) -> Result<(), XError> {
        let mut r = req.reader();
        let window = r.u32()?;
        let property = r.u32()?;
        r.end()?;
        self.window(window)?;
        self.require_atom(property)?;
        if self
            .window_mut(window)?
            .properties
            .remove(&property)
            .is_some()
        {
            self.notify_property(window, property, true);
        }
        Ok(())
    }

    pub fn get_property(&mut self, req: &Request) -> Result<Reply, XError> {
        let delete = req.detail != 0;
        let mut r = req.reader();
        let window = r.u32()?;
        let property = r.u32()?;
        let type_ = r.u32()?;
        let offset = r.u32()?;
        let length = r.u32()?;
        r.end()?;
        self.window(window)?;
        self.require_atom(property)?;
        if type_ != ANY_PROPERTY_TYPE {
            self.require_atom(type_)?;
        }

        let mut w = Writer::new(req.endian);
        let Some(found) = self.window(window)?.properties.get(&property) else {
            return Ok(Reply::new(0, w));
        };
        let total = found.data.len();
        if type_ != ANY_PROPERTY_TYPE && type_ != found.type_ {
            w.u32(found.type_);
            w.u32(total as u32);
            return Ok(Reply::new(found.format, w));
        }
        let start = (offset as usize)
            .checked_mul(4)
            .filter(|start| *start <= total)
            .ok_or(XError::Value(offset))?;
        let count = (total - start).min((length as usize).saturating_mul(4));
        let bytes_after = total - start - count;
        w.u32(found.type_);
        w.u32(bytes_after as u32);
        w.u32((count / usize::from(found.format / 8)) as u32);
        w.zeros(12);
        w.bytes(&found.data[start..start + count]);
        let format = found.format;
        if delete && bytes_after == 0 {
            self.window_mut(window)?.properties.remove(&property);
            self.notify_property(window, property, true);
        }
        Ok(Reply::new(format, w))
    }

    pub(super) fn require_atom(&self, atom: u32) -> Result<(), XError> {
        if self.atoms.contains(atom) {
            Ok(())
        } else {
            Err(XError::Atom(atom))
        }
    }

    pub(super) fn notify_property(&self, window: u32, atom: u32, deleted: bool) {
        self.notify_listeners(
            window,
            PROPERTY_CHANGE,
            &Event::PropertyNotify {
                window,
                atom,
                time: self.timestamp(),
                deleted,
            },
        );
    }
}
