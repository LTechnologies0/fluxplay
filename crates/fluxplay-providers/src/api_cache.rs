//! Disk-backed cache + shared HTTP client for Xtream `player_api` calls.
//! Cuts 429 / "too many requests" by serving stale JSON and limiting concurrency.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fluxplay_core::Stopwatch;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tracing::{debug, error, trace, warn};

use crate::{ProviderError, Result};

pub use crate::http_client::{
    app_http, apply_network_settings, current_network_settings, probe_dns, set_socks_proxy,
    shared_http, socks_proxy,
};

/// Max simultaneous portal HTTP calls (auth + catalog + EPG share this).
/// 2 unlocks real overlap for `tokio::join!` dumps while staying panel-friendly.
const PORTAL_CONCURRENCY_DEFAULT: usize = 2;

static PORTAL_SLOTS: OnceLock<usize> = OnceLock::new();
static PORTAL_TTL_SCALE: OnceLock<f32> = OnceLock::new();
static PORTAL_SEM: OnceLock<Semaphore> = OnceLock::new();

/// Soft knobs from the app [`PolicyPack`] (safe to call before first portal request).
pub fn configure_runtime_policy(portal_concurrency: usize, ttl_scale: f32) {
    let slots = portal_concurrency.clamp(1, 4);
    let scale = ttl_scale.clamp(0.25, 4.0);
    let _ = PORTAL_SLOTS.set(slots);
    let _ = PORTAL_TTL_SCALE.set(scale);
    debug!(slots, ttl_scale = scale, "portal runtime policy");
}

fn portal_sem() -> &'static Semaphore {
    PORTAL_SEM.get_or_init(|| {
        let slots = PORTAL_SLOTS
            .get()
            .copied()
            .unwrap_or(PORTAL_CONCURRENCY_DEFAULT);
        debug!(slots, "portal semaphore created");
        Semaphore::new(slots)
    })
}

pub async fn with_portal_limit<T, F, Fut>(f: F) -> Result<T>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let _prof = Stopwatch::start("portal_limit");
    trace!("acquiring portal semaphore");
    let _permit = portal_sem()
        .acquire()
        .await
        .map_err(|_| {
            error!("portal semaphore closed");
            ProviderError::Message("portal semaphore closed".into())
        })?;
    trace!("portal semaphore acquired");
    f().await
}

fn cache_root() -> PathBuf {
    if let Some(over) = CACHE_ROOT_OVERRIDE.get() {
        return over.clone();
    }
    let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
    base.join("fluxplay").join("xtream-api")
}

static CACHE_ROOT_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Point Xtream disk cache at app-private storage (Android `internal_data`).
pub fn set_cache_root(path: PathBuf) {
    let _ = CACHE_ROOT_OVERRIDE.set(path);
}

fn hash_key(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0u8]);
    }
    hex::encode(h.finalize())
}

/// Stable cache key for a portal user + action (+ optional extras like category_id).
pub fn key(portal: &str, user: &str, action: &str, extras: &[&str]) -> String {
    let mut parts = vec![portal, user, action];
    parts.extend_from_slice(extras);
    let k = hash_key(&parts);
    trace!(%action, extras = extras.len(), key_prefix = &k[..8.min(k.len())], "cache key");
    k
}

pub fn ttl_for_action(action: Option<&str>) -> Duration {
    let base = match action {
        None => Duration::from_secs(2 * 3600), // auth
        Some("get_live_categories")
        | Some("get_vod_categories")
        | Some("get_series_categories") => Duration::from_secs(24 * 3600),
        Some("get_live_streams") => Duration::from_secs(12 * 3600),
        Some("get_vod_streams") | Some("get_series") => Duration::from_secs(12 * 3600),
        Some("get_series_info") | Some("get_vod_info") => Duration::from_secs(24 * 3600),
        Some("get_short_epg") | Some("get_simple_data_table") => Duration::from_secs(45 * 60),
        _ => Duration::from_secs(6 * 3600),
    };
    let scale = PORTAL_TTL_SCALE.get().copied().unwrap_or(1.0);
    if (scale - 1.0).abs() < f32::EPSILON {
        return base;
    }
    let millis = (base.as_millis() as f32 * scale).round().max(1.0) as u64;
    Duration::from_millis(millis)
}

fn path_for(key: &str) -> PathBuf {
    cache_root().join(format!("{key}.json"))
}

