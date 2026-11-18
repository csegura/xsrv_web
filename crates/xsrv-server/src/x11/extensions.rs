// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! The list of extensions, and the extension-independent ones: BIG-REQUESTS and XC-MISC.
use super::codec::{padding, Writer};
use super::dispatch::{Reply, Request};
use super::error::XError;
use super::screen::{RESOURCE_ID_MASK, RESOURCE_ID_SHIFT};
use super::state::State;

pub const FIRST_EXTENSION_OPCODE: u8 = 128;

pub struct ExtensionInfo {
    pub name: &'static str,
    pub major_opcode: u8,
    pub first_event: u8,
    pub first_error: u8,
    /// How many event and error codes the extension uses from its first ones.
    pub events: u8,
    pub errors: u8,
}

pub const SHAPE: u8 = 129;
pub const MIT_SHM: u8 = 130;
pub const XFIXES: u8 = 138;
pub const RANDR: u8 = 140;
pub const RENDER: u8 = 139;
pub const BIG_REQUESTS: u8 = 133;
pub const XC_MISC: u8 = 134;
pub const XKEYBOARD: u8 = 135;

/// The largest request after BIG-REQUESTS is enabled, in bytes.
pub const MAX_BIG_REQUEST_BYTES: usize = 4 << 20;

pub const EXTENSIONS: &[ExtensionInfo] = &[
    ExtensionInfo {
        name: "MIT-SHM",
        major_opcode: MIT_SHM,
        first_event: 65,
        first_error: 128,
        events: 1,
        errors: 1,
    },
    ExtensionInfo {
        name: "XFIXES",
        major_opcode: XFIXES,
        first_event: 66,
        first_error: 129,
        events: 2,
        errors: 1,
    },
    ExtensionInfo {
        name: "RANDR",
        major_opcode: RANDR,
        first_event: 68,
        first_error: 130,
        events: 2,
        errors: 4,
    },
    ExtensionInfo {
        name: "SHAPE",
        major_opcode: SHAPE,
        first_event: 64,
        first_error: 0,
        events: 1,
        errors: 0,
    },
    ExtensionInfo {
        name: "BIG-REQUESTS",
        major_opcode: BIG_REQUESTS,
        first_event: 0,
        first_error: 0,
        events: 0,
        errors: 0,
    },
    ExtensionInfo {
        name: "XC-MISC",
        major_opcode: XC_MISC,
        first_event: 0,
        first_error: 0,
        events: 0,
        errors: 0,
    },
    ExtensionInfo {
        name: "RENDER",
        major_opcode: RENDER,
        first_event: 0,
        // After the 4 errors of RANDR (130 to 133).
        first_error: 134,
        events: 0,
        errors: 5,
    },
    ExtensionInfo {
        name: "XKEYBOARD",
        major_opcode: XKEYBOARD,
        // After the 2 events of RANDR (68, 69).
        first_event: 70,
        // After the 5 errors of RENDER (134 to 138).
        first_error: 139,
        events: 1,
        errors: 1,
    },
];

/// The event code of an extension event: the first event of the extension plus `offset`.
pub fn event_code(major_opcode: u8, offset: u8) -> Option<u8> {
    let ext = EXTENSIONS
        .iter()
        .find(|ext| ext.major_opcode == major_opcode)?;
    (offset < ext.events).then(|| ext.first_event + offset)
}

/// The first error code of an extension, or 0 when it defines no errors.
pub fn first_error(major_opcode: u8) -> u8 {
    EXTENSIONS
        .iter()
        .find(|ext| ext.major_opcode == major_opcode && ext.errors > 0)
        .map_or(0, |ext| ext.first_error)
}

pub fn find(name: &[u8]) -> Option<&'static ExtensionInfo> {
    EXTENSIONS.iter().find(|ext| ext.name.as_bytes() == name)
}

impl State {
    pub(super) fn query_extension(&mut self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let len = r.u16()? as usize;
        r.skip(2)?;
        let name = r.bytes(len)?;
        r.skip(padding(len))?;
        r.end()?;
        let mut w = Writer::new(req.endian);
        match find(name).filter(|ext| self.offers(ext, req.endian)) {
            Some(ext) => {
                w.u8(1);
                w.u8(ext.major_opcode);
                w.u8(ext.first_event);
                w.u8(ext.first_error);
            }
            None => {
                let name = String::from_utf8_lossy(name).into_owned();
                self.log_once(format!("extension {name}"), || {
                    format!("xsrv-web: extension not supported: {name:?}")
                });
            }
        }
        Ok(Reply::new(0, w))
    }

    /// XKEYBOARD is optional (`Options::xkb`) and only for clients with the server's byte order.
    fn offers(&self, ext: &ExtensionInfo, endian: super::codec::Endian) -> bool {
        ext.major_opcode != XKEYBOARD || self.offers_xkb(endian)
    }

