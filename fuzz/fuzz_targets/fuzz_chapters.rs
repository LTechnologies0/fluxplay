#![no_main]
//! Chapter title classify + opening/credits spans.
use libfuzzer_sys::fuzz_target;

use fluxplay_player::fuzz_api::{
    classify_chapter_title, credits_from_chapters, opening_from_chapters, ChapterClass,
};

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let series = data[0] & 1 == 1;
    let rest = &data[1..];
    let Ok(s) = std::str::from_utf8(rest) else {
        return;
    };
    let _ = classify_chapter_title(s);

    // Build a tiny chapter list from newline-split titles.
    let mut chapters = Vec::new();
    for (i, line) in s.lines().take(24).enumerate() {
        if line.len() > 128 {
            continue;
        }
        chapters.push((i as f64 * 30.0, line.to_string()));
    }
    let _ = opening_from_chapters(&chapters, series);
    let duration = (chapters.len() as f64) * 40.0 + 120.0;
    let _ = credits_from_chapters(&chapters, duration);

    // Invariant: known short tokens stay stable.
    if s.eq_ignore_ascii_case("op") {
        assert_eq!(classify_chapter_title(s), ChapterClass::Intro);
    }
    if s.eq_ignore_ascii_case("ed") {
        assert_eq!(classify_chapter_title(s), ChapterClass::Credits);
    }
});
