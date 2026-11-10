#![allow(dead_code)]
//! Mosaic browse rows — SQLite stays the catalog; RAM only holds a viewport LRU.
//!
//! Paint needs: id, name, art, year, genre, rating. Detail/play load one payload by id.
//!
//! # Contracts
//! - Positions are **browse indices** (0..BrowseIndex::len), not raw SQLite rowids.
//! - [`BrowseTileCache::store_contiguous`] expects tiles ordered for `start..start+n`.
//! - [`BrowseViewportHealth`] is the unit-testable signal for blank / janky mosaics.

use std::collections::HashMap;

use uuid::Uuid;

use fluxplay_core::models::ContentKind;

/// Max mosaic rows kept warm (≈ a few screens × columns). Soft RAM budget.
pub const BROWSE_TILE_CACHE_CAP: usize = 3_200;

/// Max tiles fetched per SQLite fill call (≈ 2–3 mosaic screens).
pub const BROWSE_TILE_FETCH_LIMIT: usize = 384;

/// One mosaic cell. No plot, cast, seasons, or stream URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseTile {
    pub id: String,
    pub name: String,
    pub art_url: Option<String>,
    pub category_id: Option<String>,
    pub source_id: Option<Uuid>,
    pub year: Option<String>,
    pub genre: Option<String>,
    pub rating: Option<String>,
    pub group: Option<String>,
}

impl BrowseTile {
    /// True when the cell can be painted with a real title (not a blank "…").
    pub fn is_paintable(&self) -> bool {
        !self.id.trim().is_empty() && !self.name.trim().is_empty()
    }

    pub fn meta_line(&self, fallback: &str) -> String {
        let mut parts = Vec::new();
        if let Some(y) = self.year.as_deref().filter(|s| !s.is_empty()) {
            parts.push(y.to_string());
        }
        if let Some(g) = self.genre.as_deref().filter(|s| !s.is_empty()) {
            let short = g.split(',').next().unwrap_or(g).trim();
            if !short.is_empty() {
                parts.push(short.to_string());
            }
        }
        if let Some(r) = self.rating.as_deref().filter(|s| !s.is_empty()) {
            parts.push(r.to_string());
        }
        if parts.is_empty() {
            fallback.into()
        } else {
            parts.join(" · ")
        }
    }
}

/// Diagnostic for one browse window — unit-tested so blank/janky paint is catchable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseViewportHealth {
    pub window_len: usize,
    pub filled: usize,
    pub holes: usize,
    pub unpaintable: usize,
}

impl BrowseViewportHealth {
    /// Mosaic is ready when every slot is filled and paintable.
    pub fn is_coherent(&self) -> bool {
        self.window_len == 0 || (self.holes == 0 && self.unpaintable == 0)
    }

    /// Fraction of positions that have a tile (0.0..=1.0). Tests / diagnostics.
    pub fn coverage(&self) -> f32 {
        if self.window_len == 0 {
            1.0
        } else {
            self.filled as f32 / self.window_len as f32
        }
    }
}

/// LRU of slim tiles for the open tab.
#[derive(Debug)]
pub struct BrowseTileCache {
    kind: ContentKind,
    by_pos: HashMap<usize, BrowseTile>,
    touch: HashMap<usize, u64>,
    clock: u64,
    cap: usize,
}

impl Default for BrowseTileCache {
    fn default() -> Self {
        Self {
            kind: ContentKind::Vod,
            by_pos: HashMap::new(),
            touch: HashMap::new(),
            clock: 0,
            cap: BROWSE_TILE_CACHE_CAP,
        }
    }
}

impl BrowseTileCache {
    pub fn clear(&mut self) {
        self.by_pos.clear();
        self.touch.clear();
        self.clock = 0;
    }

    pub fn set_kind(&mut self, kind: ContentKind) {
        if self.kind != kind {
            self.clear();
            self.kind = kind;
        }
    }

    pub fn peek(&self, pos: usize) -> Option<&BrowseTile> {
        self.by_pos.get(&pos)
    }

    /// Mark a hit as recently used (scroll/fill paths only — paint stays `&self`).
    pub fn touch(&mut self, pos: usize) {
        if self.by_pos.contains_key(&pos) {
            self.clock = self.clock.wrapping_add(1);
            self.touch.insert(pos, self.clock);
        }
    }

    pub fn len(&self) -> usize {
        self.by_pos.len()
    }

    /// First contiguous hole run inside `[start, end)`, as `(hole_start, hole_end)`.
    pub fn first_unfilled_run(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        let mut i = start;
        while i < end && self.by_pos.contains_key(&i) {
            i += 1;
        }
        if i >= end {
            return None;
        }
        let hole_start = i;
        while i < end && !self.by_pos.contains_key(&i) {
            i += 1;
        }
        Some((hole_start, i))
    }

    /// Health of a paint window — holes and empty titles cause visible "…" / flicker.
    pub fn viewport_health(&self, start: usize, end: usize) -> BrowseViewportHealth {
        let end = end.max(start);
        let window_len = end - start;
        let mut filled = 0usize;
        let mut unpaintable = 0usize;
        for pos in start..end {
            match self.by_pos.get(&pos) {
                Some(t) if t.is_paintable() => filled += 1,
                Some(_) => {
                    filled += 1;
                    unpaintable += 1;
                }
                None => {}
            }
        }
        BrowseViewportHealth {
            window_len,
            filled,
            holes: window_len.saturating_sub(filled),
            unpaintable,
        }
    }

