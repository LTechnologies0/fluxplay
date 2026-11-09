//! Property tests for UiSurface / LayoutMetrics (TDD gate).

#![cfg(test)]

use proptest::prelude::*;

use crate::theme::{Breakpoint, LayoutMetrics};
use crate::ui_surface::UiSurface;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn layout_metrics_positive(
        w in 280.0f32..4000.0,
        h in 200.0f32..2400.0,
    ) {
        let m = LayoutMetrics::compute(w, h);
        prop_assert!(m.cols >= 1);
        prop_assert!(m.tile_w >= 96.0);
        prop_assert!(m.content_w >= 120.0);
        prop_assert!(m.touch_target >= 40.0);
        prop_assert!(m.pad >= 0.0);
        prop_assert!(m.title_size > 0.0 && m.body_size > 0.0);
    }

    #[test]
    fn television_layout_is_ten_foot(
        w in 1280.0f32..3840.0,
        h in 720.0f32..2160.0,
    ) {
        let m = LayoutMetrics::compute_for(UiSurface::Television, w, h);
        prop_assert_eq!(m.bp, Breakpoint::Tv);
        prop_assert!(m.focus_chrome);
        prop_assert!(m.touch_target >= 64.0);
        prop_assert!(m.pad >= 24.0);
        prop_assert!(m.cols <= 6);
        prop_assert!(m.surface.is_television());
    }

    #[test]
    fn phone_compact_never_gets_side_rail(
        w in 280.0f32..520.0,
        h in 480.0f32..900.0,
    ) {
        let m = LayoutMetrics::compute_for(UiSurface::Phone, w, h);
        prop_assert_eq!(m.bp, Breakpoint::Phone);
        prop_assert!(m.top_nav);
        prop_assert_eq!(m.rail_w, 0.0);
        prop_assert_eq!(m.cat_w, 0.0);
    }

    #[test]
    fn detect_is_deterministic(w in 280.0f32..2400.0, h in 200.0f32..1600.0) {
        // Without FLUXPLAY_FORCE_TV, detect is pure geometry.
        if std::env::var_os("FLUXPLAY_FORCE_TV").is_some() {
            return Ok(());
        }
        let a = UiSurface::detect(w, h);
        let b = UiSurface::detect(w, h);
        prop_assert_eq!(a, b);
        prop_assert!(!a.is_television() || matches!(a, UiSurface::Television));
    }
}
