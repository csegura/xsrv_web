// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

mod auth;
mod limits;
mod session;
mod token_command;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::Request;
use axum::extract::{Query, State, WebSocketUpgrade};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::serve::ListenerExt;
use axum::Router;
use tokio::signal::unix::{signal, SignalKind};
use tokio::task::LocalSet;
use tower_http::services::ServeDir;
use xsrv_server::display::{Compression, Display};
use xsrv_server::x11;

const DEFAULT_X11_SOCKET: &str = "/tmp/.X11-unix/X1";
/// Independent screens in one process. Each has its own X11 socket and its own page.
const MAX_SESSIONS: usize = 8;
const DEFAULT_WEB_DIR: &str = "web";
const MAX_INCOMING_BYTES: usize = wire::MAX_CLIPBOARD_SET + 16;

/// The pages that are open now.
static PAGES: limits::PageSlots = limits::PageSlots::new();

const USAGE: &str = "\
Usage: xsrv-web [token | --help | --version]

  (no argument)  run the server; the settings are XSRV_* environment variables
                 (see /usr/share/doc/xsrv-web/README.txt)
  token          print the token of the installed service and the address of its page
                 (needs sudo: the settings files are readable by root only)";

fn main() -> std::process::ExitCode {
    match dispatch() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xsrv-web: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn dispatch() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => serve(),
        ["token"] => token_command::run(&[
            std::path::Path::new(token_command::GENERATED_TOKEN_FILE),
            std::path::Path::new(token_command::SETTINGS_FILE),
        ]),
        ["--help" | "-h"] => {
            println!("{USAGE}");
            Ok(())
        }
        ["--version" | "-V"] => {
            println!("xsrv-web {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => Err(refuse(format!(
            "unknown arguments: {}\n{USAGE}",
            args.join(" ")
        ))),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn serve() -> io::Result<()> {
    LocalSet::new().run_until(run()).await
}

/// The X11 socket of each session: `X1`, `X2`, ... in the directory of the first socket.
fn session_sockets(first: &str, count: usize) -> Result<Vec<String>, String> {
    if count == 1 {
        return Ok(vec![first.to_owned()]);
    }
    let path = PathBuf::from(first);
    let number = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix('X'))
        .and_then(|digits| digits.parse::<usize>().ok())
        .ok_or_else(|| {
            format!("with XSRV_SESSIONS above 1, XSRV_X11_SOCKET must end in X<number>: {first}")
        })?;
    // The first socket keeps the name that was given (X01 stays X01).
    let mut sockets = vec![first.to_owned()];
    for index in 1..count {
        let next = number
            .checked_add(index)
            .ok_or_else(|| format!("the socket number in {first} is too large"))?;
        sockets.push(
            path.with_file_name(format!("X{next}"))
                .to_string_lossy()
                .into_owned(),
        );
    }
    Ok(sockets)
}

/// 32 hexadecimal digits, the same as the cookie in an Xauthority file.
fn parse_cookie(hex: &str) -> Result<[u8; 16], String> {
    let bad = || "XSRV_X11_COOKIE must be 32 hexadecimal digits".to_owned();
    if hex.len() != 32 || !hex.is_ascii() {
        return Err(bad());
    }
    let mut cookie = [0u8; 16];
    for (byte, pair) in cookie.iter_mut().zip(hex.as_bytes().chunks(2)) {
        let digits = std::str::from_utf8(pair).map_err(|_| bad())?;
        *byte = u8::from_str_radix(digits, 16).map_err(|_| bad())?;
    }
    Ok(cookie)
}

