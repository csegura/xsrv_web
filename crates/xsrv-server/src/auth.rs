// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! A token for the page and the WebSocket, and the rule that a server on the network needs one.
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

pub const MIN_TOKEN_LEN: usize = 16;
pub const MAX_TOKEN_LEN: usize = 128;
const COOKIE_NAME: &str = "xsrv_token";

/// The token that clients must show. `None` means that the server is open.
#[derive(Clone)]
pub struct Auth {
    token: Option<Arc<str>>,
    /// Add `Secure` to the cookie. Set it when a TLS proxy sits in front of the server.
    secure_cookie: bool,
}

impl Auth {
    pub fn open() -> Self {
        Self {
            token: None,
            secure_cookie: false,
        }
    }

    pub fn with_secure_cookie(mut self, secure: bool) -> Self {
        self.secure_cookie = secure;
        self
    }

    /// A token has 16 to 128 characters of `A-Z a-z 0-9 . _ ~ -`, so it is safe in a URL and a cookie.
    pub fn with_token(token: &str) -> Result<Self, String> {
        let valid_char = |c: char| c.is_ascii_alphanumeric() || ".-_~".contains(c);
        if !(MIN_TOKEN_LEN..=MAX_TOKEN_LEN).contains(&token.len()) || !token.chars().all(valid_char)
        {
            return Err(format!(
                "XSRV_TOKEN must have {MIN_TOKEN_LEN} to {MAX_TOKEN_LEN} characters of A-Z a-z 0-9 . _ ~ -"
            ));
        }
        Ok(Self {
            token: Some(token.into()),
            secure_cookie: false,
        })
    }
}

pub const DEFAULT_PORT: u16 = 8080;

/// The address to listen on. `XSRV_ADDR` wins. Without it, the server listens on every interface
/// on `XSRV_PORT` (8080 by default) when it has a token, and on the loopback interface only when
/// it has none, because it does not serve a network without a token.
pub fn listen_address(
    addr: Option<String>,
    port: Option<String>,
    has_token: bool,
) -> Result<String, String> {
    if let Some(addr) = addr {
        return Ok(addr);
    }
    let port = match port
        .as_deref()
        .map(str::trim)
        .filter(|port| !port.is_empty())
    {
        None => DEFAULT_PORT,
        Some(text) => text
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(|| format!("XSRV_PORT {text:?} is not a port from 1 to 65535"))?,
    };
    let host = if has_token { "0.0.0.0" } else { "127.0.0.1" };
    Ok(format!("{host}:{port}"))
}

/// A server that listens on a network address needs a token, unless the operator says otherwise.
pub fn check_bind_policy(addr: &str, has_token: bool, allow_open: bool) -> Result<(), String> {
    let parsed: SocketAddr = addr
        .parse()
        .map_err(|_| format!("XSRV_ADDR {addr:?} is not an address like 127.0.0.1:8080"))?;
    if parsed.ip().is_loopback() || has_token || allow_open {
        return Ok(());
    }
    Err(format!(
        "refusing to listen on {addr} without a token: set XSRV_TOKEN, or XSRV_ALLOW_NO_AUTH=1 to accept the risk"
    ))
}

/// Compares in the same time whatever the position of the first difference.
fn same_secret(a: &[u8], b: &[u8]) -> bool {
    let mut difference = u8::from(a.len() != b.len());
    for (x, y) in a.iter().zip(b) {
        difference |= x ^ y;
    }
    difference == 0
}

fn token_in_query(query: &str) -> Option<&str> {
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("token="))
}

fn token_in_cookies(cookies: &str) -> Option<&str> {
    cookies
        .split(';')
        .find_map(|cookie| cookie.trim().strip_prefix(&format!("{COOKIE_NAME}=")[..]))
}

