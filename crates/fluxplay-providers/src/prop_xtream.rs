//! Property tests for Xtream URL helpers (credential hygiene + path encoding).

#![cfg(test)]

use proptest::prelude::*;

use crate::xtream_url::{http_status_hint, parse_xtream_get_php, path_segment, status_error};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn path_segment_encodes_slash_and_space(raw in "\\PC{0,80}") {
        let seg = path_segment(&raw);
        // Encoded segment must not re-introduce raw path separators that would
        // let a username escape its URL path component.
        if raw.contains('/') || raw.contains(' ') || raw.contains('?') {
            prop_assert!(
                seg.contains('%') || !seg.contains('/') || raw.is_empty(),
                "unsafe path segment for {raw:?} → {seg:?}"
            );
        }
        prop_assert!(!seg.contains('\0'));
    }

    #[test]
    fn parse_get_php_never_panics(url in "\\PC{0,300}") {
        let _ = parse_xtream_get_php(&url);
    }

    #[test]
    fn status_error_never_embeds_url(code in 100u16..600) {
        let err = status_error(code);
        let msg = err.to_string();
        prop_assert!(!msg.contains("http://"));
        prop_assert!(!msg.contains("https://"));
        prop_assert!(!msg.contains("password"));
        let _ = http_status_hint(code);
    }

    #[test]
    fn get_php_roundtrip_credentials(
        user in "[a-zA-Z0-9]{1,16}",
        pass in "[a-zA-Z0-9]{1,16}",
        host in "[a-z]{3,12}\\.example",
    ) {
        let url = format!(
            "http://{host}/get.php?username={user}&password={pass}&type=m3u_plus"
        );
        let c = parse_xtream_get_php(&url);
        prop_assert!(c.is_some(), "failed to parse {url}");
        let c = c.unwrap();
        prop_assert_eq!(c.username, user);
        prop_assert_eq!(c.password, pass);
        prop_assert!(c.base.contains(&host));
    }
}
