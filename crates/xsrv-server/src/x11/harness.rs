// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! Feeds raw bytes to the request handlers without a socket. For fuzzing and tests: a panic in a
//! handler is not caught here, so a fuzzer sees it.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::sync::Notify;

use crate::display::Display;

use super::codec::Endian;
use super::extensions::MAX_BIG_REQUEST_BYTES;
use super::state::{ClientId, State};

const HEADER_LEN: usize = 4;

pub struct Harness {
    state: Rc<RefCell<State>>,
    client: ClientId,
    endian: Endian,
    big_requests: Rc<Cell<bool>>,
    output: UnboundedReceiver<Vec<u8>>,
}

impl Harness {
    pub fn new(little_endian: bool) -> Self {
        let endian = if little_endian {
            Endian::Little
        } else {
            Endian::Big
        };
        let display = Display::new(super::SCREEN_WIDTH, super::SCREEN_HEIGHT);
        let mut state = State::new(display);
        // The fuzzer must reach the optional extensions too.
        state.set_xkb(true);
        let (tx, output) = unbounded_channel();
        let big_requests = Rc::new(Cell::new(false));
        let (client, _) = state
            .add_client(
                endian,
                tx,
                Rc::new(Cell::new(0)),
                Rc::new(Notify::new()),
                Rc::clone(&big_requests),
            )
            .expect("the first client is accepted");
        Self {
            state: Rc::new(RefCell::new(state)),
            client,
            endian,
            big_requests,
            output,
        }
    }

    /// Splits `data` into requests like the connection does, and handles each one. Stops at the
    /// first request that the connection would refuse (bad length). Returns how many ran.
    pub fn feed(&mut self, data: &[u8]) -> usize {
        let mut count = 0;
        let mut rest = data;
        while rest.len() >= HEADER_LEN {
            let words = usize::from(self.endian.read_u16([rest[2], rest[3]]));
            let (header, total) = if words != 0 {
                (HEADER_LEN, words * 4)
            } else if self.big_requests.get() && rest.len() >= 8 {
                let big = self.endian.read_u32([rest[4], rest[5], rest[6], rest[7]]) as usize;
                match big.checked_mul(4) {
                    Some(bytes) if (8..=MAX_BIG_REQUEST_BYTES).contains(&bytes) => (8, bytes),
                    _ => break,
                }
            } else {
                break;
            };
            if total < header || rest.len() < total {
                break;
            }
            let (opcode, detail) = (rest[0], rest[1]);
            self.state
                .borrow_mut()
                .process(self.client, opcode, detail, &rest[header..total]);
            count += 1;
            rest = &rest[total..];
        }
        while self.output.try_recv().is_ok() {}
        count
    }
}