/// Lets a request through when it shows the token, in the query or in the cookie.
/// A token in the query also sets the cookie, so the scripts and the WebSocket of the page need nothing.
pub async fn require_token(State(auth): State<Auth>, request: Request, next: Next) -> Response {
    let Some(token) = auth.token.clone() else {
        return next.run(request).await;
    };
    let from_query = request.uri().query().and_then(token_in_query);
    let from_cookie = request
        .headers()
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(token_in_cookies);
    let accepted_query = from_query.is_some_and(|t| same_secret(t.as_bytes(), token.as_bytes()));
    let accepted_cookie = from_cookie.is_some_and(|t| same_secret(t.as_bytes(), token.as_bytes()));
    if !accepted_query && !accepted_cookie {
        return (StatusCode::UNAUTHORIZED, "a token is required").into_response();
    }
    let mut response = next.run(request).await;
    if accepted_query && !accepted_cookie {
        let secure = if auth.secure_cookie { "; Secure" } else { "" };
        let cookie = format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict{secure}");
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::listen_address;

    #[test]
    fn the_listen_address_depends_on_the_token_and_the_port() {
        let some = |text: &str| Some(text.to_owned());
        assert_eq!(listen_address(None, None, true), Ok("0.0.0.0:8080".into()));
        assert_eq!(
            listen_address(None, None, false),
            Ok("127.0.0.1:8080".into())
        );
        assert_eq!(
            listen_address(None, some("9000"), true),
            Ok("0.0.0.0:9000".into())
        );
        assert_eq!(
            listen_address(None, some(" "), true),
            Ok("0.0.0.0:8080".into())
        );
        assert_eq!(
            listen_address(some("127.0.0.1:81"), some("9000"), true),
            Ok("127.0.0.1:81".into()),
            "XSRV_ADDR wins over XSRV_PORT"
        );
        for bad in ["0", "65536", "http", "-1"] {
            assert!(listen_address(None, some(bad), true).is_err(), "{bad}");
        }
    }
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use axum::middleware::from_fn_with_state;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    const TOKEN: &str = "0123456789abcdefXYZ";

    fn app(auth: Auth) -> Router {
        Router::new()
            .route("/", get(|| async { "page" }))
            .layer(from_fn_with_state(auth, require_token))
    }

    async fn call(app: Router, uri: &str, cookie: Option<&str>) -> Response {
        let mut request = HttpRequest::builder().uri(uri);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        app.oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn an_open_server_lets_everything_in() {
        assert_eq!(
            call(app(Auth::open()), "/", None).await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn a_request_without_the_token_is_refused() {
        let auth = Auth::with_token(TOKEN).unwrap();
        assert_eq!(
            call(app(auth.clone()), "/", None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let wrong = "/?token=0123456789abcdefXYX";
        assert_eq!(
            call(app(auth.clone()), wrong, None).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let cookie = "xsrv_token=nope";
        assert_eq!(
            call(app(auth), "/", Some(cookie)).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn the_cookie_is_secure_only_when_asked() {
        let plain = Auth::with_token(TOKEN).unwrap();
        let uri = format!("/?token={TOKEN}");
        let response = call(app(plain.clone()), &uri, None).await;
        let cookie = response.headers().get(header::SET_COOKIE).unwrap();
        assert!(!cookie.to_str().unwrap().contains("Secure"));
        let secure = plain.with_secure_cookie(true);
        let response = call(app(secure), &uri, None).await;
        let cookie = response.headers().get(header::SET_COOKIE).unwrap();
        assert!(cookie.to_str().unwrap().ends_with("; Secure"));
    }

    #[tokio::test]
    async fn the_token_in_the_query_opens_the_page_and_sets_the_cookie() {
        let auth = Auth::with_token(TOKEN).unwrap();
        let response = call(app(auth), &format!("/?x=1&token={TOKEN}"), None).await;
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(cookie.contains(&format!("xsrv_token={TOKEN}")));
        assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    }

    #[tokio::test]
    async fn the_cookie_opens_the_other_files_and_is_not_set_again() {
        let auth = Auth::with_token(TOKEN).unwrap();
        let cookie = format!("theme=dark; xsrv_token={TOKEN}");
        let response = call(app(auth), "/", Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(header::SET_COOKIE).is_none());
    }

    #[test]
    fn tokens_are_checked_for_length_and_characters() {
        assert!(Auth::with_token("short").is_err());
        assert!(Auth::with_token(&"a".repeat(129)).is_err());
        assert!(Auth::with_token("has space in it 123").is_err());
        assert!(Auth::with_token("semi;colon-in-token1").is_err());
        assert!(Auth::with_token("abcdefghijklmnop").is_ok());
    }

    #[test]
    fn a_network_address_needs_a_token() {
        assert!(check_bind_policy("127.0.0.1:8080", false, false).is_ok());
        assert!(check_bind_policy("[::1]:8080", false, false).is_ok());
        assert!(check_bind_policy("0.0.0.0:8080", false, false).is_err());
        assert!(check_bind_policy("0.0.0.0:8080", true, false).is_ok());
        assert!(check_bind_policy("0.0.0.0:8080", false, true).is_ok());
        assert!(check_bind_policy("not an address", true, true).is_err());
    }

    #[test]
    fn secrets_are_compared_whole() {
        assert!(same_secret(b"abc", b"abc"));
        assert!(!same_secret(b"abc", b"abd"));
        assert!(!same_secret(b"abc", b"abcd"));
        assert!(!same_secret(b"", b"a"));
    }
}