fn refuse(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn token_from_environment() -> Option<String> {
    std::env::var("XSRV_TOKEN")
        .ok()
        .filter(|token| !token.is_empty())
}

fn auth_from_environment(addr: &str, token: Option<String>) -> io::Result<auth::Auth> {
    let allow_open = std::env::var("XSRV_ALLOW_NO_AUTH").is_ok_and(|value| value == "1");
    auth::check_bind_policy(addr, token.is_some(), allow_open).map_err(refuse)?;
    match token {
        Some(token) => auth::Auth::with_token(&token).map_err(refuse),
        None => {
            if !allow_open {
                return Ok(auth::Auth::open());
            }
            eprintln!("xsrv-web: WARNING: the server is open to anyone who can reach {addr}");
            Ok(auth::Auth::open())
        }
    }
}

async fn run() -> io::Result<()> {
    let token = token_from_environment();
    let configured_addr = std::env::var("XSRV_ADDR").ok();
    if configured_addr.is_none() && token.is_none() {
        eprintln!(
            "xsrv-web: no XSRV_TOKEN: listening on 127.0.0.1 only. Set XSRV_TOKEN to listen on every interface."
        );
    }
    let addr = auth::listen_address(
        configured_addr,
        std::env::var("XSRV_PORT").ok(),
        token.is_some(),
    )
    .map_err(refuse)?;
    let secure_cookie = std::env::var("XSRV_SECURE_COOKIE").is_ok_and(|value| value == "1");
    let auth = auth_from_environment(&addr, token)?.with_secure_cookie(secure_cookie);
    let socket = std::env::var("XSRV_X11_SOCKET").unwrap_or_else(|_| DEFAULT_X11_SOCKET.into());
    let sessions = match std::env::var("XSRV_SESSIONS") {
        Ok(value) => value
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_SESSIONS).contains(n))
            .ok_or_else(|| refuse(format!("XSRV_SESSIONS must be 1 to {MAX_SESSIONS}")))?,
        Err(_) => 1,
    };
    let sockets = session_sockets(&socket, sessions).map_err(refuse)?;
    let layout = match std::env::var("XSRV_KEYMAP") {
        Ok(name) => x11::Layout::from_name(&name).ok_or_else(|| {
            refuse(format!(
                "XSRV_KEYMAP must be us, es, gb or de, not {name:?}"
            ))
        })?,
        Err(_) => x11::Layout::default(),
    };
    let monitors = match std::env::var("XSRV_MONITORS") {
        Ok(value) => value
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=x11::MAX_MONITORS).contains(n))
            .ok_or_else(|| refuse(format!("XSRV_MONITORS must be 1 to {}", x11::MAX_MONITORS)))?,
        Err(_) => 1,
    };
    let video = match std::env::var("XSRV_VIDEO").as_deref() {
        Ok("h264") => true,
        Ok("" | "off" | "0") | Err(_) => false,
        Ok(other) => {
            return Err(refuse(format!(
                "XSRV_VIDEO must be h264 or off, not {other:?}"
            )))
        }
    };
    // An empty value means the default, as for the other settings (a settings file may leave it
    // empty).
    let compression = match std::env::var("XSRV_COMPRESSION") {
        Ok(name) if !name.is_empty() => Compression::from_name(&name).ok_or_else(|| {
            refuse(format!(
                "XSRV_COMPRESSION must be lz4 or zstd, not {name:?}"
            ))
        })?,
        _ => Compression::default(),
    };
    // An empty value means no cookie, as for XSRV_TOKEN (a settings file may leave it empty).
    let cookie = match std::env::var("XSRV_X11_COOKIE") {
        Ok(hex) if !hex.is_empty() => Some(parse_cookie(&hex).map_err(refuse)?),
        _ => None,
    };
    let maximize = std::env::var("XSRV_WM_MAXIMIZE").map_or(true, |value| value != "0");
    let window_manager = std::env::var("XSRV_WM").map_or(true, |value| value != "0");
    let xkb = match std::env::var("XSRV_XKB").as_deref() {
        Ok("1") | Err(_) => true,
        Ok("0") => false,
        Ok(other) => return Err(refuse(format!("XSRV_XKB must be 0 or 1, not {other:?}"))),
    };
    let mut displays = Vec::new();
    // Kept until `run` returns: then the socket files are removed.
    let mut socket_files = Vec::new();
    for (index, socket) in sockets.iter().enumerate() {
        let path = PathBuf::from(socket);
        let listener = x11::bind(&path)?;
        socket_files.push(x11::SocketFile::new(&path));
        println!("xsrv-web session {} X11 socket: {socket}", index + 1);
        let display = Display::new(x11::SCREEN_WIDTH, x11::SCREEN_HEIGHT);
        display.set_compression(compression);
        displays.push(display.clone());
        if video {
            let display = display.clone();
            tokio::task::spawn_local(async move {
                if let Err(error) = xsrv_server::video::run(display).await {
                    eprintln!("xsrv-web: video stopped: {error}");
                }
            });
        }
        tokio::task::spawn_local(async move {
            let options = x11::Options {
                window_manager,
                layout,
                maximize,
                monitors,
                cookie,
                xkb,
            };
            if let Err(error) = x11::serve_with(listener, display, options).await {
                eprintln!("xsrv-web: X11 server stopped: {error}");
            }
        });
    }

    let web_dir = std::env::var("XSRV_WEB_DIR").unwrap_or_else(|_| DEFAULT_WEB_DIR.into());
    let has_token = std::env::var("XSRV_TOKEN").is_ok_and(|token| !token.is_empty());
    let local_only = !has_token && is_loopback_address(&addr);
    let app = Router::new()
        .route("/ws", get(ws_handler))
        .fallback_service(ServeDir::new(web_dir))
        .layer(from_fn_with_state(auth, auth::require_token))
        .layer(from_fn(move |request: Request, next: Next| async move {
            if local_only && !host_is_local(request.headers()) {
                return StatusCode::FORBIDDEN.into_response();
            }
            let mut response = next.run(request).await;
            let headers = response.headers_mut();
            for (name, value) in RESPONSE_HEADERS {
                headers.insert(
                    header::HeaderName::from_static(name),
                    header::HeaderValue::from_static(value),
                );
            }
            response
        }))
        .with_state(Arc::new(displays));
    // Without TCP_NODELAY a small tile (a typed character) can wait for the answer of the page
    // to the tile before it.
    let listener = tokio::net::TcpListener::bind(&addr)
        .await?
        .tap_io(|stream| {
            if let Err(error) = stream.set_nodelay(true) {
                eprintln!("xsrv-web: TCP_NODELAY refused: {error}");
            }
        });
    println!("xsrv-web listening on http://{addr}");
    // A stop signal ends `run` at once (open pages do not hold it), so the socket files go away.
    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        served = axum::serve(listener, app) => served?,
        _ = terminate.recv() => println!("xsrv-web: stopped (SIGTERM)"),
        _ = tokio::signal::ctrl_c() => println!("xsrv-web: stopped (Ctrl+C)"),
    }
    drop(socket_files);
    Ok(())
}

