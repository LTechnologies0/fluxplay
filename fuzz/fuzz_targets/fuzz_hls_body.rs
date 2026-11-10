#![no_main]
//! HLS/m3u8 body probe (no network).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    let probe = fluxplay_player::fuzz_api::parse_hls_manifest_body(s);
    assert_eq!(probe.bytes, s.len());
    // `is_master` is substring-based; variant_count only counts line prefixes.
    if probe.variant_count > 0 {
        assert!(probe.is_master);
    }
});