    /// Store tiles for absolute browse positions `start..start+tiles.len()`.
    pub fn store_contiguous(&mut self, start: usize, tiles: Vec<BrowseTile>) {
        for (i, tile) in tiles.into_iter().enumerate() {
            let pos = start + i;
            self.clock = self.clock.wrapping_add(1);
            self.touch.insert(pos, self.clock);
            self.by_pos.insert(pos, tile);
        }
        self.evict();
    }

    /// Store tiles at explicit browse positions (favorites / search mapping).
    pub fn store_at(&mut self, pairs: Vec<(usize, BrowseTile)>) {
        for (pos, tile) in pairs {
            self.clock = self.clock.wrapping_add(1);
            self.touch.insert(pos, self.clock);
            self.by_pos.insert(pos, tile);
        }
        self.evict();
    }

    fn evict(&mut self) {
        while self.by_pos.len() > self.cap {
            let victim = self
                .touch
                .iter()
                .min_by_key(|(_, g)| *g)
                .map(|(p, _)| *p);
            let Some(pos) = victim else {
                break;
            };
            self.by_pos.remove(&pos);
            self.touch.remove(&pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(id: &str) -> BrowseTile {
        BrowseTile {
            id: id.into(),
            name: id.into(),
            art_url: None,
            category_id: None,
            source_id: None,
            year: None,
            genre: None,
            rating: None,
            group: None,
        }
    }

    fn blank_name(id: &str) -> BrowseTile {
        BrowseTile {
            id: id.into(),
            name: "   ".into(),
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
    fn lru_evicts_oldest_when_over_cap() {
        let mut c = BrowseTileCache {
            cap: 3,
            ..Default::default()
        };
        c.store_contiguous(0, vec![tile("a"), tile("b"), tile("c")]);
        assert_eq!(c.len(), 3);
        c.touch(0);
        c.store_contiguous(3, vec![tile("d")]);
        assert_eq!(c.len(), 3);
        assert!(c.peek(0).is_some(), "recently touched kept");
        assert!(c.peek(1).is_none() || c.peek(2).is_none());
    }

    #[test]
    fn viewport_health_reports_scattered_holes() {
        let mut c = BrowseTileCache::default();
        c.store_contiguous(2, vec![tile("x")]);
        let h = c.viewport_health(0, 4);
        assert_eq!(h.holes, 3);
        assert_eq!(h.filled, 1);
        assert_eq!(c.first_unfilled_run(0, 4), Some((0, 2)));
    }

    #[test]
    fn first_unfilled_run_is_contiguous() {
        let mut c = BrowseTileCache::default();
        c.store_contiguous(0, vec![tile("a")]);
        c.store_contiguous(3, vec![tile("d")]);
        assert_eq!(c.first_unfilled_run(0, 5), Some((1, 3)));
    }

    #[test]
    fn viewport_health_detects_blank_titles() {
        let mut c = BrowseTileCache::default();
        c.store_contiguous(0, vec![tile("ok"), blank_name("bad")]);
        let h = c.viewport_health(0, 3);
        assert_eq!(h.window_len, 3);
        assert_eq!(h.filled, 2);
        assert_eq!(h.holes, 1);
        assert_eq!(h.unpaintable, 1);
        assert!(!h.is_coherent());
        assert!((h.coverage() - 2.0 / 3.0).abs() < f32::EPSILON);
    }

    #[test]
    fn coherent_viewport_after_full_fill() {
        let mut c = BrowseTileCache::default();
        c.store_contiguous(0, vec![tile("a"), tile("b"), tile("c")]);
        let h = c.viewport_health(0, 3);
        assert!(h.is_coherent());
        assert_eq!(h.coverage(), 1.0);
    }

    #[test]
    fn paintable_rejects_empty_id() {
        let t = BrowseTile {
            id: String::new(),
            name: "Film".into(),
            art_url: None,
            category_id: None,
            source_id: None,
            year: None,
            genre: None,
            rating: None,
            group: None,
        };
        assert!(!t.is_paintable());
    }

    #[test]
    fn soft_ram_budgets_are_bounded() {
        // Cache ≪ full catalog; fetch covers a few screens without one giant alloc.
        assert!(BROWSE_TILE_CACHE_CAP <= 4_096);
        assert!(BROWSE_TILE_FETCH_LIMIT <= 512);
        assert!(BROWSE_TILE_FETCH_LIMIT < BROWSE_TILE_CACHE_CAP);
    }

    #[test]
    fn store_at_fills_sparse_favorites_without_holes_report() {
        let mut c = BrowseTileCache::default();
        c.store_at(vec![(0, tile("a")), (5, tile("f"))]);
        let h = c.viewport_health(0, 6);
        assert_eq!(h.holes, 4);
        assert_eq!(h.filled, 2);
        assert!(!h.is_coherent());
        // Filling the hole run restores coherence for that band.
        c.store_contiguous(1, vec![tile("b"), tile("c"), tile("d"), tile("e")]);
        assert!(c.viewport_health(0, 6).is_coherent());
    }
}
