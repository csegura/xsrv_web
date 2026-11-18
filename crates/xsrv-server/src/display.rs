// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use bytes::Bytes;
use tokio::sync::{broadcast, mpsc};
use wire::{ClipboardSource, CursorShape, Input, Rect};

use crate::clipboard_set::ClipboardData;
use crate::framebuffer::{Framebuffer, RegionError};
use crate::tile::TileEncoder;
pub use crate::tile::{decode_tile, Compression};

const TILE_CHANNEL_CAPACITY: usize = 64;

/// One message for the pages. Every page gets the same bytes, without a copy.
pub type TileMessage = Bytes;
/// What a new page gets first: the screen size, a tile of the whole screen, the pointer shape.
pub type Snapshot = (Vec<u8>, Vec<u8>, Vec<u8>);
/// What a page sends to the X11 server. One channel carries all of it, so that the order in which
/// the page sent it (clipboard text, then Ctrl+V) is the order in which the server handles it.
#[derive(Debug, PartialEq, Eq)]
pub enum PageEvent {
    Input(Input),
    Clipboard(ClipboardData),
}

pub type InputReceiver = mpsc::UnboundedReceiver<PageEvent>;

/// The screen that the web page shows. The X11 server writes to it, and web sessions read it.
#[derive(Clone)]
pub struct Display {
    framebuffer: Arc<Mutex<Framebuffer>>,
    tiles: broadcast::Sender<TileMessage>,
    /// Copy messages that wait for the tiles of the same burst (see `copy`). Used only while the
    /// framebuffer is locked, so they keep their order with the tiles.
    held: Arc<Mutex<Vec<TileMessage>>>,
    /// Used only while the framebuffer is locked, as `held` is.
    encoder: Arc<Mutex<TileEncoder>>,
    /// In video mode: a copy moved pixels and the end of its burst must count it as a change.
    /// Counted then and not at once, so that the encoder never sees half of a move.
    copied_unannounced: Arc<AtomicBool>,
    cursor: Arc<Mutex<CursorShape>>,
    /// The keyboard layout of the session, as an index into `wire::LAYOUTS`.
    layout: Arc<AtomicU8>,
    input: mpsc::UnboundedSender<PageEvent>,
    /// True when the pages get the screen as an H.264 stream and not as tiles.
    video: Arc<AtomicBool>,
    /// Grows at each change of the pixels. The video task sends a frame when it grows.
    frame_version: Arc<AtomicU64>,
    /// The last clipboard message that X gave to the pages (as the pages got it), with a number
    /// that grows at each new one.
    clipboard: Arc<Mutex<(u64, Vec<u8>)>>,
    input_receiver: Arc<Mutex<Option<InputReceiver>>>,
}

impl Display {
    pub fn new(width: u16, height: u16) -> Self {
        let (tiles, _) = broadcast::channel(TILE_CHANNEL_CAPACITY);
        let (input, input_receiver) = mpsc::unbounded_channel();
        Self {
            framebuffer: Arc::new(Mutex::new(Framebuffer::new(width, height))),
            tiles,
            held: Arc::new(Mutex::new(Vec::new())),
            encoder: Arc::new(Mutex::new(TileEncoder::default())),
            copied_unannounced: Arc::new(AtomicBool::new(false)),
            cursor: Arc::new(Mutex::new(CursorShape::Default)),
            layout: Arc::new(AtomicU8::new(0)),
            input,
            video: Arc::new(AtomicBool::new(false)),
            frame_version: Arc::new(AtomicU64::new(0)),
            clipboard: Arc::new(Mutex::new((0, Vec::new()))),
            input_receiver: Arc::new(Mutex::new(Some(input_receiver))),
        }
    }

    pub fn size(&self) -> (u16, u16) {
        self.lock().size()
    }

    /// Gives the screen a new size, empty and black, and tells every page the size. The caller
    /// draws the screen again. No black tile is sent first: it made the page flicker.
    pub fn resize(&self, width: u16, height: u16) {
        let mut framebuffer = self.lock();
        self.send_held(&framebuffer);
        *framebuffer = Framebuffer::new_unsent(width, height);
        self.frame_version.fetch_add(1, Ordering::Release);
        let _ = self
            .tiles
            .send(Bytes::copy_from_slice(&wire::encode_screen(width, height)));
    }

