//! Property tests for metadata title / text formatters.

#![cfg(test)]

use proptest::prelude::*;

use crate::metadata::{
    clean_text, fmt_genre, fmt_people, fmt_plot, fmt_rated, fmt_rating, fmt_runtime, fmt_year,
    parse_title_query,
};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn clean_text_never_panics(raw in "\\PC{0,500}") {
        let out = clean_text(&raw);
        prop_assert!(!out.contains('\0'));
    }

    #[test]
    fn parse_title_query_never_panics(raw in "\\PC{0,200}") {
        let q = parse_title_query(&raw);
        prop_assert!(!q.title.contains('\0'));
    }

    #[test]
    fn formatters_tolerate_junk(
        raw in prop::option::of("\\PC{0,80}"),
        title in "[A-Za-z0-9 ]{0,40}",
    ) {
        let _ = fmt_plot(raw.as_deref(), &title);
        let _ = fmt_rating(raw.as_deref());
        let _ = fmt_runtime(raw.as_deref());
        let _ = fmt_year(raw.as_deref());
        let _ = fmt_genre(raw.as_deref());
        let _ = fmt_people(raw.as_deref());
        let _ = fmt_rated(raw.as_deref());
    }
}
