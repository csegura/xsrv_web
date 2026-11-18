// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use super::codec::{padded, Endian, Reader, Writer};
use super::error::XError;
use super::extensions::FIRST_EXTENSION_OPCODE;
use super::state::{ClientId, State};

pub struct Request<'a> {
    pub client: ClientId,
    pub endian: Endian,
    pub opcode: u8,
    pub detail: u8,
    pub body: &'a [u8],
}

impl Request<'_> {
    pub fn reader(&self) -> Reader<'_> {
        Reader::new(self.body, self.endian)
    }
}

pub struct Reply {
    detail: u8,
    body: Vec<u8>,
}

const REPLY_FIXED_LEN: usize = 24;

impl Reply {
    pub fn new(detail: u8, body: Writer) -> Self {
        Self {
            detail,
            body: body.finish(),
        }
    }

    /// A reply that was encoded whole (by `x11rb-protocol`): the header is written again by `encode`.
    pub fn from_serialized(bytes: &[u8]) -> Self {
        assert!(bytes.len() >= 32, "a whole reply has at least 32 bytes");
        Self {
            detail: bytes[1],
            body: bytes[8..].to_vec(),
        }
    }

    pub(super) fn encode(mut self, endian: Endian, seq: u16) -> Vec<u8> {
        self.body
            .resize(padded(self.body.len()).max(REPLY_FIXED_LEN), 0);
        let mut out = Writer::new(endian);
        out.u8(1);
        out.u8(self.detail);
        out.u16(seq);
        out.u32(((self.body.len() - REPLY_FIXED_LEN) / 4) as u32);
        out.bytes(&self.body);
        out.finish()
    }
}

fn error_packet(endian: Endian, error: XError, seq: u16, (opcode, minor): (u8, u8)) -> Vec<u8> {
    let mut out = Writer::new(endian);
    out.u8(0);
    out.u8(error.code());
    out.u16(seq);
    out.u32(error.bad_value());
    out.u16(minor.into());
    out.u8(opcode);
    out.zeros(21);
    out.finish()
}

fn no_reply(_: ()) -> Option<Reply> {
    None
}

impl State {
    /// Runs one request and queues the reply or the error for the client.
    pub fn process(&mut self, client: ClientId, opcode: u8, detail: u8, body: &[u8]) {
        let (Some(endian), Some(seq)) = (self.client_endian(client), self.next_sequence(client))
        else {
            return;
        };
        let request = Request {
            client,
            endian,
            opcode,
            detail,
            body,
        };
        // A request with a reply or an error is a point where the client looks at the result, so
        // the screen is drawn at once. A burst of requests without a reply is drawn once, by the
        // timer of `serve_with` or by the next request that needs it.
        self.last_request = Some(std::time::Instant::now());
        // GetImage reads the screen, so the screen must be up to date before it runs.
        const GET_IMAGE: u8 = 73;
        if opcode == GET_IMAGE {
            self.flush_display();
        }
        let mut sync_point = true;
        match self.dispatch(&request) {
            Ok(Some(reply)) => self.send_raw(client, reply.encode(endian, seq)),
            Ok(None) => sync_point = false,
            Err(error) => {
                let minor = if opcode >= FIRST_EXTENSION_OPCODE {
                    detail
                } else {
                    0
                };
                self.send_raw(client, error_packet(endian, error, seq, (opcode, minor)));
            }
        }
        self.after_request();
        if sync_point {
            self.flush_display();
        }
    }

