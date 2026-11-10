//! Property tests for UiSurface / LayoutMetrics (TDD gate).

#![cfg(test)]

use proptest::prelude::*;

use crate::theme::{Breakpoint, LayoutMetrics};
use crate::ui_surface::UiSurface;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn layout_metrics_positive(
        w in 280.0f32..4000.0,
        h in 200.0f32..2400.0,
    ) {
        let m = LayoutMetrics::compute(w, h);
        prop_assert!(m.cols >= 1);
        prop_assert!(m.tile_w >= 96.0);
        prop_assert!(m.content_w >= 120.0);
        prop_assert!(m.pad >= 0.0);
        prop_assert!(m.title_size > 0.0 && m.body_size > 0.0);
        prop_assert!(m.width >= 280.0 && m.height >= 200.0);
    }

    #[test]
    fn phone_compact_never_gets_side_rail(
        w in 280.0f32..520.0,
        h in 480.0f32..900.0,
    ) {
        let m = LayoutMetrics::compute(w, h);
        prop_assert_eq!(m.bp, Breakpoint::Phone);
        prop_assert!(m.top_nav);
        prop_assert_eq!(m.rail_w, 0.0);
        prop_assert_eq!(m.cat_w, 0.0);
    }

    #[test]
    fn wide_short_landscape_stays_compact(
        w in 700.0f32..1200.0,
        h in 320.0f32..500.0,
    ) {
        // Phone landscape is often ≥600 wide but short — must not open desktop rail.
        let m = LayoutMetrics::compute(w, h);
        prop_assert!(m.top_nav || m.bp == Breakpoint::Phone);
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
        prop_assert!(a.touch_target() >= 40.0);
        prop_assert!(a.type_scale() >= 1.0);
        let bp = a.layout_breakpoint(w, h);
        if a.is_television() {
            prop_assert_eq!(bp, Breakpoint::Tv);
        }
    }

    #[test]
    fn force_tv_env_overrides_geometry(
        w in 800.0f32..1920.0,
        h in 600.0f32..1080.0,
    ) {
        // Isolation: only assert the contract when the env is already set by the operator/CI.
        if std::env::var_os("FLUXPLAY_FORCE_TV").is_none() {
            return Ok(());
        }
        prop_assert!(UiSurface::detect(w, h).is_television());
    }
}