    /// Passes pointer and key input from a web page to the X11 server.
    /// The input is dropped when the X11 server is gone.
    pub fn send_input(&self, input: Input) {
        let _ = self.input.send(PageEvent::Input(input));
    }

    /// The X11 server takes the receiving end once.
    pub fn take_input(&self) -> Option<InputReceiver> {
        self.input_receiver
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    /// Passes what the clipboard of a web page holds to the X11 server.
    pub fn send_clipboard_in(&self, data: ClipboardData) {
        let _ = self.input.send(PageEvent::Clipboard(data));
    }

    /// Gives what an X client copied to every page. Nothing goes out when there is nothing to
    /// copy, or when the set is too big for a message.
    pub fn publish_clipboard(&self, source: ClipboardSource, data: &ClipboardData) {
        if data.is_empty() {
            return;
        }
        let Some(message) = data.encode_for_page(source) else {
            return;
        };
        {
            let mut last = self.clipboard.lock().unwrap_or_else(|p| p.into_inner());
            *last = (last.0 + 1, message.clone());
        }
        let _ = self.tiles.send(Bytes::from(message));
    }

    /// The number of the newest clipboard message. A page keeps it to know what it has seen.
    pub fn clipboard_version(&self) -> u64 {
        self.clipboard.lock().unwrap_or_else(|p| p.into_inner()).0
    }

    /// The newest clipboard message, when it is newer than `seen`. A page that fell behind lost
    /// messages, and this is the one that it may have lost.
    pub fn clipboard_after(&self, seen: u64) -> Option<(u64, Vec<u8>)> {
        let last = self.clipboard.lock().unwrap_or_else(|p| p.into_inner());
        (last.0 > seen).then(|| (last.0, last.1.clone()))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TileMessage> {
        self.tiles.subscribe()
    }

    /// Tells every page which pointer shape to show.
    pub fn set_cursor(&self, shape: CursorShape) {
        let mut current = self.cursor.lock().unwrap_or_else(|p| p.into_inner());
        if *current == shape {
            return;
        }
        *current = shape;
        let _ = self
            .tiles
            .send(Bytes::copy_from_slice(&wire::encode_cursor(shape)));
    }

    /// Tells every page the keyboard layout of the session (an index into `wire::LAYOUTS`).
    pub fn set_layout(&self, layout: u8) {
        if self.layout.swap(layout, Ordering::Relaxed) != layout {
            let _ = self
                .tiles
                .send(Bytes::copy_from_slice(&wire::encode_layout(layout)));
        }
    }

    /// The layout message for a page that has just subscribed. A page may get it twice (from
    /// here and from the channel); applying it again changes nothing.
    pub fn layout_message(&self) -> Vec<u8> {
        wire::encode_layout(self.layout.load(Ordering::Relaxed)).to_vec()
    }

    /// A receiver of the screen messages, and a snapshot of the screen that holds all messages
    /// sent before it. A page must not apply a message twice: a copy message moves pixels again.
    /// Screen messages are sent with the framebuffer locked, so the two cannot overlap.
    pub fn subscribe_with_snapshot(&self) -> (broadcast::Receiver<TileMessage>, Snapshot) {
        let framebuffer = self.lock();
        // The snapshot holds the held copies already: the old pages get them, the new one not.
        self.send_held(&framebuffer);
        (self.tiles.subscribe(), self.snapshot_of(&framebuffer))
    }

    fn snapshot_of(&self, framebuffer: &Framebuffer) -> Snapshot {
        let (width, height) = framebuffer.size();
        let screen = wire::encode_screen(width, height).to_vec();
        let full = self
            .encoder()
            .tile(framebuffer.full_rect(), framebuffer.pixels());
        let shape = *self.cursor.lock().unwrap_or_else(|p| p.into_inner());
        (screen, full, wire::encode_cursor(shape).to_vec())
    }

    /// Writes BGRX pixels and tells every web session about the parts that changed. Pixels that
    /// are already on the screen are not sent again.
    pub fn update(&self, rect: Rect, pixels: &[u8]) -> Result<(), RegionError> {
        let mut framebuffer = self.lock();
        let bands = framebuffer.changed_bands(rect, pixels)?;
        if bands.is_empty() {
            return Ok(());
        }
        for band in &bands {
            framebuffer.write_part(rect, pixels, *band);
        }
        self.frame_version.fetch_add(1, Ordering::Release);
        if self.video_enabled() {
            return Ok(());
        }
        self.send_held(&framebuffer);
        // No receiver means that no page is open. A new page gets a snapshot, so no tile is
        // built.
        if self.tiles.receiver_count() == 0 {
            return Ok(());
        }
        let mut encoder = self.encoder();
        for band in bands {
            let _ = self
                .tiles
                .send(Bytes::from(encoder.band(rect, pixels, band)));
        }
        Ok(())
    }

    /// Gives the work buffers of the tile encoder back to the system.
    pub fn release_buffers(&self) {
        self.encoder().release();
    }

    fn encoder(&self) -> MutexGuard<'_, TileEncoder> {
        self.encoder
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Moves the pixels of `from` on the screen to `to`, and tells the pages to do the same. A
    /// scroll costs one short message instead of a tile of the whole moved area. Returns false,
    /// and moves nothing, when the pages do not show the screen yet (after a resize).
    ///
    /// The message is held until the next tiles or `release_held`: a client scrolls, then draws
    /// the new line a few milliseconds later. Sent at once, the copy alone would show the old line
    /// twice for a frame.
    pub fn copy(&self, from: Rect, to: (u16, u16)) -> Result<bool, RegionError> {
        let mut framebuffer = self.lock();
        if framebuffer.is_unsent() {
            return Ok(false);
        }
        framebuffer.copy_rect(from, to)?;
        if self.video_enabled() {
            self.copied_unannounced.store(true, Ordering::Release);
        } else {
            self.frame_version.fetch_add(1, Ordering::Release);
            self.held_messages()
                .push(Bytes::copy_from_slice(&wire::encode_copy(from, to)));
        }
        Ok(true)
    }

    /// Sends the held copy messages. The end of a burst calls it.
    pub fn release_held(&self) {
        let framebuffer = self.lock();
        self.send_held(&framebuffer);
        if self.copied_unannounced.swap(false, Ordering::AcqRel) {
            self.frame_version.fetch_add(1, Ordering::Release);
        }
    }

    /// True while copy messages wait for the end of their burst.
    pub fn holds_messages(&self) -> bool {
        let _framebuffer = self.lock();
        !self.held_messages().is_empty() || self.copied_unannounced.load(Ordering::Acquire)
    }

    /// `_locked` shows that the caller holds the framebuffer lock.
    fn send_held(&self, _locked: &Framebuffer) {
        for message in self.held_messages().drain(..) {
            let _ = self.tiles.send(message);
        }
    }

    fn held_messages(&self) -> MutexGuard<'_, Vec<TileMessage>> {
        self.held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The pixels of a rectangle of the screen as it is shown, BGRX. Outside the screen: black.
    pub fn read_region(&self, x: i32, y: i32, width: i32, height: i32) -> Vec<u8> {
        self.lock().read_region(x, y, width, height)
    }

    /// Chooses how the next tiles are compressed. Pages decode every kind.
    pub fn set_compression(&self, compression: Compression) {
        let _framebuffer = self.lock();
        self.encoder().set_compression(compression);
    }

    /// Sends the screen as an H.264 stream (see `video.rs`) instead of tiles.
    pub fn set_video(&self, on: bool) {
        self.video.store(on, Ordering::Release);
    }

    pub fn video_enabled(&self) -> bool {
        self.video.load(Ordering::Acquire)
    }

    /// The number of the newest screen contents, and a copy of them: (version, width, height, BGRX).
    pub fn frame(&self) -> (u64, u16, u16, Vec<u8>) {
        let framebuffer = self.lock();
        let (width, height) = framebuffer.size();
        let version = self.frame_version.load(Ordering::Acquire);
        (version, width, height, framebuffer.pixels().to_vec())
    }

    pub fn frame_version(&self) -> u64 {
        self.frame_version.load(Ordering::Acquire)
    }

    /// Gives a piece of the H.264 stream to every page.
    pub fn publish_video(&self, chunk: &[u8]) {
        let mut message = Vec::with_capacity(1 + chunk.len());
        message.push(wire::VIDEO_PREFIX);
        message.extend_from_slice(chunk);
        let _ = self.tiles.send(Bytes::from(message));
    }

    fn lock(&self) -> MutexGuard<'_, Framebuffer> {
        self.framebuffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_that_fell_behind_can_get_the_newest_clipboard_text() {
        let display = Display::new(4, 4);
        let seen = display.clipboard_version();
        assert!(display.clipboard_after(seen).is_none());
        let copy = |text: &str| {
            display.publish_clipboard(
                ClipboardSource::Clipboard,
                &ClipboardData::text_only(text.to_owned()),
            );
        };
        copy("uno");
        copy("dos ñ");
        let (version, message) = display.clipboard_after(seen).unwrap();
        let Ok(wire::Message::ClipboardSet { parts, .. }) = wire::decode(&message) else {
            panic!("not a clipboard set");
        };
        assert_eq!(
            parts.collect::<Vec<_>>(),
            [(wire::ClipboardFormat::Text, "dos ñ".as_bytes())]
        );
        assert!(display.clipboard_after(version).is_none(), "seen already");
    }

    #[test]
    fn clipboard_text_and_input_keep_the_order_of_the_page() {
        let display = Display::new(4, 4);
        let mut receiver = display.take_input().unwrap();
        display.send_input(Input::PointerLeft);
        display.send_clipboard_in(ClipboardData::text_only("texto".into()));
        display.send_input(Input::ReleaseAll);
        assert_eq!(
            receiver.try_recv(),
            Ok(PageEvent::Input(Input::PointerLeft))
        );
        assert_eq!(
            receiver.try_recv(),
            Ok(PageEvent::Clipboard(ClipboardData::text_only(
                "texto".into()
            )))
        );
        assert_eq!(receiver.try_recv(), Ok(PageEvent::Input(Input::ReleaseAll)));
    }

    #[test]
    fn a_flat_tile_is_sent_compressed_and_comes_back_the_same() {
        let display = Display::new(64, 64);
        let mut tiles = display.subscribe();
        let rect = Rect {
            x: 0,
            y: 0,
            w: 32,
            h: 32,
        };
        let pixels: Vec<u8> = [10, 20, 30, 0].repeat(32 * 32);
        display.update(rect, &pixels).unwrap();
        let message = tiles.try_recv().unwrap();
        assert!(
            message.len() < pixels.len() / 10,
            "a flat tile must be compressed"
        );
        assert_eq!(decode_tile(&message), Some((rect, pixels)));
    }

    #[test]
    fn noise_that_does_not_shrink_stays_raw() {
        let mut state = 0x1234_5678_u32;
        let pixels: Vec<u8> = (0..32 * 32 * 4)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect();
        let display = Display::new(64, 64);
        let mut tiles = display.subscribe();
        let rect = Rect {
            x: 0,
            y: 0,
            w: 32,
            h: 32,
        };
        display.update(rect, &pixels).unwrap();
        let message = tiles.try_recv().unwrap();
        assert!(matches!(
            wire::decode(&message),
            Ok(wire::Message::Tile { .. })
        ));
    }

    #[test]
    fn resize_tells_the_pages_the_new_size() {
        let display = Display::new(4, 4);
        let mut tiles = display.subscribe();
        display.resize(8, 6);
        assert_eq!(display.size(), (8, 6));
        let first = tiles.try_recv().unwrap();
        assert_eq!(
            wire::decode(&first),
            Ok(wire::Message::Screen {
                width: 8,
                height: 6
            })
        );
        assert!(
            tiles.try_recv().is_err(),
            "the caller draws the screen, not resize"
        );
    }

    #[test]
    fn input_goes_to_the_one_receiver() {
        let display = Display::new(4, 4);
        let mut receiver = display.take_input().expect("first take");
        assert!(display.take_input().is_none());
        display.clone().send_input(Input::PointerLeft);
        assert_eq!(
            receiver.try_recv(),
            Ok(PageEvent::Input(Input::PointerLeft))
        );
    }

    #[test]
    fn only_the_changed_part_is_sent_and_the_same_pixels_send_nothing() {
        let display = Display::new(64, 64);
        let mut tiles = display.subscribe();
        let rect = Rect {
            x: 8,
            y: 8,
            w: 32,
            h: 32,
        };
        let mut pixels = vec![0; 32 * 32 * 4];
        let at = (10 * 32 + 20) * 4;
        pixels[at..at + 4].copy_from_slice(&[1, 2, 3, 0]);
        display.update(rect, &pixels).unwrap();
        let message = tiles.try_recv().unwrap();
        assert_eq!(
            wire::decode(&message),
            Ok(wire::Message::Fill {
                rect: Rect {
                    x: 28,
                    y: 18,
                    w: 1,
                    h: 1
                },
                pixel: [1, 2, 3, 0]
            })
        );
        let version = display.frame_version();
        display.update(rect, &pixels).unwrap();
        assert!(tiles.try_recv().is_err(), "nothing changed");
        assert_eq!(display.frame_version(), version);
    }

    #[test]
    fn a_copy_waits_for_the_tiles_of_its_burst_and_never_reaches_a_page_twice() {
        let display = Display::new(4, 4);
        let mut old_page = display.subscribe();
        let row = Rect {
            x: 0,
            y: 0,
            w: 4,
            h: 1,
        };
        display.update(row, &[7; 16]).unwrap();
        old_page.try_recv().unwrap();
        let moved = Rect {
            x: 0,
            y: 0,
            w: 4,
            h: 1,
        };
        assert!(display.copy(moved, (0, 1)).unwrap());
        assert!(old_page.try_recv().is_err(), "the copy is held");

        let (mut new_page, (_, full, _)) = display.subscribe_with_snapshot();
        assert_eq!(
            &full[wire::TILE_HEADER_LEN + 16..][..4],
            [7; 4],
            "the snapshot has the copy"
        );
        let copy = old_page.try_recv().unwrap();
        assert!(matches!(
            wire::decode(&copy),
            Ok(wire::Message::Copy { .. })
        ));

        display.copy(moved, (0, 2)).unwrap();
        display
            .update(
                Rect {
                    x: 0,
                    y: 3,
                    w: 1,
                    h: 1,
                },
                &[9; 4],
            )
            .unwrap();
        for page in [&mut old_page, &mut new_page] {
            let first = page.try_recv().unwrap();
            assert!(matches!(
                wire::decode(&first),
                Ok(wire::Message::Copy { .. })
            ));
            assert!(matches!(
                wire::decode(&page.try_recv().unwrap()),
                Ok(wire::Message::Tile { .. } | wire::Message::Fill { .. })
            ));
        }
        display.copy(moved, (0, 3)).unwrap();
        display.release_held();
        assert!(
            new_page.try_recv().is_ok(),
            "released at the end of the burst"
        );
    }

    #[test]
    fn subscriber_gets_the_updated_tile() {
        let display = Display::new(4, 4);
        let mut tiles = display.subscribe();
        let rect = Rect {
            x: 1,
            y: 1,
            w: 1,
            h: 1,
        };
        display.update(rect, &[5, 6, 7, 0]).unwrap();
        let message = tiles.try_recv().unwrap();
        assert_eq!(&message[wire::TILE_HEADER_LEN..], [5, 6, 7, 0]);
        let (_, (_, full, _)) = display.subscribe_with_snapshot();
        assert_eq!(
            &full[wire::TILE_HEADER_LEN + (4 + 1) * 4..][..4],
            [5, 6, 7, 0]
        );
    }
}
