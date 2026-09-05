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

/// Max simultaneous portal HTTP calls (auth + catalog + EPG share this).
/// 2 unlocks real overlap for `tokio::join!` dumps while staying panel-friendly.
const PORTAL_CONCURRENCY: usize = 2;

static HTTP: OnceLock<reqwest::Client> = OnceLock::new();
static PORTAL_SEM: OnceLock<Semaphore> = OnceLock::new();

pub fn shared_http() -> Result<reqwest::Client> {
    if let Some(c) = HTTP.get() {
        return Ok(c.clone());
    }
    let built = reqwest::Client::builder()
        .user_agent(crate::xtream::SMARTERS_UA)
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::limited(8))
        .gzip(true)
        .pool_max_idle_per_host(4)
        .build();
    match built {
        Ok(client) => {
            debug!("shared HTTP client initialized");
            Ok(HTTP.get_or_init(|| client).clone())
        }
        Err(e) => {
            error!(error = %e, "HTTP client build failed");
            Err(ProviderError::Message(format!(
                "http client build failed: {e}"
            )))
        }
    }
}

fn portal_sem() -> &'static Semaphore {
    PORTAL_SEM.get_or_init(|| {
        debug!(slots = PORTAL_CONCURRENCY, "portal semaphore created");
        Semaphore::new(PORTAL_CONCURRENCY)
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
    let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
    base.join("fluxplay").join("xtream-api")
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
    match action {
        None => Duration::from_secs(2 * 3600), // auth
        Some("get_live_categories")
        | Some("get_vod_categories")
        | Some("get_series_categories") => Duration::from_secs(24 * 3600),
        Some("get_live_streams") => Duration::from_secs(12 * 3600),
        Some("get_vod_streams") | Some("get_series") => Duration::from_secs(12 * 3600),
        Some("get_series_info") => Duration::from_secs(24 * 3600),
        Some("get_short_epg") | Some("get_simple_data_table") => Duration::from_secs(45 * 60),
        _ => Duration::from_secs(6 * 3600),
    }
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

pub fn put(key: &str, value: &Value) {
    let root = cache_root();
    if let Err(e) = std::fs::create_dir_all(&root) {
        warn!(error = %e, "xtream cache mkdir failed");
        return;
    }
    let path = path_for(key);
    let meta = meta_path(key);
    match serde_json::to_vec(value) {
        Ok(bytes) => {
            if let Err(e) = std::fs::write(&path, bytes) {
                warn!(error = %e, "xtream cache write failed");
                return;
            }
            let _ = std::fs::write(&meta, now_secs().to_string());
            trace!(%key, "xtream cache put");
        }
        Err(e) => warn!(error = %e, "xtream cache serialize failed"),
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
