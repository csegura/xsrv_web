// SPDX-FileCopyrightText: 2026 Carlos Segura Sanz
// SPDX-License-Identifier: AGPL-3.0-only

//! `xsrv-web token`: prints the token of the installed service and the address of its page.
//!
//! The service reads two settings files, as systemd does: first the token that the package made
//! (`/var/lib/xsrv-web/token`), then `/etc/default/xsrv-web`, whose values win. Both are root
//! only, so the command needs `sudo`. The environment of the caller is not read: the service
//! does not see it either.
use std::io;
use std::path::Path;

pub const GENERATED_TOKEN_FILE: &str = "/var/lib/xsrv-web/token";
pub const SETTINGS_FILE: &str = "/etc/default/xsrv-web";

/// The value of `key` in a systemd environment file: the last `KEY=VALUE` line, with one pair of
/// surrounding quotes removed. `None` when the line is missing or commented out.
pub fn value_in(text: &str, key: &str) -> Option<String> {
    text.lines()
        .rev()
        .map(str::trim)
        .filter(|line| !line.starts_with('#') && !line.starts_with(';'))
        .filter_map(|line| line.split_once('='))
        .find(|(name, _)| name.trim() == key)
        .map(|(_, value)| unquote(value.trim()).to_owned())
}

fn unquote(value: &str) -> &str {
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner;
        }
    }
    value
}

/// A file that does not exist counts as empty; one that cannot be read is an error.
fn read_optional(path: &Path) -> io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Err(io::Error::new(
            error.kind(),
            format!(
                "cannot read {}: it is readable by root only, run: sudo xsrv-web token",
                path.display()
            ),
        )),
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!("cannot read {}: {error}", path.display()),
        )),
    }
}

/// A setting as the service sees it: the last file that sets it wins, as with systemd's
/// EnvironmentFile lines in that order. An empty value means "not set", as for the server.
fn setting(key: &str, files: &[String]) -> Option<String> {
    files
        .iter()
        .rev()
        .find_map(|text| value_in(text, key))
        .filter(|value| !value.is_empty())
}

/// The address and the token of the service from the texts of its settings files, in the order
/// the service reads them. The error says what is missing.
pub fn resolve(files: &[String]) -> Result<(String, String), String> {
    let Some(token) = setting("XSRV_TOKEN", files) else {
        return Err(format!(
            "no token is set (XSRV_TOKEN in {SETTINGS_FILE}, or the file that the package \
             made, {GENERATED_TOKEN_FILE}). Without a token the service listens on 127.0.0.1 \
             only, and refuses a network address in XSRV_ADDR"
        ));
    };
    let addr = crate::auth::listen_address(
        setting("XSRV_ADDR", files),
        setting("XSRV_PORT", files),
        true,
    )?;
    Ok((addr, token))
}

/// The page address for a listening address: a wildcard address is shown as this host's name.
pub fn page_url(addr: &str, token: &str) -> String {
    let host_port = match addr.rsplit_once(':') {
        Some(("0.0.0.0" | "[::]" | "", port)) => format!("{}:{port}", host_name()),
        _ => addr.to_owned(),
    };
    format!("http://{host_port}/?token={token}")
}

fn host_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|name| name.trim().to_owned())
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "localhost".into())
}

/// Prints the token and the page address. `files` are the settings files in the order the
/// service reads them.
pub fn run(files: &[&Path]) -> io::Result<()> {
    let texts = files
        .iter()
        .map(|path| read_optional(path))
        .collect::<io::Result<Vec<_>>>()?;
    let (addr, token) =
        resolve(&texts).map_err(|message| io::Error::new(io::ErrorKind::NotFound, message))?;
    println!("token: {token}");
    println!("page:  {}", page_url(&addr, &token));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_uncommented_line_wins_and_quotes_go() {
        let text = "#XSRV_TOKEN=commented\nXSRV_TOKEN=first\n XSRV_TOKEN = \"second\" \n";
        assert_eq!(value_in(text, "XSRV_TOKEN").as_deref(), Some("second"));
        assert_eq!(
            value_in("XSRV_ADDR='0.0.0.0:80'", "XSRV_ADDR").as_deref(),
            Some("0.0.0.0:80")
        );
        assert_eq!(value_in("# XSRV_TOKEN=x\n", "XSRV_TOKEN"), None);
        assert_eq!(value_in("XSRV_TOKENS=x\n", "XSRV_TOKEN"), None);
    }

    #[test]
    fn the_settings_file_wins_over_the_generated_token() {
        let generated = "XSRV_TOKEN=generated-token-0123456789\n".to_owned();
        let files = |settings: &str| [generated.clone(), settings.to_owned()];
        assert_eq!(
            resolve(&files("XSRV_TOKEN=chosen-token-0123456789\n")),
            Ok(("0.0.0.0:8080".into(), "chosen-token-0123456789".into()))
        );
        assert_eq!(
            resolve(&files("#XSRV_TOKEN=\nXSRV_ADDR=0.0.0.0:80\n")),
            Ok(("0.0.0.0:80".into(), "generated-token-0123456789".into())),
            "a commented line leaves the generated token"
        );
        assert_eq!(
            resolve(&files("XSRV_PORT=9000\n")),
            Ok(("0.0.0.0:9000".into(), "generated-token-0123456789".into())),
            "XSRV_PORT sets the port of the default address"
        );
        assert!(
            resolve(&files("XSRV_TOKEN=\n")).is_err(),
            "an empty value turns the token off, as for the service"
        );
    }

    #[test]
    fn a_wildcard_address_is_shown_as_the_host_name() {
        assert_eq!(
            page_url("127.0.0.1:8080", "t"),
            "http://127.0.0.1:8080/?token=t"
        );
        let url = page_url("0.0.0.0:8080", "t");
        assert!(url.starts_with("http://") && url.ends_with(":8080/?token=t"));
        assert!(!url.contains("0.0.0.0"));
    }

    #[test]
    fn a_missing_file_is_empty() {
        let text = read_optional(Path::new("/nonexistent/xsrv-web/settings")).unwrap();
        assert!(text.is_empty());
    }
}
