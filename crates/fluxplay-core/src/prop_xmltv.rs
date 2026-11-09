//! Property tests for XMLTV parser robustness.

#![cfg(test)]

use proptest::prelude::*;

use crate::xmltv::parse_xmltv;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn parse_xmltv_never_panics(body in "\\PC{0,4000}") {
        let _ = parse_xmltv(&body);
    }

    #[test]
    fn tiny_valid_guide_ok(
        ch in "[a-z0-9.]{2,16}",
        title in "[A-Za-z0-9][A-Za-z0-9 ]{0,39}",
    ) {
        let body = format!(
            r#"<?xml version="1.0"?><tv>
  <programme start="20240101120000 +0000" stop="20240101130000 +0000" channel="{ch}">
    <title>{title}</title>
  </programme>
</tv>"#
        );
        let progs = parse_xmltv(&body);
        prop_assert!(progs.is_ok(), "{progs:?}");
        let progs = progs.unwrap();
        prop_assert_eq!(progs.len(), 1);
        prop_assert_eq!(&progs[0].channel_id, &ch);
        // Parser trims title text nodes.
        prop_assert_eq!(progs[0].title.as_str(), title.trim());
    }

    #[test]
    fn programmes_never_have_inverted_times(body in "\\PC{0,2000}") {
        if let Ok(progs) = parse_xmltv(&body) {
            for p in progs {
                prop_assert!(p.stop >= p.start, "inverted EPG window");
            }
        }
    }
}
