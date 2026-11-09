#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else {
        return;
    };
    let _ = fluxplay_core::protocol::StreamScheme::parse(raw);
    let _ = fluxplay_core::protocol::StreamUrl::parse(raw);
});
