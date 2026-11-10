#![no_main]
//! Xtream get.php / portal URL helpers — never panic, path segment safe.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else {
        return;
    };
    let _ = fluxplay_providers::xtream_url::parse_xtream_get_php(raw);
    let seg = fluxplay_providers::xtream_url::path_segment(raw);
    // Path segments used in portal URLs must not re-introduce separators.
    assert!(!seg.contains('/'));
    assert!(!seg.contains('\\'));
    assert!(!seg.contains('\0'));

    if let Ok(url) = url::Url::parse(raw) {
        let _ = fluxplay_providers::xtream_url::portal_base_from_url(&url);
        let joined = fluxplay_providers::xtream_url::join_portal_script(&url, "player_api.php");
        assert!(!joined.as_str().is_empty());
    }

    if data.len() >= 2 {
        let code = u16::from_le_bytes([data[0], data[1]]);
        let _ = fluxplay_providers::xtream_url::http_status_hint(code);
        let _ = fluxplay_providers::xtream_url::status_error(code);
    }
});
