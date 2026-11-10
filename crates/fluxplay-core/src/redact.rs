//! Redact credentials that ride inside free-form error / status text.

/// Replace every `http(s)://…` token in `text` with `<url>` (ASCII case-insensitive).
///
/// Xtream / M3U URLs often embed `username=` / `password=` in the query string.
/// Anything that reaches [`crate::codes::ensure_status`] or a `Display` impl must
/// pass through this (or an equivalent host-only helper) before UI / logs.
pub fn redact_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let lower = rest.to_ascii_lowercase();
        let next = ["http://", "https://"]
            .iter()
            .filter_map(|p| lower.find(p))
            .min();
        let Some(start) = next else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..start]);
        out.push_str("<url>");
        let tail = &rest[start..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, ')' | '"' | '\'' | '>'))
            .unwrap_or(tail.len());
        rest = &tail[end..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_https_with_password_query() {
        let raw = "http error: https://host/get.php?username=u&password=s3cret failed";
        let out = redact_urls(raw);
        assert!(out.contains("<url>"), "{out}");
        assert!(!out.contains("s3cret"), "{out}");
        assert!(!out.contains("password="), "{out}");
    }

    #[test]
    fn leaves_non_url_text() {
        assert_eq!(redact_urls("no link here"), "no link here");
    }
}
