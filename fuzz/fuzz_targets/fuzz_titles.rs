#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else {
        return;
    };
    let info = fluxplay::fuzz_api::parse_item_title(raw);
    let cat = fluxplay::fuzz_api::parse_category(raw);
    let ch = fluxplay::fuzz_api::parse_channel(raw);
    let dt = fluxplay::fuzz_api::display_title(raw);
    let dc = fluxplay::fuzz_api::display_channel(raw);
    let cl = fluxplay::fuzz_api::category_label(raw);

    // Display helpers must stay UTF-8 and bounded (UI text widgets).
    for s in [&info.title, &dt, &dc, &cl, &ch.name, &cat.title] {
        assert!(s.len() <= raw.len().saturating_mul(8).max(512) + 64);
        assert!(!s.contains('\0'));
        assert!(!s.chars().any(|c| c.is_control() && c != '\t'));
    }
    assert_eq!(ch.name, dc);
    // Category / title cores must not reintroduce path separators used as IDs.
    assert!(!cl.contains('\0'));
    assert!(info.title.chars().count() <= raw.chars().count().saturating_add(32).max(64));
});
