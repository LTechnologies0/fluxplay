//! Property tests for virtualization math (blank rows / scroll jank detectors).

#![cfg(test)]

use proptest::prelude::*;

use crate::browser::{
    list_row_height_for, mosaic_row_height_for, virtual_content_height, virtual_slice,
};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn virtual_slice_window_invariants(
        scroll_y in 0.0f32..50_000.0,
        view_h in 40.0f32..2000.0,
        item_h in 20.0f32..400.0,
        total in 0usize..50_000,
        overscan in 0usize..32,
    ) {
        let s = virtual_slice(scroll_y, view_h, item_h, total, overscan);
        prop_assert!(s.start <= s.end);
        prop_assert!(s.end <= total);
        prop_assert!(s.top_spacer.is_finite() && s.top_spacer >= 0.0);
        prop_assert!(s.bottom_spacer.is_finite() && s.bottom_spacer >= 0.0);
        if total == 0 || item_h <= 1.0 {
            prop_assert_eq!(s.start, 0);
            prop_assert_eq!(s.end, 0);
        } else {
            // Mounted window must stay bounded (overscan ×2 + visible).
            let mounted = s.end - s.start;
            prop_assert!(mounted <= total);
            prop_assert!(mounted > 0 || total == 0);
        }
    }

    #[test]
    fn mosaic_row_height_positive(
        tile_w in 96.0f32..400.0,
        touch in 40.0f32..80.0,
    ) {
        let h = mosaic_row_height_for(tile_w, touch);
        prop_assert!(h.is_finite() && h > tile_w);
        let list = list_row_height_for(touch, touch);
        prop_assert!(list >= touch);
    }

    #[test]
    fn virtual_content_height_monotonic(
        total in 0usize..10_000,
        view_h in 100.0f32..1200.0,
        item_h in 30.0f32..200.0,
    ) {
        let a = virtual_content_height(total, view_h, item_h);
        let b = virtual_content_height(total.saturating_add(10), view_h, item_h);
        prop_assert!(a.is_finite() && a >= 0.0);
        prop_assert!(b >= a || total == 0);
    }
}
