#![no_main]
//! Credential scrubbing — http(s) tokens must not survive redaction.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else {
        return;
    };
    let out = fluxplay_providers::redact_urls(raw);
    assert!(out.len() <= raw.len().saturating_add(8));
    let lower = out.to_ascii_lowercase();
    // After redaction, no live http(s) scheme may remain (credentials ride in URLs).
    assert!(
        !lower.contains("http://") && !lower.contains("https://"),
        "url leaked through redact_urls: {out:?}"
    );
});
