//! Guided API-key acquisition for TMDB / OMDb.
//!
//! FluxPlay never ships shared keys. The user creates their own free account.
//! Captchas are solved by the human in a real browser / WebView window — we do
//! not automate captcha bypass (that would look like abuse).
//!
//! While the assistant is open the UI polls the clipboard (via iced) so a
//! copied token lands in settings without retyping.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiProvider {
    Tmdb,
    Omdb,
}

impl ApiProvider {
    pub fn label(self) -> &'static str {
        match self {
            Self::Tmdb => "TMDB",
            Self::Omdb => "OMDb",
        }
    }

    pub fn signup_url(self) -> &'static str {
        match self {
            Self::Tmdb => "https://www.themoviedb.org/signup",
            Self::Omdb => "https://www.omdbapi.com/apikey.aspx",
        }
    }

    pub fn key_page_url(self) -> &'static str {
        match self {
            Self::Tmdb => "https://www.themoviedb.org/settings/api",
            Self::Omdb => "https://www.omdbapi.com/apikey.aspx",
        }
    }
}

/// Detect a TMDB read token / v3 key or an OMDb key in free-form clipboard text.
pub fn extract_key(text: &str) -> Option<(ApiProvider, String)> {
    let t = text.trim();
    if t.is_empty() || t.len() > 4096 {
        return None;
    }
    // TMDB Read Access Token (JWT)
    if let Some(tok) = t
        .split_whitespace()
        .find(|w| w.starts_with("eyJ") && w.len() > 40)
    {
        let clean = tok.trim_matches(|c: char| {
            !c.is_ascii_alphanumeric() && c != '-' && c != '_' && c != '.'
        });
        if clean.starts_with("eyJ") && clean.len() > 40 {
            return Some((ApiProvider::Tmdb, clean.to_string()));
        }
    }
    // TMDB v3 api_key — 32 hex chars
    for w in t.split_whitespace() {
        let clean: String = w.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        if clean.len() == 32 {
            return Some((ApiProvider::Tmdb, clean));
        }
    }
    // OMDb keys are typically 8–23 alphanumeric.
    for w in t.split_whitespace() {
        let clean: String = w.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        if (8..24).contains(&clean.len()) && !clean.starts_with("eyJ") && clean.len() != 32 {
            return Some((ApiProvider::Omdb, clean));
        }
    }
    None
}

/// Open the provider page so the user can pass any captcha legitimately.
pub fn open_provider_page(provider: ApiProvider, signup: bool) {
    let url = if signup {
        provider.signup_url()
    } else {
        provider.key_page_url()
    };
    #[cfg(target_os = "android")]
    {
        let _ = crate::android_intent::open_url(url, None);
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = open::that(url);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_tmdb_jwt() {
        let (p, k) =
            extract_key("  eyJhbGciOiJIUzI1NiJ9.e30.signaturepadpadpadpad  ").unwrap();
        assert_eq!(p, ApiProvider::Tmdb);
        assert!(k.starts_with("eyJ"));
    }

    #[test]
    fn detects_tmdb_v3_hex() {
        let (p, k) = extract_key("abc123def4567890abc123def4567890").unwrap();
        assert_eq!(p, ApiProvider::Tmdb);
        assert_eq!(k.len(), 32);
    }

    #[test]
    fn detects_omdb_short() {
        let (p, k) = extract_key("a1b2c3d4").unwrap();
        assert_eq!(p, ApiProvider::Omdb);
        assert_eq!(k, "a1b2c3d4");
    }
}
