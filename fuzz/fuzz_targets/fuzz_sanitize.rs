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
    assert!(!s.contains('\0'));
    assert!(!s.contains(':'));
    assert!(!s.contains('*'));
    assert!(!s.contains('?'));
    assert!(!s.contains('"'));
    assert!(!s.contains('<'));
    assert!(!s.contains('>'));
    assert!(!s.contains('|'));
    assert!(!s.chars().any(|c| c.is_control()));
    assert!(!s.is_empty());
    assert!(s.chars().count() <= 150);
    assert!(s.len() <= 240, "utf-8 byte ceiling: {}", s.len());
    assert!(s != ".." && s != ".");
    assert!(!s.starts_with('.'));
    // Windows reserved device names must not pass through unchanged as a path leaf.
    let upper = s.to_ascii_uppercase();
    for bad in ["CON", "PRN", "AUX", "NUL", "COM1", "LPT1"] {
        assert_ne!(upper, bad, "reserved device name survived: {s}");
    }
});
