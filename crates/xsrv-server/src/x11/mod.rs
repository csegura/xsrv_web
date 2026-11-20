// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

mod atoms;
mod clipboard;
mod codec;
mod color_requests;
mod colors;
mod compose;
mod connection;
mod cursor_requests;
mod dispatch;
mod drawing_requests;
mod error;
mod event;
mod ext_randr;
mod ext_shape;
mod ext_shm;
mod ext_xfixes;
mod ext_xfixes_region;
mod ext_xinerama;
mod ext_xkb;
mod extensions;
mod font;
mod graphics_requests;
mod harness;
mod input;
mod input_requests;
mod keymap;
mod message_requests;
mod property_requests;
mod raster;
mod region;
mod render;
mod resources;
mod retained;
mod screen;
mod setup;
#[allow(unsafe_code)]
mod shm;
mod state;
mod text_requests;
mod window_requests;
mod wm;
mod xkb_map;
mod zpixmap;

use std::cell::{Cell, RefCell};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use tokio::net::UnixListener;

use crate::display::{Display, PageEvent};
use state::State;

pub use ext_randr::MAX_MONITORS;
pub use harness::Harness;
pub use keymap::Layout;
pub use screen::{SCREEN_HEIGHT, SCREEN_WIDTH};

/// How long drawing may wait for the end of a burst of requests that have no reply.
const FLUSH_EVERY: Duration = Duration::from_millis(2);
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Creates the socket. A socket file that no server listens on is removed first.
/// Errors name the path, because the usual cause is a directory that is not writable.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    prepare(path)
        .and_then(|()| UnixListener::bind(path))
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "X11 socket {}: {error} (set XSRV_X11_SOCKET to a writable path)",
                    path.display()
                ),
            )
        })
}

fn prepare(path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "an X server already listens here",
            ));
        }
        fs::remove_file(path)?;
    }
    Ok(())
}

/// The socket file of a running server. It is removed when this value is dropped, at the end of
/// the server, but only when it is still the file that this server created: a newer server may
/// have replaced it.
pub struct SocketFile {
    path: PathBuf,
    id: Option<FileId>,
}

/// Device, inode and change time. A newer file can get the same inode number when the old file
/// was removed first, as a new server does with a stale socket; its change time differs.
type FileId = (u64, u64, i64, i64);

impl SocketFile {
    /// Call it right after `bind`, so that the file is the one this server created.
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_owned(),
            id: file_id(path),
        }
    }
}

impl Drop for SocketFile {
    fn drop(&mut self) {
        if self.id.is_some() && file_id(&self.path) == self.id {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn file_id(path: &Path) -> Option<FileId> {
    fs::symlink_metadata(path)
        .ok()
        .map(|meta| (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec()))
}

/// What the X11 server does besides the protocol.
#[derive(Clone, Copy, Default)]
pub struct Options {
    /// Draw frames and manage top-level windows. Off by default.
    pub window_manager: bool,
    /// The keyboard layout. US by default.
    pub layout: keymap::Layout,
    /// The built-in manager makes each top-level window fill the screen and follow its size.
    pub maximize: bool,
    /// How many virtual monitors RANDR reports (1 to 4). 0 is the same as 1.
    pub monitors: usize,
    /// The 16-byte cookie that X clients must show (MIT-MAGIC-COOKIE-1). `None`: no check.
    pub cookie: Option<[u8; 16]>,
    /// Offer the XKEYBOARD extension (the server keymap for libxkbcommon and Xlib clients).
    pub xkb: bool,
}

/// Accepts X11 clients without a window manager. Call it inside a `tokio::task::LocalSet`.
pub async fn serve(listener: UnixListener, display: Display) -> io::Result<()> {
    serve_with(listener, display, Options::default()).await
}

pub async fn serve_with(
    listener: UnixListener,
    display: Display,
    options: Options,
) -> io::Result<()> {
    let input = display.take_input();
    let state = Rc::new(RefCell::new(State::new(display)));
    state
        .borrow_mut()
        .set_window_manager(options.window_manager);
    state.borrow_mut().set_layout(options.layout);
    state.borrow_mut().set_maximize(options.maximize);
    state.borrow_mut().set_monitors(options.monitors);
    state.borrow_mut().set_xkb(options.xkb);
    state
        .borrow_mut()
        .set_cookie(options.cookie.map(|cookie| cookie.to_vec()));
    if let Some(mut input) = input {
        let state = Rc::clone(&state);
        tokio::task::spawn_local(async move {
            while let Some(event) = input.recv().await {
                let handled = catch_unwind(AssertUnwindSafe(|| match event {
                    PageEvent::Input(input) => state.borrow_mut().handle_input(input),
                    PageEvent::Clipboard(data) => state.borrow_mut().page_clipboard(data),
                }));
                if handled.is_err() {
                    eprintln!("xsrv-web: internal error while handling page input");
                }
            }
        });
    }
    {
        // Draws what a burst of requests without a reply changed, a few milliseconds later. It
        // ticks only while there is such work, and otherwise waits to be woken.
        let state = Rc::clone(&state);
        let wake = state.borrow().tick_wake();
        tokio::task::spawn_local(async move {
            loop {
                let (ticks, release_at) = {
                    let state = state.borrow();
                    (state.needs_ticks(), state.buffers_release_at())
                };
                if ticks {
                    tokio::time::sleep(FLUSH_EVERY).await;
                } else if let Some(at) = release_at {
                    // One wake after a quiet time gives the work buffers back.
                    tokio::select! {
                        () = tokio::time::sleep_until(at.into()) => {}
                        () = wake.notified() => {}
                    }
                } else {
                    wake.notified().await;
                }
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    let mut state = state.borrow_mut();
                    state.thaw_stale_pointer();
                    state.flush_display_if_settled();
                    state.release_buffers_if_quiet();
                }));
            }
        });
    }
    let pending_setups = Rc::new(Cell::new(0));
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(error) => {
                eprintln!("xsrv-web: accept failed: {error}");
                tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
                continue;
            }
        };
        let Some(slot) = connection::SetupSlot::take(&pending_setups) else {
            state.borrow_mut().log_once("setup limit".into(), || {
                format!(
                    "xsrv-web: {} connections wait for their setup; new ones are closed",
                    connection::MAX_PENDING_SETUPS
                )
            });
            continue;
        };
        let state = Rc::clone(&state);
        tokio::task::spawn_local(async move {
            if let Err(error) = connection::run_client(stream, state, slot).await {
                eprintln!("xsrv-web: client closed: {error}");
            }
        });
    }
}

#[cfg(test)]
mod socket_file_tests {
    use super::*;

    #[test]
    fn the_socket_file_goes_away_unless_another_server_replaced_it() {
        let dir = std::env::temp_dir().join(format!("xsrv-socket-file-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("X1");
        fs::write(&path, b"").unwrap();
        drop(SocketFile::new(&path));
        assert!(!path.exists(), "removed at the end of the server");

        fs::write(&path, b"").unwrap();
        let ours = SocketFile::new(&path);
        fs::remove_file(&path).unwrap();
        fs::write(dir.join("other"), b"").unwrap();
        fs::rename(dir.join("other"), &path).unwrap();
        drop(ours);
        assert!(path.exists(), "the file of a newer server stays");
        fs::remove_dir_all(&dir).unwrap();
    }
}
