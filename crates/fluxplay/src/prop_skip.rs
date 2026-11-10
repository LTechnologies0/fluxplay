//! Property tests for chapter skip heuristics (via player crate).
#![cfg(test)]

use proptest::prelude::*;

use fluxplay_player::{
    classify_chapter_title, credits_start_from_chapters, opening_span_from_chapters, ChapterClass,
};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn classify_total(title in "\\PC{0,200}") {
        let _ = classify_chapter_title(&title);
    }

    #[test]
    fn opening_and_credits_from_list(
        titles in prop::collection::vec("\\PC{0,64}", 0..16),
        series in any::<bool>(),
        duration in 0.0f64..10_000.0,
    ) {
        let chapters: Vec<(f64, String)> = titles
            .into_iter()
            .enumerate()
            .map(|(i, t)| (i as f64 * 40.0, t))
            .collect();
        let _ = opening_span_from_chapters(&chapters, series);
        let _ = credits_start_from_chapters(&chapters, duration);
    }
}

#[test]
fn known_labels() {
    assert_eq!(classify_chapter_title("OP"), ChapterClass::Intro);
    assert_eq!(classify_chapter_title("ED"), ChapterClass::Credits);
    assert_eq!(classify_chapter_title("Previously on"), ChapterClass::Recap);
}
