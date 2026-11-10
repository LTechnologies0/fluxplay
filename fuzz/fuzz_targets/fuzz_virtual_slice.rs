#![no_main]
//! Virtualization math — blank mosaics / scroll desync crash detectors.
use libfuzzer_sys::fuzz_target;

use fluxplay::fuzz_api::{
    list_row_height, mosaic_row_height, virtual_content_height, virtual_slice,
    MAX_VIRTUAL_CONTENT_PX,
};

fuzz_target!(|data: &[u8]| {
    if data.len() < 16 {
        return;
    }
    // Pack f32-ish params from bytes (little-endian bit patterns).
    let u = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    let f = |o: usize| {
        let bits = u(o);
        let mut v = f32::from_bits(bits);
        if !v.is_finite() {
            v = 0.0;
        }
        v.abs() % 80_000.0
    };
    let scroll_y = f(0);
    let view_h = f(4).max(1.0);
    let item_h = f(8).clamp(0.0, 800.0);
    let total = (u(12) as usize) % 2_000_001;
    let overscan = data.get(16).copied().unwrap_or(0) as usize % 64;

    let s = virtual_slice(scroll_y, view_h, item_h, total, overscan);
    assert!(s.start <= s.end, "start {} > end {}", s.start, s.end);
    assert!(s.end <= total, "end {} > total {}", s.end, total);
    assert!(s.top_spacer.is_finite() && s.top_spacer >= 0.0);
    assert!(s.bottom_spacer.is_finite() && s.bottom_spacer >= 0.0);
    if total > 0 && item_h > 1.0 {
        assert!(s.end > s.start, "empty mount on non-empty catalog");
        let mounted = s.end - s.start;
        assert!(mounted <= total);
        // Visible rows + 2×overscan + a small float-rounding fudge.
        let visible = ((view_h / item_h.max(1.0)).ceil() as usize).saturating_add(2);
        let max_mount = visible
            .saturating_add(overscan.saturating_mul(2))
            .saturating_add(8)
            .min(total);
        assert!(
            mounted <= max_mount.max(512.min(total)),
            "mounted window too large: {mounted} (max {max_mount}) total={total}"
        );
        let mid = mounted as f32 * item_h.max(1.0);
        assert!((s.top_spacer + mid + s.bottom_spacer).is_finite());
    }

    let ch = virtual_content_height(total, view_h, item_h.max(1.0));
    assert!(ch.is_finite() && ch >= 0.0);
    assert!(
        ch <= MAX_VIRTUAL_CONTENT_PX.max(view_h * 4.0) + item_h.max(1.0) * 2.0
            || total < 100,
        "content height blew past soft ceiling: {ch}"
    );
    // Monotonic soft ceiling once the catalog is non-empty. Empty lists use
    // `view_h` as a placeholder paint height, which can be taller than 1×item.
    if total > 0 {
        let ch2 = virtual_content_height(total.saturating_add(1), view_h, item_h.max(1.0));
        assert!(ch2 + 0.5 >= ch, "content height shrank: {ch} → {ch2}");
    }

    // Row helpers stay finite for fuzzed tile / touch sizes.
    if data.len() >= 24 {
        let tile = f(17).clamp(48.0, 600.0);
        let touch = f(20).clamp(32.0, 96.0);
        let mh = mosaic_row_height(tile);
        let lh = list_row_height(touch);
        assert!(mh.is_finite() && mh > 0.0);
        assert!(lh.is_finite() && lh >= touch);
    }
});
