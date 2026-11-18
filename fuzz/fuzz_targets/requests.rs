#![no_main]
// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! The first byte chooses the byte order. The rest is a stream of X11 requests.
use libfuzzer_sys::fuzz_target;
use xsrv_server::x11::Harness;

fuzz_target!(|data: &[u8]| {
    let Some((flag, requests)) = data.split_first() else {
        return;
    };
    let mut harness = Harness::new(flag & 1 == 0);
    harness.feed(requests);
});
