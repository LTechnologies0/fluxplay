#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(conf) = std::str::from_utf8(data) else {
        return;
    };
    let _ = fluxplay::fuzz_api::parse_wg_profile(conf);
});
