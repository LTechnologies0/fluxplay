//! [The Open Skip Database](https://github.com/The-Open-Skip-Database/db) client.
//!
//! Community timestamps for series intros / outros and movie end-credits.
//! Identity is **TMDB id** (same as FluxPlay's enrich path).
//!
//! Endpoints (public GET, no key):
//! - `GET {base}/api/series/{tmdb}?season=&episode=` → intro_start/end, outro_start
//! - `GET {base}/api/movie/{tmdb}` → outro_start
//!
//! Override base with `FLUXPLAY_TOSD_URL` (default `https://tosd.qtchaos.de`).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tracing::{debug, info, warn};

const DEFAULT_BASE: &str = "https://tosd.qtchaos.de";

fn cache_ttl() -> Duration {
    Duration::from_secs(crate::policy::get().skip.tosd_cache_ttl_secs)
}

fn negative_ttl() -> Duration {
    Duration::from_secs(crate::policy::get().skip.tosd_negative_ttl_secs)
}

/// One skippable window from TOSD (seconds, float for the player seek path).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkipHit {
    pub intro: Option<(f64, f64)>,
    pub outro_start: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct SeriesBody {
    intro_start: Option<f64>,
    intro_end: Option<f64>,
    outro_start: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct MovieBody {
    outro_start: Option<f64>,
}

#[derive(Clone)]
enum CacheEntry {
    Hit { at: Instant, value: SkipHit },
    Miss { at: Instant },
}

fn cache() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static C: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn base_url() -> String {
    std::env::var("FLUXPLAY_TOSD_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.into())
}

fn cache_get(key: &str) -> Option<Option<SkipHit>> {
    let guard = cache().lock().ok()?;
    match guard.get(key)? {
        CacheEntry::Hit { at, value } if at.elapsed() < cache_ttl() => Some(Some(*value)),
        CacheEntry::Miss { at } if at.elapsed() < negative_ttl() => Some(None),
        _ => None,
    }
}

fn cache_put(key: String, value: Option<SkipHit>) {
    let Ok(mut guard) = cache().lock() else {
        return;
    };
    let entry = match value {
        Some(v) => CacheEntry::Hit {
            at: Instant::now(),
            value: v,
        },
        None => CacheEntry::Miss {
            at: Instant::now(),
        },
    };
    guard.insert(key, entry);
    if guard.len() > 512 {
        let hit_ttl = cache_ttl();
        let miss_ttl = negative_ttl();
        guard.retain(|_, e| match e {
            CacheEntry::Hit { at, .. } => at.elapsed() < hit_ttl,
            CacheEntry::Miss { at } => at.elapsed() < miss_ttl,
        });
    }
}

fn series_key(tmdb: u32, season: u32, episode: u32) -> String {
    format!("s:{tmdb}:{season}:{episode}")
}

fn movie_key(tmdb: u32) -> String {
    format!("m:{tmdb}")
}

fn normalize_intro(start: Option<f64>, end: Option<f64>) -> Option<(f64, f64)> {
    let (s, e) = (start?, end?);
    if !s.is_finite() || !e.is_finite() || e <= s + 1.5 || s < 0.0 {
        return None;
    }
    // Guard absurd community submissions (whole feature marked as intro).
    if e - s > 240.0 {
        return None;
    }
    Some((s, e))
}

fn normalize_outro(start: Option<f64>) -> Option<f64> {
    let s = start?;
    if !s.is_finite() || s < 30.0 {
        return None;
    }
    Some(s)
}

/// Parse a TOSD series JSON body without network I/O (fuzz / property surface).
pub fn parse_series_json(raw: &str) -> Option<SkipHit> {
    let body: SeriesBody = serde_json::from_str(raw).ok()?;
    let hit = SkipHit {
        intro: normalize_intro(body.intro_start, body.intro_end),
        outro_start: normalize_outro(body.outro_start),
    };
    if hit.intro.is_none() && hit.outro_start.is_none() {
        return None;
    }
    Some(hit)
}

/// Parse a TOSD movie JSON body without network I/O (fuzz / property surface).
pub fn parse_movie_json(raw: &str) -> Option<SkipHit> {
    let body: MovieBody = serde_json::from_str(raw).ok()?;
    let hit = SkipHit {
        intro: None,
        outro_start: normalize_outro(body.outro_start),
    };
    if hit.outro_start.is_none() {
        return None;
    }
    Some(hit)
}

/// Lookup series intro/outro. `None` = network/404/invalid (caller keeps chapter/picture).
pub async fn lookup_series(tmdb_id: u32, season: u32, episode: u32) -> Option<SkipHit> {
    if tmdb_id == 0 || season == 0 || episode == 0 {
        return None;
    }
    let key = series_key(tmdb_id, season, episode);
    if let Some(cached) = cache_get(&key) {
        return cached;
    }
    let url = format!(
        "{}/api/series/{tmdb_id}?season={season}&episode={episode}",
        base_url()
    );
    let hit = fetch_series(&url).await;
    cache_put(key, hit);
    hit
}

/// Lookup movie end-credits start.
pub async fn lookup_movie(tmdb_id: u32) -> Option<SkipHit> {
    if tmdb_id == 0 {
        return None;
    }
    let key = movie_key(tmdb_id);
    if let Some(cached) = cache_get(&key) {
        return cached;
    }
    let url = format!("{}/api/movie/{tmdb_id}", base_url());
    let hit = fetch_movie(&url).await;
    cache_put(key, hit);
    hit
}

fn http() -> reqwest::Client {
    fluxplay_providers::app_http("FluxPlay/0.3 (+tosd; The Open Skip Database)", 10).unwrap_or_else(
        |_| {
            fluxplay_providers::emergency_http("FluxPlay/0.3 (+tosd; The Open Skip Database)", 10)
        },
    )
}

async fn fetch_series(url: &str) -> Option<SkipHit> {
    let resp = match http()
        .get(url)
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %fluxplay_providers::redact_error(&e), "tosd series fetch");
            return None;
        }
    };
    let status = resp.status();
    if status.as_u16() == 404 {
        debug!(%url, "tosd series miss");
        return None;
    }
    if !status.is_success() {
        warn!(%status, "tosd series http");
        return None;
    }
    let text = match resp.text().await {
        Ok(t) => t,
        Err(e) => {
            warn!(error = %e, "tosd series body");
            return None;
        }
    };
    let hit = match parse_series_json(&text) {
        Some(h) => h,
        None => {
            warn!("tosd series json");
            return None;
        }
    };
    info!(
        intro = ?hit.intro,
        outro = ?hit.outro_start,
        "tosd series hit"
    );
    Some(hit)
}

