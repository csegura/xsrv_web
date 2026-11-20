// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! The XINERAMA extension, version 1.1: the list of the monitors of one screen. The monitors are
//! the virtual ones of RANDR (`XSRV_MONITORS`), side by side. Programs that use XINERAMA to place
//! their windows on a monitor (the Common Desktop Environment, older GTK and Motif programs) wait
//! for the answer, so a server without it can make them stop at the start.
use super::codec::Writer;
use super::dispatch::{Reply, Request};
use super::error::XError;
use super::state::State;

const QUERY_VERSION: u8 = 0;
const GET_STATE: u8 = 1;
const GET_SCREEN_COUNT: u8 = 2;
const GET_SCREEN_SIZE: u8 = 3;
const IS_ACTIVE: u8 = 4;
const QUERY_SCREENS: u8 = 5;

impl State {
    pub(super) fn xinerama_request(&mut self, req: &Request) -> Result<Option<Reply>, XError> {
        let mut r = req.reader();
        let mut w = Writer::new(req.endian);
        let count = self.monitors.max(1);
        match req.detail {
            QUERY_VERSION => {
                r.skip(4)?;
                r.end()?;
                w.u16(1);
                w.u16(1);
                Ok(Some(Reply::new(0, w)))
            }
            // The answers of GetState and GetScreenCount are in the detail byte of the reply.
            GET_STATE | GET_SCREEN_COUNT => {
                let window = r.u32()?;
                r.end()?;
                self.window(window)?;
                w.u32(window);
                let detail = if req.detail == GET_STATE {
                    1
                } else {
                    count as u8
                };
                Ok(Some(Reply::new(detail, w)))
            }
            GET_SCREEN_SIZE => {
                let window = r.u32()?;
                let screen = r.u32()?;
                r.end()?;
                self.window(window)?;
                let index = usize::try_from(screen)
                    .ok()
                    .filter(|index| *index < count)
                    .ok_or(XError::Value(screen))?;
                let (_, height) = self.screen_size();
                w.u32(self.monitor_columns(index).1.into());
                w.u32(height.into());
                w.u32(window);
                w.u32(screen);
                Ok(Some(Reply::new(0, w)))
            }
            IS_ACTIVE => {
                r.end()?;
                w.u32(1);
                Ok(Some(Reply::new(0, w)))
            }
            QUERY_SCREENS => {
                r.end()?;
                let (_, height) = self.screen_size();
                w.u32(count as u32);
                w.zeros(20);
                for index in 0..count {
                    let (x, width) = self.monitor_columns(index);
                    w.i16(x as i16);
                    w.i16(0);
                    w.u16(width);
                    w.u16(height);
                }
                Ok(Some(Reply::new(0, w)))
            }
            _ => Err(XError::Request),
        }
    }
}
