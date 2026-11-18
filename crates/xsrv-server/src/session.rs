// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket};
use std::time::Instant;
use tokio::sync::broadcast::error::RecvError;

use crate::limits::{PageGuard, RateLimit, MAX_INPUT_PER_SECOND, MAX_RESIZE_PER_SECOND};
use wire::Input;
use xsrv_server::clipboard_set::ClipboardData;
use xsrv_server::display::{Display, Snapshot};

pub async fn run(mut socket: WebSocket, display: Display, _slot: PageGuard<'static>) {
    let mut sent_input = false;
    pump(&mut socket, &display, &mut sent_input).await;
    // A page that closed with a key or a button down would leave it down for the next page.
    if sent_input {
        display.send_input(Input::ReleaseAll);
    }
}

async fn pump(socket: &mut WebSocket, display: &Display, sent_input: &mut bool) {
    let (mut tiles, snapshot) = display.subscribe_with_snapshot();
    let mut seen_clipboard = display.clipboard_version();
    let mut rate = RateLimit::new(MAX_INPUT_PER_SECOND, Instant::now());
    let mut resize_rate = RateLimit::new(MAX_RESIZE_PER_SECOND, Instant::now());
    if send_snapshot(socket, display, snapshot).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            tile = tiles.recv() => {
                let sent = match tile {
                    Ok(tile) => {
                        if tile.first() == Some(&wire::KIND_CLIPBOARD_SET) {
                            seen_clipboard = display.clipboard_version();
                        }
                        send(socket, tile).await
                    }
                    Err(RecvError::Lagged(_)) => {
                        // Old tiles would redraw old pixels over the snapshot.
                        let (receiver, snapshot) = display.subscribe_with_snapshot();
                        tiles = receiver;
                        let snapshot = send_snapshot(socket, display, snapshot).await;
                        // The lost messages may have held clipboard text from X.
                        match display.clipboard_after(seen_clipboard) {
                            Some((version, message)) if snapshot.is_ok() => {
                                seen_clipboard = version;
                                send(socket, message).await
                            }
                            _ => snapshot,
                        }
                    }
                    Err(RecvError::Closed) => return,
                };
                if sent.is_err() {
                    return;
                }
            }
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) if !rate.allow(Instant::now()) => {
                    eprintln!("xsrv-web: closing a page that sends more than {MAX_INPUT_PER_SECOND} messages a second");
                    return;
                }
                Some(Ok(Message::Binary(bytes))) if bytes.first() == Some(&wire::KIND_CLIPBOARD_IN) => {
                    match wire::decode_clipboard_in(&bytes) {
                        Ok(text) => display.send_clipboard_in(ClipboardData::text_only(text.to_owned())),
                        Err(error) => {
                            eprintln!("xsrv-web: closing a page that sent bad clipboard text: {error:?}");
                            return;
                        }
                    }
                }
                Some(Ok(Message::Binary(bytes))) if bytes.first() == Some(&wire::KIND_CLIPBOARD_SET_IN) => {
                    match wire::decode_clipboard_set_in(&bytes) {
                        Ok(parts) => display.send_clipboard_in(ClipboardData::from_parts(parts)),
                        Err(error) => {
                            eprintln!("xsrv-web: closing a page that sent a bad clipboard: {error:?}");
                            return;
                        }
                    }
                }
                Some(Ok(Message::Binary(bytes))) => match wire::decode_input(&bytes) {
                    // Each resize draws the whole screen again, so a page may not send many.
                    Ok(Input::Resize { .. }) if !resize_rate.allow(Instant::now()) => {}
                    Ok(input) => {
                        *sent_input = true;
                        display.send_input(input);
                    }
                    Err(error) => {
                        eprintln!("xsrv-web: closing a page that sent bad input: {error:?}");
                        return;
                    }
                },
                Some(Ok(_)) => {}
            }
        }
    }
}

async fn send_snapshot(
    socket: &mut WebSocket,
    display: &Display,
    snapshot: Snapshot,
) -> Result<(), axum::Error> {
    let (screen, full, cursor) = snapshot;
    send(socket, screen).await?;
    send(socket, full).await?;
    send(socket, cursor).await?;
    send(socket, display.layout_message()).await
}

async fn send(socket: &mut WebSocket, bytes: impl Into<Bytes>) -> Result<(), axum::Error> {
    socket.send(Message::Binary(bytes.into())).await
}
