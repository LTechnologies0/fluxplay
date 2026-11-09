#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(body) = std::str::from_utf8(data) else {
        return;
    };
    // Must never panic — Err is fine.
    let _ = fluxplay_core::m3u::parse_m3u(body, None);
});
