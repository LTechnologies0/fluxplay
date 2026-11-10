#![allow(dead_code)]
//! Compile-time + runtime contracts for soft RAM / UI coherence.
//!
//! These `const` checks fail **at rustc time** if budgets drift. Property tests
//! (`proptest`) and fuzz targets exercise the same contracts with random input.

use crate::browse_tiles::{BROWSE_TILE_CACHE_CAP, BROWSE_TILE_FETCH_LIMIT};
use crate::browser::{MOSAIC_PAGE_ITEMS, VIRTUAL_OVERSCAN};
use crate::images::{MAX_RAM_HANDLES, MAX_UI_EDGE_PX};

/// Soft-present Auto ceiling (desktop) — must stay ≤ 720p for the ~100 Mo plan.
pub const SOFT_PRESENT_MAX_W: u32 = 1280;
pub const SOFT_PRESENT_MAX_H: u32 = 720;

/// Poster decode longest-edge hard cap (display_caps).
pub const DECODE_EDGE_HARD_CAP: u32 = 300;

/// iced soft Allocation hold depth — matches `iced_wgpu` worker video atlases[5].
/// Host RGBA is reclaimed when a hold is expelled (W4-01); do not shrink without
/// blink measurement against atlas_binds.
pub const SOFT_HOLD_DEPTH: usize = 5;

/// SoftPump recycle pool slots (upload + pending + mpv scratch headroom).
pub const SOFT_RGBA_RECYCLE_POOL: usize = 3;

/// Max host soft RGBA retained by holds at Auto ceiling (bytes, exclusive of pool).
pub const SOFT_HOLD_HOST_BYTES_MAX: usize =
    SOFT_HOLD_DEPTH * (SOFT_PRESENT_MAX_W as usize) * (SOFT_PRESENT_MAX_H as usize) * 4;

// ── rustc-time budget locks (plan RAM ~100 Mo) ──────────────────────────────
const _: () = assert!(BROWSE_TILE_FETCH_LIMIT > 0);
const _: () = assert!(BROWSE_TILE_FETCH_LIMIT <= 512);
const _: () = assert!(BROWSE_TILE_CACHE_CAP <= 4_096);
const _: () = assert!(BROWSE_TILE_FETCH_LIMIT < BROWSE_TILE_CACHE_CAP);
const _: () = assert!(MAX_RAM_HANDLES == 72);
const _: () = assert!(MAX_UI_EDGE_PX <= 300);
const _: () = assert!(SOFT_PRESENT_MAX_W * SOFT_PRESENT_MAX_H <= 1280 * 720);
const _: () = assert!(DECODE_EDGE_HARD_CAP <= 300);
const _: () = assert!(MOSAIC_PAGE_ITEMS > 0 && MOSAIC_PAGE_ITEMS <= MAX_RAM_HANDLES);
const _: () = assert!(VIRTUAL_OVERSCAN <= 8);
const _: () = assert!(SOFT_HOLD_DEPTH == 5);
const _: () = assert!(SOFT_RGBA_RECYCLE_POOL == 3);
const _: () = assert!(SOFT_HOLD_HOST_BYTES_MAX <= 5 * 1280 * 720 * 4);

/// Validate a browse window health report — used by fill paths + property tests.
#[cfg(any(test, feature = "fuzzing"))]
pub fn assert_viewport_sane(
    window_len: usize,
    filled: usize,
    holes: usize,
    unpaintable: usize,
) -> Result<(), &'static str> {
    if filled > window_len {
        return Err("filled > window_len");
    }
    if holes + filled != window_len {
        return Err("holes + filled != window_len");
    }
    if unpaintable > filled {
        return Err("unpaintable > filled");
    }
    Ok(())
}

/// Soft present request must stay inside the Auto 720p envelope (unless UHD env).
pub fn soft_present_within_budget(w: u32, h: u32, allow_uhd: bool) -> bool {
    if allow_uhd {
        return w > 0 && h > 0 && w <= 3840 && h <= 2160;
    }
    w > 0 && h > 0 && w <= SOFT_PRESENT_MAX_W && h <= SOFT_PRESENT_MAX_H
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browse_tiles::{BrowseTile, BrowseTileCache};
    use proptest::prelude::*;

    fn tile(id: &str, name: &str) -> BrowseTile {
        BrowseTile {
            id: id.into(),
            name: name.into(),
            art_url: None,
            category_id: None,
            source_id: None,
            year: None,
            genre: None,
            rating: None,
            group: None,
        }
    }

    #[test]
    fn compile_time_budgets_are_live() {
        assert_eq!(MAX_RAM_HANDLES, 72);
        assert!(BROWSE_TILE_FETCH_LIMIT < BROWSE_TILE_CACHE_CAP);
        assert!(MOSAIC_PAGE_ITEMS <= MAX_RAM_HANDLES);
        assert!(VIRTUAL_OVERSCAN <= 8);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn viewport_health_math_holds(
            start in 0usize..200,
            len in 0usize..80,
            fill_every in 1usize..9,
        ) {
            let end = start + len;
            let mut c = BrowseTileCache::default();
            for pos in (start..end).step_by(fill_every) {
                c.store_at(vec![(pos, tile(&pos.to_string(), "ok"))]);
            }
            let h = c.viewport_health(start, end);
            assert_viewport_sane(h.window_len, h.filled, h.holes, h.unpaintable)
                .expect("viewport invariant");
            prop_assert_eq!(h.coverage(), if h.window_len == 0 {
                1.0
            } else {
                h.filled as f32 / h.window_len as f32
            });
        }

        #[test]
        fn store_contiguous_never_exceeds_cap(
            start in 0usize..64,
            n in 1usize..600,
        ) {
            let mut c = BrowseTileCache::default();
            let tiles: Vec<_> = (0..n)
                .map(|i| tile(&format!("{i}"), &format!("t{i}")))
                .collect();
            c.store_contiguous(start, tiles);
            prop_assert!(c.len() <= BROWSE_TILE_CACHE_CAP);
        }

        #[test]
        fn soft_present_budget_rejects_1080p_auto(
            w in 2u32..4000u32,
            h in 2u32..3000u32,
        ) {
            let (ew, eh) = (w & !1, h & !1);
            let ok = soft_present_within_budget(ew, eh, false);
            if ew <= SOFT_PRESENT_MAX_W && eh <= SOFT_PRESENT_MAX_H {
                prop_assert!(ok);
            } else {
                prop_assert!(!ok);
            }
        }

        #[test]
        fn first_unfilled_run_is_inside_window(
            start in 0usize..100,
            len in 1usize..50,
            hole_at in 0usize..50,
        ) {
            let end = start + len;
            let mut c = BrowseTileCache::default();
            for pos in start..end {
                if pos != start + (hole_at % len) {
                    c.store_at(vec![(pos, tile("x", "x"))]);
                }
            }
            if let Some((hs, he)) = c.first_unfilled_run(start, end) {
                prop_assert!(hs >= start && he <= end && hs < he);
            }
        }
    }
}
