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

    let mut base = url.clone();
    base.set_path("");
    base.set_query(None);
    base.set_fragment(None);

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
}