fn meta_path(key: &str) -> PathBuf {
    cache_root().join(format!("{key}.meta"))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Read cached JSON if younger than `ttl`. If expired, returns `None` (caller may still use stale).
pub fn get_fresh(key: &str, ttl: Duration) -> Option<Value> {
    let (value, age) = read_entry(key)?;
    if age <= ttl.as_secs() {
        debug!(%key, age_secs = age, "xtream cache hit");
        Some(value)
    } else {
        debug!(%key, age_secs = age, ttl_secs = ttl.as_secs(), "xtream cache expired");
        None
    }
}

/// Stale-but-present entry (for 429 fallback).
pub fn get_stale(key: &str) -> Option<Value> {
    let entry = read_entry(key).map(|(v, age)| {
        debug!(%key, age_secs = age, "xtream stale cache available");
        v
    });
    if entry.is_none() {
        trace!(%key, "xtream stale cache miss");
    }
    entry
}

fn read_entry(key: &str) -> Option<(Value, u64)> {
    let path = path_for(key);
    let meta = meta_path(key);
    let stored = std::fs::read_to_string(&meta).ok()?.parse::<u64>().ok()?;
    let age = now_secs().saturating_sub(stored);
    let bytes = std::fs::read(&path).ok()?;
    let value = serde_json::from_slice(&bytes).ok()?;
    Some((value, age))
}

/// Skip caching blobs larger than this (still usable in-memory once).
const MAX_ENTRY_BYTES: usize = 16 * 1024 * 1024;
/// Soft budget for the whole xtream-api cache directory.
const MAX_CACHE_DIR_BYTES: u64 = 256 * 1024 * 1024;

fn dir_usage_bytes(root: &std::path::Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(root) else {
        return 0;
    };
    rd.filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// Drop oldest `.json`/`.meta` pairs until under budget (mtime LRU).
fn enforce_cache_budget(root: &std::path::Path) {
    let mut usage = dir_usage_bytes(root);
    if usage <= MAX_CACHE_DIR_BYTES {
        return;
    }
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    let mut entries: Vec<(SystemTime, PathBuf, u64)> = rd
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x == "json")
        })
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let mtime = meta.modified().ok()?;
            Some((mtime, e.path(), meta.len()))
        })
        .collect();
    entries.sort_by_key(|(mtime, _, _)| *mtime);
    for (_mtime, path, len) in entries {
        if usage <= MAX_CACHE_DIR_BYTES {
            break;
        }
        let meta = path.with_extension("meta");
        let meta_len = std::fs::metadata(&meta).map(|m| m.len()).unwrap_or(0);
        if std::fs::remove_file(&path).is_ok() {
            usage = usage.saturating_sub(len);
        }
        if std::fs::remove_file(&meta).is_ok() {
            usage = usage.saturating_sub(meta_len);
        }
        warn!(?path, "xtream cache LRU eviction");
    }
}

pub fn put(key: &str, value: &Value) {
    let root = cache_root();
    if let Err(e) = std::fs::create_dir_all(&root) {
        warn!(error = %e, "xtream cache mkdir failed");
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700));
    }
    let path = path_for(key);
    let meta = meta_path(key);
    match serde_json::to_vec(value) {
        Ok(bytes) => {
            if bytes.len() > MAX_ENTRY_BYTES {
                warn!(
                    %key,
                    bytes = bytes.len(),
                    cap = MAX_ENTRY_BYTES,
                    "xtream cache skip oversize entry"
                );
                return;
            }
            if let Err(e) = std::fs::write(&path, &bytes) {
                warn!(error = %e, "xtream cache write failed");
                return;
            }
            let _ = std::fs::write(&meta, now_secs().to_string());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
                let _ = std::fs::set_permissions(&meta, std::fs::Permissions::from_mode(0o600));
            }
            enforce_cache_budget(&root);
            trace!(%key, "xtream cache put");
        }
        Err(e) => warn!(error = %e, "xtream cache serialize failed"),
    }
}

/// Drop a cached portal answer so the next call hits the network (e.g. empty seasons).
pub fn forget(key: &str) {
    let _ = std::fs::remove_file(path_for(key));
    let _ = std::fs::remove_file(meta_path(key));
    debug!(%key, "xtream cache forgotten");
}

/// True when `get_series_info` JSON carries at least one episode entry.
pub fn series_info_has_episodes(value: &Value) -> bool {
    match value.get("episodes") {
        Some(Value::Object(map)) => map.values().any(|eps| match eps {
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
            _ => false,
        }),
        Some(Value::Array(arr)) => !arr.is_empty(),
        _ => false,
    }
}

/// After HTTP 429, keep the portal quiet so the next call does not extend the ban.
static PORTAL_QUIET_UNTIL_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Remember that the portal refused a call. Background calls must not hit it yet.
pub fn note_portal_backoff() {
    let until = now_ms().saturating_add(30_000);
    let _ = PORTAL_QUIET_UNTIL_MS.try_update(
        std::sync::atomic::Ordering::Relaxed,
        std::sync::atomic::Ordering::Relaxed,
        |cur| Some(cur.max(until)),
    );
    warn!(quiet_secs = 30, "portal backoff");
}

pub fn portal_quiet_remaining() -> Duration {
    let until = PORTAL_QUIET_UNTIL_MS.load(std::sync::atomic::Ordering::Relaxed);
    let now = now_ms();
    if until > now {
        Duration::from_millis(until - now)
    } else {
        Duration::ZERO
    }
}

/// Block until the quiet window opened by a 429 has elapsed.
pub async fn wait_portal_quiet() {
    let wait = portal_quiet_remaining();
    if !wait.is_zero() {
        debug!(wait_ms = wait.as_millis() as u64, "waiting out portal backoff");
        tokio::time::sleep(wait).await;
    }
}

/// Drop all cached Xtream API responses (manual reload).
pub fn clear_all() {
    let _prof = Stopwatch::start("xtream_cache_clear");
    let root = cache_root();
    if root.is_dir() {
        match std::fs::remove_dir_all(&root) {
            Ok(()) => debug!(path = %root.display(), "xtream API cache cleared"),
            Err(e) => warn!(error = %e, path = %root.display(), "xtream cache clear failed"),
        }
    } else {
        debug!("xtream API cache already empty");
    }
}