const RESPONSE_HEADERS: [(&str, &str); 4] = [
    // The browser asks again each time (a 304 when nothing changed), so a page never runs an old
    // module against a newer server that sends messages it does not know.
    ("cache-control", "no-cache"),
    ("x-frame-options", "DENY"),
    (
        "content-security-policy",
        "frame-ancestors 'none'; default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'",
    ),
    ("referrer-policy", "no-referrer"),
];

fn is_loopback_address(addr: &str) -> bool {
    addr.starts_with("127.") || addr.starts_with("localhost:") || addr.starts_with("[::1]:")
}

/// Without a token, only a loopback name may reach the server. A page on another site that
/// points its own name at 127.0.0.1 (DNS rebinding) sends a Host header with that other name.
fn host_is_local(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let name = host.rsplit_once(':').map_or(host, |(name, _)| name);
    matches!(name, "127.0.0.1" | "localhost" | "[::1]")
}

#[derive(serde::Deserialize)]
struct SessionQuery {
    session: Option<usize>,
}

async fn ws_handler(
    State(displays): State<Arc<Vec<Display>>>,
    Query(query): Query<SessionQuery>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !origin_matches_host(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(display) = query
        .session
        .unwrap_or(1)
        .checked_sub(1)
        .and_then(|index| displays.get(index))
        .cloned()
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(slot) = PAGES.try_acquire(limits::MAX_PAGES) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    upgrade
        .max_message_size(MAX_INCOMING_BYTES)
        .max_frame_size(MAX_INCOMING_BYTES)
        .on_upgrade(move |socket| session::run(socket, display, slot))
}

/// Browsers do not apply the same-origin policy to WebSockets, so the server checks it.
/// Clients that send no `Origin` header are not browsers and are allowed.
fn origin_matches_host(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let origin_host = origin
        .to_str()
        .ok()
        .and_then(|value| value.split_once("://"))
        .map(|(_, host)| host);
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    matches!((origin_host, host), (Some(a), Some(b)) if a == b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(origin: Option<&str>, host: &str) -> HeaderMap {
        let mut map = HeaderMap::new();
        map.insert(header::HOST, host.parse().unwrap());
        if let Some(origin) = origin {
            map.insert(header::ORIGIN, origin.parse().unwrap());
        }
        map
    }

    #[test]
    fn the_x11_cookie_is_32_hex_digits() {
        assert_eq!(
            parse_cookie("000102030405060708090a0b0c0d0e0f").unwrap(),
            [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
        );
        assert!(parse_cookie("0001").is_err());
        assert!(parse_cookie(&"zz".repeat(16)).is_err());
        assert!(parse_cookie(&"é".repeat(16)).is_err());
    }

    #[test]
    fn sessions_get_one_socket_each_next_to_the_first() {
        assert_eq!(
            session_sockets("/tmp/.X11-unix/X1", 3).unwrap(),
            [
                "/tmp/.X11-unix/X1",
                "/tmp/.X11-unix/X2",
                "/tmp/.X11-unix/X3"
            ]
        );
        assert_eq!(
            session_sockets("/run/my-socket", 1).unwrap(),
            ["/run/my-socket"]
        );
        assert!(session_sockets("/run/my-socket", 2).is_err());
        assert!(session_sockets("/tmp/Xabc", 2).is_err());
    }

    #[test]
    fn same_origin_is_allowed() {
        assert!(origin_matches_host(&headers(
            Some("http://127.0.0.1:8080"),
            "127.0.0.1:8080"
        )));
    }

    #[test]
    fn foreign_origin_is_rejected() {
        assert!(!origin_matches_host(&headers(
            Some("http://evil.example"),
            "127.0.0.1:8080"
        )));
    }

    #[test]
    fn missing_origin_is_allowed() {
        assert!(origin_matches_host(&headers(None, "127.0.0.1:8080")));
    }
}
