#![no_main]
//! Protocol router — every URL-like blob must not panic.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    if s.len() > 4096 {
        return;
    }
    let _ = fluxplay_player::fuzz_api::route(s);
});