    pub(super) fn list_extensions(&self, req: &Request) -> Result<Reply, XError> {
        req.reader().end()?;
        let mut w = Writer::new(req.endian);
        w.zeros(24);
        let offered: Vec<_> = EXTENSIONS
            .iter()
            .filter(|ext| self.offers(ext, req.endian))
            .collect();
        for ext in &offered {
            w.u8(ext.name.len() as u8);
            w.bytes(ext.name.as_bytes());
        }
        Ok(Reply::new(offered.len() as u8, w))
    }

    pub(super) fn extension_request(&mut self, req: &Request) -> Result<Option<Reply>, XError> {
        let result = match req.opcode {
            SHAPE => self.shape_request(req),
            RANDR => self.randr_request(req),
            XFIXES => self.xfixes_request(req),
            MIT_SHM => self.shm_request(req),
            BIG_REQUESTS => self.big_requests(req).map(Some),
            XC_MISC => self.xc_misc(req).map(Some),
            RENDER => self.render_request(req),
            XKEYBOARD if self.offers_xkb(req.endian) => self.xkb_request(req),
            _ => Err(XError::Request),
        };
        if matches!(result, Err(XError::Request)) {
            let name = EXTENSIONS
                .iter()
                .find(|ext| ext.major_opcode == req.opcode)
                .map_or("unknown", |ext| ext.name);
            let (opcode, minor) = (req.opcode, req.detail);
            self.log_once(format!("minor {opcode} {minor}"), || {
                format!("xsrv-web: request {minor} of {name} is not supported")
            });
        }
        result
    }

    /// BigReqEnable: from now on a request may have a 32-bit length.
    fn big_requests(&mut self, req: &Request) -> Result<Reply, XError> {
        if req.detail != 0 {
            return Err(XError::Request);
        }
        req.reader().end()?;
        self.enable_big_requests(req.client);
        let mut w = Writer::new(req.endian);
        w.u32((MAX_BIG_REQUEST_BYTES / 4) as u32);
        Ok(Reply::new(0, w))
    }

    fn xc_misc(&mut self, req: &Request) -> Result<Reply, XError> {
        let mut r = req.reader();
        let mut w = Writer::new(req.endian);
        match req.detail {
            0 => {
                r.skip(4)?;
                r.end()?;
                w.u16(1);
                w.u16(1);
            }
            1 => {
                r.end()?;
                let (start, count) = self.largest_free_id_range(req.client);
                w.u32(start);
                w.u32(count);
            }
            2 => {
                let wanted = r.u32()? as usize;
                r.end()?;
                let ids = self.free_ids(req.client, wanted.min(MAX_IDS_IN_LIST));
                w.u32(ids.len() as u32);
                w.zeros(20);
                for id in ids {
                    w.u32(id);
                }
            }
            _ => return Err(XError::Request),
        }
        Ok(Reply::new(0, w))
    }

    /// The ids of a client that no resource uses, as sorted ranges (first id, count).
    fn free_id_ranges(&self, client: u32) -> Vec<(u32, u32)> {
        let base = client << RESOURCE_ID_SHIFT;
        let mut used: Vec<u32> = self
            .owners
            .iter()
            .filter(|(id, owner)| **owner == client && self.owns_id(client, **id))
            .map(|(id, _)| *id)
            .collect();
        used.sort_unstable();
        let mut ranges = Vec::new();
        let mut next = base;
        let end = base + RESOURCE_ID_MASK;
        for id in used {
            if id > next {
                ranges.push((next, id - next));
            }
            next = id + 1;
        }
        if next <= end {
            ranges.push((next, end - next + 1));
        }
        ranges
    }

    fn largest_free_id_range(&self, client: u32) -> (u32, u32) {
        self.free_id_ranges(client)
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .unwrap_or((0, 0))
    }

    fn free_ids(&self, client: u32, wanted: usize) -> Vec<u32> {
        self.free_id_ranges(client)
            .into_iter()
            .flat_map(|(start, count)| (0..count).map(move |offset| start + offset))
            .take(wanted)
            .collect()
    }
}

const MAX_IDS_IN_LIST: usize = 4096;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_found_by_exact_name() {
        assert_eq!(find(b"BIG-REQUESTS").unwrap().major_opcode, BIG_REQUESTS);
        assert!(find(b"big-requests").is_none());
        assert!(find(b"NOPE").is_none());
    }

    #[test]
    fn opcodes_and_events_do_not_overlap() {
        let mut opcodes: Vec<u8> = EXTENSIONS.iter().map(|e| e.major_opcode).collect();
        opcodes.sort_unstable();
        opcodes.dedup();
        assert_eq!(opcodes.len(), EXTENSIONS.len());
        assert!(opcodes.iter().all(|op| *op >= FIRST_EXTENSION_OPCODE));

        let ranges = |first: fn(&ExtensionInfo) -> u8, count: fn(&ExtensionInfo) -> u8| {
            let mut codes: Vec<u8> = EXTENSIONS
                .iter()
                .flat_map(|e| (0..count(e)).map(move |i| first(e) + i))
                .collect();
            let total = codes.len();
            codes.sort_unstable();
            codes.dedup();
            assert_eq!(codes.len(), total, "two extensions share a code");
        };
        ranges(|e| e.first_event, |e| e.events);
        ranges(|e| e.first_error, |e| e.errors);
    }
}
