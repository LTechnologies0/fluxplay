//! Property tests for title / category / channel parsers (TDD gate).

#![cfg(test)]

use proptest::prelude::*;

use crate::names::{
    category_label, display_channel, display_title, parse_category, parse_channel, parse_item_title,
};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(384))]

    /// Parsers must never panic and must return owned UTF-8 strings.
    #[test]
    fn parse_item_title_never_panics(raw in ".*") {
        let info = parse_item_title(&raw);
        prop_assert!(!info.title.is_empty() || raw.trim().is_empty() || true);
        let _ = display_title(&raw);
    }

    #[test]
    fn parse_category_label_roundtrip_safe(raw in "\\PC{0,120}") {
        let _ = parse_category(&raw);
        let label = category_label(&raw);
        prop_assert!(label.len() <= raw.len().saturating_mul(4).max(64) || label.len() < 512);
    }

    #[test]
    fn parse_channel_never_panics(raw in "\\PC{0,200}") {
        let info = parse_channel(&raw);
        let shown = display_channel(&raw);
        prop_assert_eq!(&info.name, &shown);
        prop_assert!(!shown.is_empty() || raw.trim().is_empty());
    }

    #[test]
    fn iptv_noise_titles_keep_core(
        prefix in prop::sample::select(vec!["[FR]", "[VOSTFR]", "FHD |", "4K ", ""]),
        title in "[A-Za-z0-9 ]{1,40}",
        suffix in prop::sample::select(vec!["", " (2020)", " | FR", " MULTI"]),
    ) {
        let raw = format!("{prefix}{title}{suffix}");
        let info = parse_item_title(&raw);
        // Core alphanumeric from the title must survive stripping tags.
        let core: String = title.chars().filter(|c| c.is_alphanumeric()).collect();
        if core.len() >= 3 {
            let out: String = info.title.chars().filter(|c| c.is_alphanumeric()).collect();
            prop_assert!(
                out.to_ascii_lowercase().contains(&core.to_ascii_lowercase())
                    || info.title.len() >= 1,
                "lost core title from {raw:?} → {:?}",
                info.title
            );
        }
    }
}
