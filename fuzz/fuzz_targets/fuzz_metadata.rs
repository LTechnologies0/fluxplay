#![no_main]
//! Metadata cleaners / title query — UI text must stay bounded & panic-free.
use libfuzzer_sys::fuzz_target;

use fluxplay::fuzz_api::{
    age_rank, age_rating, clean_text, clean_title, display_genres, fmt_airdate, fmt_genre,
    fmt_people, fmt_plot, fmt_rated, fmt_rating, fmt_runtime, fmt_year, parse_title_query,
};

fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else {
        return;
    };
    let bound = raw.len().saturating_mul(8).max(1024) + 128;

    let q = parse_title_query(raw);
    assert!(q.title.len() <= bound);
    assert!(!q.title.contains('\0'));

    let ct = clean_title(raw);
    let cx = clean_text(raw);
    assert!(ct.len() <= bound && cx.len() <= bound);
    assert!(!ct.contains('\0') && !cx.contains('\0'));

    let genres = display_genres(raw);
    assert!(genres.len() <= raw.len().saturating_add(8).max(64));
    for g in &genres {
        assert!(!g.contains('\0'));
        assert!(g.len() <= bound);
    }

    let _ = age_rating(Some(raw));
    let _ = age_rank(raw);
    let _ = fmt_plot(Some(raw), raw);
    let _ = fmt_rating(Some(raw));
    let _ = fmt_runtime(Some(raw));
    let _ = fmt_airdate(Some(raw));
    let _ = fmt_year(Some(raw));
    let _ = fmt_genre(Some(raw));
    let _ = fmt_people(Some(raw));
    let _ = fmt_rated(Some(raw));
});
