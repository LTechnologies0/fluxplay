//! Detect Xtream Codes `get.php` playlist URLs and rewrite to API access.
//!
//! Many panels return non-standard HTTP status / empty body for
//! `get.php` while `player_api.php` works — IPTVnator-style apps fall back to XC API.

use tracing::{debug, trace};
use url::Url;

use crate::ProviderError;

#[derive(Debug, Clone)]
pub struct XtreamCredentials {
    pub base: String,
    pub username: String,
    pub password: String,
}

/// If `endpoint` looks like `http(s)://host[:port]/get.php?username=&password=`, extract creds.
pub fn parse_xtream_get_php(endpoint: &str) -> Option<XtreamCredentials> {
    let endpoint = endpoint.trim();
    let url = Url::parse(endpoint).ok()?;
    let path = url.path().to_ascii_lowercase();
    if !path.ends_with("get.php") && !path.contains("/get.php") {
        trace!("endpoint is not get.php");
        return None;
    }

    let mut username = None;
    let mut password = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "username" => username = Some(v.to_string()),
            "password" => password = Some(v.to_string()),
            _ => {}
        }
    }
    let username = username.filter(|s| !s.is_empty())?;
    let password = password.filter(|s| !s.is_empty())?;

    let base = portal_base_from_url(&url);

    let creds = XtreamCredentials {
        base: base.as_str().trim_end_matches('/').to_string(),
        username,
        password,
    };
    // Never log password — only base + username.
    debug!(
        base = %creds.base,
        user = %creds.username,
        "parsed get.php Xtream credentials"
    );
    Some(creds)
}

/// Keep `/c` / `/iptv` path prefixes; strip trailing `get.php` / `player_api.php`.
pub fn portal_base_from_url(url: &Url) -> Url {
    let mut u = url.clone();
    u.set_query(None);
    u.set_fragment(None);
    let path = u.path().to_string();
    let lower = path.to_ascii_lowercase();
    let stripped = if let Some(idx) = lower.rfind("/get.php") {
        &path[..idx]
    } else if lower.ends_with("get.php") {
        path.trim_end_matches("get.php").trim_end_matches('/')
    } else if let Some(idx) = lower.rfind("/player_api.php") {
        &path[..idx]
    } else if lower.ends_with("player_api.php") {
        path.trim_end_matches("player_api.php")
            .trim_end_matches('/')
    } else {
        path.trim_end_matches('/')
    };
    if stripped.is_empty() {
        u.set_path("");
    } else {
        u.set_path(stripped);
    }
    u
}

/// Join a PHP script onto a portal base that may include a directory prefix.
pub fn join_portal_script(portal: &Url, script: &str) -> Url {
    let mut u = portal.clone();
    let base = portal.path().trim_end_matches('/');
    if base.is_empty() {
        u.set_path(script);
    } else {
        u.set_path(&format!("{base}/{script}"));
    }
    u
}

pub fn http_status_hint(status: u16) -> Option<&'static str> {
    let hint = match status {
        885 | 886 | 887 => Some(
            "endpoint get.php bloqué par le panel (souvent indépendant du User-Agent). Fallback API Xtream.",
        ),
        401 | 403 => Some("identifiants refusés / UA filtré"),
        404 => Some("endpoint introuvable"),
        429 => Some("trop de requêtes — réessayez"),
        500..=599 => Some("erreur serveur IPTV"),
        _ => None,
    };
    if let Some(h) = hint {
        trace!(status, hint = h, "HTTP status hint");
    }
    hint
}

pub fn map_http_error(err: reqwest::Error) -> ProviderError {
    if let Some(status) = err.status() {
        let code = status.as_u16();
        if let Some(hint) = http_status_hint(code) {
            debug!(code, hint, "mapped HTTP error with hint");
            return ProviderError::Message(format!("HTTP {code}: {hint}"));
        }
        debug!(code, "mapped HTTP error");
        return ProviderError::Message(format!("HTTP {code}: {err}"));
    }
    debug!(error = %err, "mapped network HTTP error");
    ProviderError::Http(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_get_php_style() {
        let c = parse_xtream_get_php(
            "http://panel.example/get.php?username=abc&password=xyz&type=m3u_plus&output=ts",
        )
        .unwrap();
        assert_eq!(c.username, "abc");
        assert_eq!(c.password, "xyz");
        assert!(c.base.starts_with("http://panel.example"));
    }

    #[test]
    fn preserves_path_prefix_on_get_php() {
        let c = parse_xtream_get_php(
            "http://panel.example/c/get.php?username=abc&password=xyz",
        )
        .unwrap();
        assert_eq!(c.base, "http://panel.example/c");
        let portal = Url::parse(&c.base).unwrap();
        let api = join_portal_script(&portal, "player_api.php");
        assert_eq!(api.path(), "/c/player_api.php");
    }
}
