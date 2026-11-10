#![no_main]
//! Soft-present Auto envelope — reject 1080p+ when UHD is off (RAM plan).
use libfuzzer_sys::fuzz_target;

use fluxplay::fuzz_api::{
    soft_present_within_budget, DECODE_EDGE_HARD_CAP, SOFT_PRESENT_MAX_H, SOFT_PRESENT_MAX_W,
};

fuzz_target!(|data: &[u8]| {
    if data.len() < 9 {
        return;
    }
    let w = u32::from_le_bytes(data[0..4].try_into().unwrap()) % 8192;
    let h = u32::from_le_bytes(data[4..8].try_into().unwrap()) % 8192;
    let allow_uhd = data[8] & 1 == 1;

    let ok = soft_present_within_budget(w, h, allow_uhd);
    if w == 0 || h == 0 {
        assert!(!ok, "zero dimension must fail");
        return;
    }
    if !allow_uhd {
        if w > SOFT_PRESENT_MAX_W || h > SOFT_PRESENT_MAX_H {
            assert!(!ok, "{w}x{h} must fail Auto 720p envelope");
        }
        if w <= SOFT_PRESENT_MAX_W && h <= SOFT_PRESENT_MAX_H {
            assert!(ok, "{w}x{h} inside Auto envelope");
        }
    } else if w <= 3840 && h <= 2160 {
        assert!(ok, "UHD path should accept {w}x{h}");
    }

    // Decode-edge hard cap is a compile-time budget — keep fuzzed edges in range.
    let edge = (w.max(h)).min(DECODE_EDGE_HARD_CAP.saturating_mul(2));
    assert!(DECODE_EDGE_HARD_CAP <= 300);
    let _ = edge;
});
