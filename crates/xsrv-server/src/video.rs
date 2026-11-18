// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! The screen as an H.264 stream. An `ffmpeg` process with libx264 encodes the frames, and the
//! output goes to the pages, which decode it with WebCodecs. It is off by default (`XSRV_VIDEO`).
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

use crate::display::Display;

const FRAMES_PER_SECOND: u64 = 30;
/// A key frame (with the stream headers) at least this often, so that a page that joins late, or
/// that missed a piece, can start again.
const KEY_FRAME_EVERY: u64 = 30;
/// A frame is sent at least this often even when the screen did not change, so that the encoder
/// keeps producing key frames for new pages.
const STATIC_FRAME_EVERY: Duration = Duration::from_secs(1);
const READ_CHUNK: usize = 64 * 1024;

/// The arguments of `ffmpeg`: raw BGRX frames in, H.264 in Annex B out, with the headers repeated
/// at each key frame and no frame reordering, so that a frame can be shown as soon as it arrives.
pub fn ffmpeg_args(width: u16, height: u16) -> Vec<String> {
    let keyint = KEY_FRAME_EVERY;
    [
        "-loglevel",
        "error",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "bgr0",
        "-s",
        &format!("{width}x{height}"),
        "-framerate",
        &FRAMES_PER_SECOND.to_string(),
        "-i",
        "-",
        // yuv420p needs even sizes. The page draws the frame at the top left, so a padded row or
        // column falls outside the canvas.
        "-vf",
        "pad=ceil(iw/2)*2:ceil(ih/2)*2,format=yuv420p",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-tune",
        "zerolatency",
        "-profile:v",
        "baseline",
        "-x264-params",
        &format!("keyint={keyint}:min-keyint={keyint}:repeat-headers=1:scenecut=0:aud=1"),
        "-f",
        "h264",
        "-",
    ]
    .iter()
    .map(|arg| (*arg).to_owned())
    .collect()
}

fn spawn(width: u16, height: u16) -> std::io::Result<Child> {
    Command::new("ffmpeg")
        .args(ffmpeg_args(width, height))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
}

/// Encodes the screen for as long as the server runs. Returns an error when `ffmpeg` cannot run.
pub async fn run(display: Display) -> std::io::Result<()> {
    display.set_video(true);
    loop {
        let (_, width, height, _) = display.frame();
        let mut child = spawn(width, height)?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let reader = {
            let display = display.clone();
            tokio::task::spawn_local(async move {
                let mut buffer = vec![0u8; READ_CHUNK];
                loop {
                    match stdout.read(&mut buffer).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => display.publish_video(&buffer[..n]),
                    }
                }
            })
        };
        let mut sent_version = None;
        let mut last_send = tokio::time::Instant::now();
        let mut ticker = tokio::time::interval(Duration::from_millis(1000 / FRAMES_PER_SECOND));
        let outcome = loop {
            ticker.tick().await;
            let version = display.frame_version();
            let due = last_send.elapsed() >= STATIC_FRAME_EVERY;
            if sent_version == Some(version) && !due {
                continue;
            }
            let (version, now_width, now_height, pixels) = display.frame();
            if (now_width, now_height) != (width, height) {
                break Ok(());
            }
            if stdin.write_all(&pixels).await.is_err() {
                break Err(std::io::Error::other("ffmpeg stopped"));
            }
            sent_version = Some(version);
            last_send = tokio::time::Instant::now();
        };
        drop(stdin);
        reader.abort();
        let _ = child.kill().await;
        outcome?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_encoder_gets_the_screen_size_and_repeats_its_headers() {
        let args = ffmpeg_args(801, 601);
        let size = args.iter().position(|a| a == "-s").unwrap();
        assert_eq!(args[size + 1], "801x601");
        assert!(args
            .iter()
            .any(|a| a.contains("repeat-headers=1") && a.contains("aud=1")));
        assert!(args.iter().any(|a| a.contains("pad=ceil(iw/2)*2")));
    }
}
