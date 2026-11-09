#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else {
        return;
    };
    let s = fluxplay::fuzz_api::sanitize_file_name(raw);
    assert!(!s.contains('/'));
    assert!(!s.contains('\\'));
    assert!(!s.contains('%'));
    assert!(!s.is_empty());
});