async fn fetch_movie(url: &str) -> Option<SkipHit> {
    let resp = match http()
        .get(url)
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %fluxplay_providers::redact_error(&e), "tosd movie fetch");
            return None;
        }
    };
    let status = resp.status();
    if status.as_u16() == 404 {
        debug!(%url, "tosd movie miss");
        return None;
    }
    if !status.is_success() {
        warn!(%status, "tosd movie http");
        return None;
    }
    let text = match resp.text().await {
        Ok(t) => t,
        Err(e) => {
            warn!(error = %e, "tosd movie body");
            return None;
        }
    };
    let hit = match parse_movie_json(&text) {
        Some(h) => h,
        None => {
            warn!("tosd movie json");
            return None;
        }
    };
    info!(outro = ?hit.outro_start, "tosd movie hit");
    Some(hit)
}

/// Resolve TMDB id from an IMDb id when the catalog only has `tt…`.
pub async fn tmdb_id_from_imdb(imdb_id: &str, series: bool) -> Option<u32> {
    let tt = imdb_id.trim();
    if !tt.starts_with("tt") {
        return None;
    }
    // Reuse the same TMDB finder the enrich path uses.
    crate::metadata::tmdb_id_for_imdb(tt, series).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_intro_rejects_junk() {
        assert!(normalize_intro(Some(10.0), Some(70.0)).is_some());
        assert!(normalize_intro(Some(10.0), Some(10.5)).is_none());
        assert!(normalize_intro(Some(-1.0), Some(20.0)).is_none());
        assert!(normalize_intro(Some(0.0), Some(400.0)).is_none());
    }

    #[test]
    fn normalize_outro_floor() {
        assert_eq!(normalize_outro(Some(120.0)), Some(120.0));
        assert!(normalize_outro(Some(5.0)).is_none());
    }

    #[test]
    fn cache_keys_stable() {
        assert_eq!(series_key(1396, 1, 1), "s:1396:1:1");
        assert_eq!(movie_key(550), "m:550");
    }
}
