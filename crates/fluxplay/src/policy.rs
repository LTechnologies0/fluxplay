//! Runtime policy pack — soft knobs that used to be scattered `const`s.
//!
//! **Hard ceilings** stay in [`crate::invariants`] / `const` (OOM / fuzz contracts).
//! This pack only supplies *effective* values, always clamped under those ceilings.
//!
//! Load order (later wins):
//! 1. compiled defaults (= previous hardcodes)
//! 2. optional `policy.json` in the config dir
//! 3. `FLUXPLAY_*` env overlays
//!
//! Write a starter file with [`ensure_default_file`] so power users can edit it.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Process-wide policy (set once at boot, readable anywhere).
static POLICY: OnceLock<PolicyPack> = OnceLock::new();
/// Mutable handle for tests / late reload (rare).
static POLICY_CELL: OnceLock<Mutex<PolicyPack>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyPack {
    pub version: u32,
    pub browse: BrowsePolicy,
    pub meta: MetaPolicy,
    pub images: ImagePolicy,
    pub skip: SkipPolicy,
    pub cache: CachePolicy,
    pub downloads: DownloadsPolicy,
}

impl Default for PolicyPack {
    fn default() -> Self {
        Self {
            version: 1,
            browse: BrowsePolicy::default(),
            meta: MetaPolicy::default(),
            images: ImagePolicy::default(),
            skip: SkipPolicy::default(),
            cache: CachePolicy::default(),
            downloads: DownloadsPolicy::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowsePolicy {
    /// Films / series page size (≤ [`crate::images::MAX_RAM_HANDLES`]).
    pub list_page: usize,
    /// Live / EPG page size.
    pub live_page: usize,
    /// Virtual-list overscan rows (≤ 8).
    pub virtual_overscan: usize,
    /// Mosaic min tile edge (px).
    pub mosaic_tile_min: f32,
}

impl Default for BrowsePolicy {
    fn default() -> Self {
        Self {
            list_page: crate::browser::LIST_PAGE,
            live_page: crate::browser::LIVE_PAGE,
            virtual_overscan: crate::browser::VIRTUAL_OVERSCAN,
            mosaic_tile_min: crate::theme::MOSAIC_TILE_MIN,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MetaPolicy {
    pub gate_tvmaze_ms: u64,
    pub gate_omdb_ms: u64,
    pub gate_tmdb_ms: u64,
    pub gate_itunes_ms: u64,
    pub gate_anilist_ms: u64,
    pub gate_wikipedia_ms: u64,
    /// Background enrich batch: series / vod caps per wave.
    pub enrich_series_take: usize,
    pub enrich_vod_take: usize,
    pub enrich_join_parallel_max: usize,
}

impl Default for MetaPolicy {
    fn default() -> Self {
        Self {
            gate_tvmaze_ms: 750,
            gate_omdb_ms: 280,
            gate_tmdb_ms: 250,
            gate_itunes_ms: 200,
            gate_anilist_ms: 700,
            gate_wikipedia_ms: 150,
            enrich_series_take: 12,
            enrich_vod_take: 24,
            enrich_join_parallel_max: 6,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ImagePolicy {
    pub max_inflight: usize,
    pub host_fail_trip: u32,
    pub soft_fail_secs: u64,
    pub host_cooldown_secs: u64,
}

impl Default for ImagePolicy {
    fn default() -> Self {
        Self {
            max_inflight: 24,
            host_fail_trip: 5,
            soft_fail_secs: 12 * 60,
            host_cooldown_secs: 25 * 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkipPolicy {
    pub tosd_cache_ttl_secs: u64,
    pub tosd_negative_ttl_secs: u64,
    /// Extra intro keywords (folded lowercase substrings). Merged with builtins.
    pub intro_extra: Vec<String>,
    pub credits_extra: Vec<String>,
    pub recap_extra: Vec<String>,
}

impl Default for SkipPolicy {
    fn default() -> Self {
        Self {
            tosd_cache_ttl_secs: 6 * 3600,
            tosd_negative_ttl_secs: 30 * 60,
            intro_extra: Vec::new(),
            credits_extra: Vec::new(),
            recap_extra: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CachePolicy {
    /// Multiplier on portal API cache TTLs (1.0 = compiled defaults).
    pub portal_ttl_scale: f32,
    pub portal_concurrency: usize,
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self {
            portal_ttl_scale: 1.0,
            portal_concurrency: 2,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadsPolicy {
    pub default_connections: usize,
    pub max_connections: usize,
    pub stall_timeout_secs: u64,
}

impl Default for DownloadsPolicy {
    fn default() -> Self {
        Self {
            default_connections: 2,
            max_connections: 8,
            stall_timeout_secs: 45,
        }
    }
}

impl PolicyPack {
    /// Clamp every field under compile-time hard ceilings.
    pub fn sanitize(mut self) -> Self {
        use crate::images::MAX_RAM_HANDLES;
        self.browse.list_page = self.browse.list_page.clamp(8, MAX_RAM_HANDLES);
        self.browse.live_page = self.browse.live_page.clamp(24, 512);
        self.browse.virtual_overscan = self.browse.virtual_overscan.clamp(2, 8);
        self.browse.mosaic_tile_min = self.browse.mosaic_tile_min.clamp(80.0, 240.0);

        self.meta.gate_tvmaze_ms = self.meta.gate_tvmaze_ms.clamp(50, 5_000);
        self.meta.gate_omdb_ms = self.meta.gate_omdb_ms.clamp(50, 5_000);
        self.meta.gate_tmdb_ms = self.meta.gate_tmdb_ms.clamp(50, 5_000);
        self.meta.gate_itunes_ms = self.meta.gate_itunes_ms.clamp(50, 5_000);
        self.meta.gate_anilist_ms = self.meta.gate_anilist_ms.clamp(50, 5_000);
        self.meta.gate_wikipedia_ms = self.meta.gate_wikipedia_ms.clamp(50, 5_000);
        self.meta.enrich_series_take = self.meta.enrich_series_take.clamp(1, 64);
        self.meta.enrich_vod_take = self.meta.enrich_vod_take.clamp(1, 96);
        self.meta.enrich_join_parallel_max = self.meta.enrich_join_parallel_max.clamp(1, 8);

        self.images.max_inflight = self.images.max_inflight.clamp(4, 48);
        self.images.host_fail_trip = self.images.host_fail_trip.clamp(2, 20);
        self.images.soft_fail_secs = self.images.soft_fail_secs.clamp(60, 24 * 3600);
        self.images.host_cooldown_secs = self.images.host_cooldown_secs.clamp(60, 24 * 3600);

        self.skip.tosd_cache_ttl_secs = self.skip.tosd_cache_ttl_secs.clamp(300, 7 * 24 * 3600);
        self.skip.tosd_negative_ttl_secs = self.skip.tosd_negative_ttl_secs.clamp(60, 24 * 3600);

        self.cache.portal_ttl_scale = self.cache.portal_ttl_scale.clamp(0.25, 4.0);
        self.cache.portal_concurrency = self.cache.portal_concurrency.clamp(1, 4);

        self.downloads.default_connections = self.downloads.default_connections.clamp(1, 8);
        self.downloads.max_connections = self.downloads.max_connections.clamp(1, 16);
        // Never raise a deliberately low ceiling (e.g. policy max=1) back to default.
        if self.downloads.default_connections > self.downloads.max_connections {
            self.downloads.default_connections = self.downloads.max_connections;
        }
        self.downloads.stall_timeout_secs = self.downloads.stall_timeout_secs.clamp(10, 300);
        self
    }

    fn apply_env(mut self) -> Self {
        if let Some(v) = env_usize("FLUXPLAY_LIST_PAGE") {
            self.browse.list_page = v;
        }
        if let Some(v) = env_usize("FLUXPLAY_LIVE_PAGE") {
            self.browse.live_page = v;
        }
        if let Some(v) = env_usize("FLUXPLAY_VIRTUAL_OVERSCAN") {
            self.browse.virtual_overscan = v;
        }
        if let Some(v) = env_usize("FLUXPLAY_IMAGE_INFLIGHT") {
            self.images.max_inflight = v;
        }
        if let Some(v) = env_usize("FLUXPLAY_META_PARALLEL") {
            self.meta.enrich_join_parallel_max = v;
        }
        if let Some(v) = env_usize("FLUXPLAY_PORTAL_CONCURRENCY") {
            self.cache.portal_concurrency = v;
        }
        if let Some(v) = env_f32("FLUXPLAY_CACHE_TTL_SCALE") {
            self.cache.portal_ttl_scale = v;
        }
        if let Some(v) = env_usize("FLUXPLAY_ENRICH_SERIES") {
            self.meta.enrich_series_take = v;
        }
        if let Some(v) = env_usize("FLUXPLAY_ENRICH_VOD") {
            self.meta.enrich_vod_take = v;
        }
        if let Some(v) = env_usize("FLUXPLAY_DL_CONNECTIONS") {
            self.downloads.default_connections = v;
        }
        if let Some(v) = env_f32("FLUXPLAY_MOSAIC_TILE_MIN") {
            self.browse.mosaic_tile_min = v;
        }
        self
    }
}

fn env_usize(key: &str) -> Option<usize> {
    std::env::var(key).ok()?.parse().ok()
}

fn env_f32(key: &str) -> Option<f32> {
    std::env::var(key).ok()?.parse().ok()
}

pub fn path() -> PathBuf {
    crate::storage::config_dir().join("policy.json")
}

/// Load defaults → file → env, install globally, push skip dicts into the player.
pub fn init() -> &'static PolicyPack {
    POLICY.get_or_init(|| {
        let mut pack = PolicyPack::default();
        let p = path();
        if p.is_file() {
            match load_file(&p) {
                Ok(file) => {
                    info!(path = %p.display(), "policy.json loaded");
                    pack = file;
                }
                Err(e) => warn!(path = %p.display(), error = %e, "policy.json ignored"),
            }
        }
        let pack = pack.apply_env().sanitize();
        install_side_effects(&pack);
        let _ = POLICY_CELL.set(Mutex::new(pack.clone()));
        debug!(
            list = pack.browse.list_page,
            live = pack.browse.live_page,
            inflight = pack.images.max_inflight,
            "policy ready"
        );
        pack
    })
}

fn install_side_effects(pack: &PolicyPack) {
    // Chapter keyword extras → player crate.
    fluxplay_player::set_chapter_keyword_extras(
        pack.skip.intro_extra.clone(),
        pack.skip.credits_extra.clone(),
        pack.skip.recap_extra.clone(),
    );
    // Portal concurrency / TTL scale → providers.
    fluxplay_providers::configure_runtime_policy(
        pack.cache.portal_concurrency,
        pack.cache.portal_ttl_scale,
    );
}

pub fn get() -> &'static PolicyPack {
    POLICY.get().unwrap_or_else(|| init())
}

pub fn load_file(path: &Path) -> Result<PolicyPack, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let pack: PolicyPack = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    Ok(pack.sanitize())
}

pub fn save_file(path: &Path, pack: &PolicyPack) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let raw = serde_json::to_string_pretty(pack).map_err(|e| e.to_string())?;
    crate::storage::write_private(path, raw.as_bytes()).map_err(|e| e.to_string())
}

/// Create `policy.json` with defaults if missing (discoverable for power users).
pub fn ensure_default_file() {
    let p = path();
    if p.is_file() {
        return;
    }
    let pack = PolicyPack::default().sanitize();
    match save_file(&p, &pack) {
        Ok(()) => info!(path = %p.display(), "wrote default policy.json"),
        Err(e) => warn!(path = %p.display(), error = %e, "could not write policy.json"),
    }
}

/// Effective browse page for the current tab kind.
pub fn list_page() -> usize {
    get().browse.list_page
}

pub fn live_page() -> usize {
    get().browse.live_page
}

pub fn virtual_overscan() -> usize {
    get().browse.virtual_overscan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_legacy_budgets() {
        let p = PolicyPack::default().sanitize();
        assert_eq!(p.browse.list_page, 24);
        assert_eq!(p.browse.live_page, 96);
        assert_eq!(p.browse.virtual_overscan, 4);
        assert_eq!(p.images.max_inflight, 24);
        assert_eq!(p.meta.gate_tmdb_ms, 250);
    }

    #[test]
    fn sanitize_clamps_absurd() {
        let mut p = PolicyPack::default();
        p.browse.list_page = 9999;
        p.browse.virtual_overscan = 100;
        p.images.max_inflight = 1;
        p = p.sanitize();
        assert!(p.browse.list_page <= crate::images::MAX_RAM_HANDLES);
        assert!(p.browse.virtual_overscan <= 8);
        assert!(p.images.max_inflight >= 4);
    }

    #[test]
    fn sanitize_keeps_single_connection_ceiling() {
        let mut p = PolicyPack::default();
        p.downloads.default_connections = 2;
        p.downloads.max_connections = 1;
        p = p.sanitize();
        assert_eq!(p.downloads.max_connections, 1);
        assert_eq!(p.downloads.default_connections, 1);
    }

    #[test]
    fn roundtrip_json() {
        let p = PolicyPack::default().sanitize();
        let j = serde_json::to_string(&p).unwrap();
        let back: PolicyPack = serde_json::from_str(&j).unwrap();
        assert_eq!(back.browse.list_page, p.browse.list_page);
    }
}