    fn dispatch(&mut self, req: &Request) -> Result<Option<Reply>, XError> {
        match req.opcode {
            1 => self.create_window(req).map(no_reply),
            2 => self.change_window_attributes(req).map(no_reply),
            3 => self.get_window_attributes(req).map(Some),
            4 => self.destroy_window_request(req).map(no_reply),
            5 => self.destroy_subwindows(req).map(no_reply),
            6 => self.change_save_set(req).map(no_reply),
            7 => self.reparent_window(req).map(no_reply),
            8 => self.map_window(req).map(no_reply),
            9 => self.map_subwindows(req).map(no_reply),
            10 => self.unmap_window(req).map(no_reply),
            11 => self.unmap_subwindows(req).map(no_reply),
            12 => self.configure_window(req).map(no_reply),
            14 => self.get_geometry(req).map(Some),
            15 => self.query_tree(req).map(Some),
            16 => self.intern_atom(req).map(Some),
            17 => self.get_atom_name(req).map(Some),
            18 => self.change_property(req).map(no_reply),
            19 => self.delete_property(req).map(no_reply),
            20 => self.get_property(req).map(Some),
            22 => self.set_selection_owner(req).map(no_reply),
            23 => self.get_selection_owner(req).map(Some),
            24 => self.convert_selection(req).map(no_reply),
            25 => self.send_event(req).map(no_reply),
            26 => self.grab_pointer(req).map(Some),
            27 => self.ungrab_pointer(req).map(no_reply),
            30 => self.change_active_pointer_grab(req).map(no_reply),
            28 => self.grab_button(req).map(no_reply),
            29 => self.ungrab_button(req).map(no_reply),
            31 => self.grab_keyboard(req).map(Some),
            32 => self.ungrab_keyboard(req).map(no_reply),
            33 => self.grab_key(req).map(no_reply),
            34 => self.ungrab_key(req).map(no_reply),
            35 => self.allow_events(req).map(no_reply),
            36 | 37 => self.grab_server(req).map(no_reply),
            38 => self.query_pointer(req).map(Some),
            40 => self.translate_coordinates(req).map(Some),
            42 => self.set_input_focus(req).map(no_reply),
            43 => self.get_input_focus(req).map(Some),
            44 => self.query_keymap(req).map(Some),
            45 => self.open_font(req).map(no_reply),
            46 => self.close_font(req).map(no_reply),
            47 => self.query_font(req).map(Some),
            49 => self.list_fonts(req).map(Some),
            50 => self.list_fonts_with_info(req),
            112 => self.set_close_down_mode(req).map(no_reply),
            113 => self.kill_client(req).map(no_reply),
            53 => self.create_pixmap(req).map(no_reply),
            54 => self.free_pixmap(req).map(no_reply),
            55 => self.create_gc(req).map(no_reply),
            56 => self.change_gc(req).map(no_reply),
            59 => self.set_clip_rectangles(req).map(no_reply),
            60 => self.free_gc(req).map(no_reply),
            61 => self.clear_area(req).map(no_reply),
            62 => self.copy_area(req).map(no_reply),
            63 => self.copy_plane(req).map(no_reply),
            64 => self.poly_point(req).map(no_reply),
            65 => self.poly_line(req).map(no_reply),
            66 => self.poly_segment(req).map(no_reply),
            67 => self.poly_rectangle(req).map(no_reply),
            68 => self.poly_arc(req).map(no_reply),
            69 => self.fill_poly(req).map(no_reply),
            70 => self.poly_fill_rectangle(req).map(no_reply),
            71 => self.poly_fill_arc(req).map(no_reply),
            72 => self.put_image(req).map(no_reply),
            73 => self.get_image(req).map(Some),
            74 => self.poly_text8(req).map(no_reply),
            75 => self.poly_text16(req).map(no_reply),
            76 => self.image_text8(req).map(no_reply),
            77 => self.image_text16(req).map(no_reply),
            84 => self.alloc_color(req).map(Some),
            85 => self.alloc_named_color(req).map(Some),
            88 => self.free_colors(req).map(no_reply),
            91 => self.query_colors(req).map(Some),
            92 => self.lookup_color(req).map(Some),
            93 => self.create_cursor(req).map(no_reply),
            94 => self.create_glyph_cursor(req).map(no_reply),
            95 => self.free_cursor(req).map(no_reply),
            96 => self.recolor_cursor(req).map(no_reply),
            97 => self.query_best_size(req).map(Some),
            98 => self.query_extension(req).map(Some),
            99 => self.list_extensions(req).map(Some),
            opcode if opcode >= FIRST_EXTENSION_OPCODE => self.extension_request(req),
            101 => self.get_keyboard_mapping(req).map(Some),
            104 => self.bell(req).map(no_reply),
            117 => self.get_pointer_mapping(req).map(Some),
            119 => self.get_modifier_mapping(req).map(Some),
            127 => Ok(None),
            opcode => {
                self.log_once(format!("opcode {opcode}"), || {
                    format!("xsrv-web: unsupported request opcode {opcode}")
                });
                Err(XError::Request)
            }
        }
    }
}
