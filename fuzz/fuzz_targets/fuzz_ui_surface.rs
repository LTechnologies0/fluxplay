#![no_main]
//! Form-factor detect — NaN/Inf geometry must not poison layout metrics.
use libfuzzer_sys::fuzz_target;

use fluxplay::fuzz_api::UiSurface;

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    let mut w = f32::from_bits(u32::from_le_bytes(data[0..4].try_into().unwrap()));
    let mut h = f32::from_bits(u32::from_le_bytes(data[4..8].try_into().unwrap()));
    if !w.is_finite() {
        w = 0.0;
    }
    if !h.is_finite() {
        h = 0.0;
    }
    w = w.abs() % 16_000.0;
    h = h.abs() % 16_000.0;

    let surface = UiSurface::detect(w, h);
    let target = surface.touch_target();
    let pad = surface.overscan_pad();
    let scale = surface.type_scale();
    assert!(target.is_finite() && target >= 32.0 && target <= 128.0);
    assert!(pad.is_finite() && pad >= 0.0 && pad <= 64.0);
    assert!(scale.is_finite() && scale >= 1.0 && scale <= 2.0);

    let bp = surface.layout_breakpoint(w.max(1.0), h.max(1.0));
    let _ = bp;
    let _ = surface.is_touch();
    let _ = surface.wants_focus_chrome();
    let _ = surface.is_television();
});
