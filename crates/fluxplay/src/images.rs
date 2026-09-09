//! Logo / poster / banner cache — memory LRU + disk persistence (per-profile).
//!
//! Resilience for huge IPTV catalogs:
//! - per-URL soft-fail TTL (no permanent blacklist on transient errors)
//! - per-host circuit breaker (dead CDNs stop flooding retries/logs)
//! - SOCKS (WireGuard) → clearnet fallback for image GETs only
//! - proper percent-encoding (Wikimedia / unicode paths → no more HTTP 400)

use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use iced::widget::image::Handle;
use image::GenericImageView;
use sha2::{Digest, Sha256};
use tracing::{debug, error, trace, warn};
use url::Url;
use uuid::Uuid;

/// Cap RAM handles — keep several mosaic viewports warm during fast flings.
const MAX_RAM_HANDLES: usize = 768;
const MAX_IMAGE_BYTES: usize = 2_500_000;
/// Default soft cap when host caps are not applied yet.
pub const MAX_INFLIGHT_IMAGES: usize = 24;
/// Longest edge for GPU/RAM handles (overridden by host decode_edge_px).
const MAX_UI_EDGE_PX: u32 = 480;

const URL_SOFT_FAIL: Duration = Duration::from_secs(12 * 60);
const HOST_COOLDOWN: Duration = Duration::from_secs(25 * 60);
const HOST_FAIL_TRIP: u32 = 5;
const HOST_WARN_INTERVAL: Duration = Duration::from_secs(90);

#[derive(Debug, Clone)]
struct HostCircuit {
    fails: u32,
    cool_until: Option<Instant>,
}

#[derive(Debug)]
pub struct ImageCache {
    handles: HashMap<String, Handle>,
    /// Recency stamp — promote is O(1); eviction scans only when over cap.
    touch_gen: HashMap<String, u64>,
    clock: u64,
    inflight: HashSet<String>,
    /// URL → profile that requested the fetch (for disk path).
    inflight_source: HashMap<String, Option<Uuid>>,
    /// Permanent fail (HTTP 4xx / garbage body).
    failed: HashSet<String>,
    /// Transient fail — skip until Instant (connect / DNS / SOCKS).
    soft_fail: HashMap<String, Instant>,
    /// Per-host circuit: trip after repeated connect failures.
    host_circuit: HashMap<String, HostCircuit>,
    /// Host-tuned parallel fetch cap.
    pub max_inflight: usize,
    /// Host-tuned decode longest edge.
    pub decode_edge_px: u32,
}

impl Default for ImageCache {
    fn default() -> Self {
        Self {
            handles: HashMap::new(),
            touch_gen: HashMap::new(),
            clock: 0,
            inflight: HashSet::new(),
            inflight_source: HashMap::new(),
            failed: HashSet::new(),
            soft_fail: HashMap::new(),
            host_circuit: HashMap::new(),
            max_inflight: MAX_INFLIGHT_IMAGES,
            decode_edge_px: MAX_UI_EDGE_PX,
        }
    }
}

impl ImageCache {
    pub fn get(&self, url: &str) -> Option<&Handle> {
        let h = self.handles.get(url);
        if h.is_some() {
            trace!(target: "fluxplay::images", %url, "cache hit");
        }
        h
    }

    pub fn inflight_len(&self) -> usize {
        self.inflight.len()
    }

    /// O(1) MRU bump — safe to call for every visible mosaic tile each frame.
    pub fn promote(&mut self, url: &str) {
        if !self.handles.contains_key(url) {
            return;
        }
        self.clock = self.clock.wrapping_add(1);
        self.touch_gen.insert(url.to_string(), self.clock);
    }

    fn evict_oldest(&mut self) {
        while self.handles.len() >= MAX_RAM_HANDLES {
            let victim = self
                .touch_gen
                .iter()
                .min_by_key(|(_, g)| *g)
                .map(|(u, _)| u.clone());
            let Some(old) = victim else {
                break;
            };
            self.handles.remove(&old);
            self.touch_gen.remove(&old);
        }
    }

    fn host_blocked(&self, url: &str) -> bool {
        let Some(host) = host_key(url) else {
            return false;
        };
        self.host_circuit
            .get(&host)
            .and_then(|c| c.cool_until)
            .is_some_and(|until| Instant::now() < until)
    }

    fn url_soft_blocked(&mut self, url: &str) -> bool {
        let now = Instant::now();
        if let Some(until) = self.soft_fail.get(url).copied() {
            if now < until {
                return true;
            }
            self.soft_fail.remove(url);
        }
        false
    }

    pub fn request(&mut self, url: &str, source_id: Option<Uuid>) -> RequestOutcome {
        let url_n = normalize_image_url(url);
        if url_n.is_empty() || !is_fetchable_image_url(&url_n) || self.failed.contains(&url_n) {
            return RequestOutcome::Skip;
        }
        if self.url_soft_blocked(&url_n) || self.host_blocked(&url_n) {
            return RequestOutcome::Skip;
        }
        if self.handles.contains_key(&url_n) {
            self.promote(&url_n);
            return RequestOutcome::Ready;
        }
        if self.inflight.contains(&url_n) {
            return RequestOutcome::Pending;
        }
        if self.inflight.len() >= self.max_inflight {
            return RequestOutcome::Skip;
        }
        trace!(url = %url_n, ?source_id, "image fetch enqueue");
        self.inflight.insert(url_n.clone());
        self.inflight_source.insert(url_n.clone(), source_id);
        RequestOutcome::Fetch {
            url: url_n,
            source_id,
        }
    }

    pub fn take_inflight_source(&mut self, url: &str) -> Option<Uuid> {
        self.inflight_source.remove(url).flatten()
    }

    pub fn insert_bytes(&mut self, url: String, bytes: Vec<u8>) {
        self.inflight.remove(&url);
        self.inflight_source.remove(&url);
        self.soft_fail.remove(&url);
        if bytes.len() < 64 || looks_like_html(&bytes) {
            self.failed.insert(url);
            return;
        }
        let bytes = downscale_for_ui(bytes, self.decode_edge_px);
        self.insert_ui_bytes(url, bytes);
    }

    /// Insert bytes already prepared for UI (decoded/downscaled off-thread).
    pub fn insert_ui_bytes(&mut self, url: String, bytes: Vec<u8>) {
        self.inflight.remove(&url);
        self.inflight_source.remove(&url);
        self.soft_fail.remove(&url);
        if let Some(host) = host_key(&url) {
            // Success resets the circuit so a flaky host can recover.
            self.host_circuit.remove(&host);
        }
        if bytes.len() < 64 || looks_like_html(&bytes) {
            warn!(target: "fluxplay::images", %url, len = bytes.len(), "reject non-image bytes");
            self.failed.insert(url);
            return;
        }
        debug!(target: "fluxplay::images", %url, len = bytes.len(), "insert handle");
        if self.handles.contains_key(&url) {
            self.handles.insert(url.clone(), Handle::from_bytes(bytes));
            self.promote(&url);
            return;
        }
        self.evict_oldest();
        self.clock = self.clock.wrapping_add(1);
        self.touch_gen.insert(url.clone(), self.clock);
        self.handles.insert(url, Handle::from_bytes(bytes));
    }

    pub fn mark_failed(&mut self, url: String) {
        debug!(%url, "image mark failed (permanent)");
        self.inflight.remove(&url);
        self.inflight_source.remove(&url);
        self.soft_fail.remove(&url);
        self.failed.insert(url);
    }

    /// Transient network error: cool the URL + bump host circuit (no retry storm).
    pub fn mark_soft_failed(&mut self, url: String) {
        self.inflight.remove(&url);
        self.inflight_source.remove(&url);
        let until = Instant::now() + URL_SOFT_FAIL;
        self.soft_fail.insert(url.clone(), until);
        if let Some(host) = host_key(&url) {
            let entry = self.host_circuit.entry(host.clone()).or_insert(HostCircuit {
                fails: 0,
                cool_until: None,
            });
            if entry.cool_until.is_some_and(|t| Instant::now() < t) {
                return;
            }
            entry.fails = entry.fails.saturating_add(1);
            if entry.fails >= HOST_FAIL_TRIP {
                entry.cool_until = Some(Instant::now() + HOST_COOLDOWN);
                entry.fails = 0;
                warn!(
                    %host,
                    secs = HOST_COOLDOWN.as_secs(),
                    "image host circuit open — skipping further fetches"
                );
            }
        }
    }

    /// Drop inflight without blacklisting — allows retry after transient network errors.
    pub fn clear_inflight(&mut self, url: &str) {
        self.inflight.remove(url);
        self.inflight_source.remove(url);
    }
}

#[derive(Debug)]
pub enum RequestOutcome {
    Ready,
    Pending,
    Skip,
    Fetch {
        url: String,
        source_id: Option<Uuid>,
    },
}

fn host_key(url: &str) -> Option<String> {
    let u = Url::parse(url).ok()?;
    let host = u.host_str()?;
    match u.port() {
        Some(p) => Some(format!("{host}:{p}")),
        None => Some(host.to_string()),
    }
}

fn shared_http() -> reqwest::Client {
    fluxplay_providers::app_http("IPTVSmartersPlayer", 15).unwrap_or_else(|e| {
        error!(error = %e, "image http client build failed — emergency default");
        reqwest::Client::builder()
            .user_agent("IPTVSmartersPlayer")
            .timeout(std::time::Duration::from_secs(15))
            .connect_timeout(std::time::Duration::from_secs(6))
            .no_proxy()
            .build()
            .expect("image http client")
    })
}

/// Direct clearnet client (no SOCKS) — fallback when WireGuard cannot reach image CDNs.
fn clearnet_image_http() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent("IPTVSmartersPlayer")
                .timeout(Duration::from_secs(12))
                .connect_timeout(Duration::from_secs(6))
                .redirect(reqwest::redirect::Policy::limited(8))
                .gzip(true)
                .pool_max_idle_per_host(4)
                .no_proxy()
                .build()
                .expect("clearnet image http")
        })
        .clone()
}

fn warn_host_once(host: &str, msg: &str, detail: &str) {
    static LAST: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    let map = LAST.get_or_init(|| Mutex::new(HashMap::new()));
    let now = Instant::now();
    if let Ok(mut g) = map.lock() {
        if let Some(prev) = g.get(host) {
            if now.duration_since(*prev) < HOST_WARN_INTERVAL {
                debug!(%host, %detail, "{msg}");
                return;
            }
        }
        g.insert(host.to_string(), now);
    }
    warn!(%host, %detail, "{msg}");
}

/// Collapse `//`, percent-encode unicode path/query (fixes Wikimedia HTTP 400).
fn normalize_image_url(url: &str) -> String {
    let url = url.trim();
    let collapsed = if let Some((scheme, rest)) = url.split_once("://") {
        let mut path = rest.replace("//", "/");
        while path.contains("//") {
            path = path.replace("//", "/");
        }
        format!("{scheme}://{path}")
    } else {
        url.to_string()
    };
    // `Url::parse` + `as_str()` re-emits a wire-safe percent-encoded form.
    match Url::parse(&collapsed) {
        Ok(u) => u.as_str().to_string(),
        Err(_) => collapsed,
    }
}

pub fn is_fetchable_image_url(url: &str) -> bool {
    let url = url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return false;
    }
    // Workspace `image` crate has no SVG decoder — skip remote .svg logos.
    let path = url.split('?').next().unwrap_or(url).to_ascii_lowercase();
    if path.ends_with(".svg") || path.contains(".svg/") {
        trace!(target: "fluxplay::images", %url, "reject svg logo url");
        return false;
    }
    if let Some(rest) = url.split("/t/p/").nth(1) {
        let segs: Vec<_> = rest
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        if segs.len() < 2 {
            return false;
        }
    }
    true
}

fn is_connect_like(err: &reqwest::Error) -> bool {
    err.is_connect()
        || err.is_timeout()
        || err.is_request()
        || format!("{err}").contains("error sending request")
}

async fn image_get(url: &str) -> Result<reqwest::Response, reqwest::Error> {
    let primary = shared_http();
    match primary.get(url).send().await {
        Ok(r) => Ok(r),
        Err(e) => {
            let via_socks = fluxplay_providers::socks_proxy().is_some();
            if via_socks && is_connect_like(&e) {
                // When WireGuard is enabled, never fall back to clearnet (DNS/CDN leak).
                if std::env::var_os("FLUXPLAY_ALLOW_CLEARNET_IMAGES").is_some() {
                    let host = host_key(url).unwrap_or_else(|| "?".into());
                    warn_host_once(
                        &host,
                        "image via WireGuard SOCKS failed — clearnet fallback (explicit allow)",
                        &e.to_string(),
                    );
                    clearnet_image_http().get(url).send().await
                } else {
                    Err(e)
                }
            } else {
                Err(e)
            }
        }
    }
}

pub async fn fetch_image_bytes(
    url: String,
    source_id: Option<Uuid>,
) -> Result<(String, Option<Uuid>, Vec<u8>), (String, String)> {
    let url = normalize_image_url(&url);
    if !is_fetchable_image_url(&url) {
        return Err((url, "invalid image url".into()));
    }
    if let Some(path) = disk_path_for(&url, source_id) {
        if let Ok(bytes) = std::fs::read(&path) {
            if bytes.len() >= 64 && !looks_like_html(&bytes) && bytes.len() <= MAX_IMAGE_BYTES {
                trace!(%url, "fetch_image_bytes disk race hit");
                return Ok((url, source_id, bytes));
            }
        }
    }
    // Legacy global cache fallback
    if source_id.is_some() {
        if let Some(path) = disk_path_for(&url, None) {
            if let Ok(bytes) = std::fs::read(&path) {
                if bytes.len() >= 64 && !looks_like_html(&bytes) && bytes.len() <= MAX_IMAGE_BYTES {
                    if let Some(dest) = disk_path_for(&url, source_id) {
                        if let Some(parent) = dest.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = std::fs::copy(&path, &dest);
                    }
                    return Ok((url, source_id, bytes));
                }
            }
        }
    }
    let t0 = Instant::now();
    tracing::debug!(target: "fluxplay::net", method = "GET", kind = "image", url = %url, "net.request");

    let host = host_key(&url).unwrap_or_else(|| "?".into());
    let mut last_err = String::new();

    // One primary attempt (+ internal clearnet fallback). Retry only for 408/429.
    for attempt in 0..2u8 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        let resp = match image_get(&url).await {
            Ok(r) => r,
            Err(e) => {
                last_err = e.to_string();
                warn_host_once(&host, "image HTTP send failed", &last_err);
                // Don't hammer the same dead path — clearnet already tried inside image_get.
                break;
            }
        };
        let status = resp.status();
        tracing::debug!(
            target: "fluxplay::net",
            method = "GET",
            kind = "image",
            status = status.as_u16(),
            elapsed_ms = format!("{:.1}", t0.elapsed().as_secs_f64() * 1000.0),
            "net.response"
        );
        if !status.is_success() {
            if status.as_u16() == 429 || status.as_u16() == 408 {
                last_err = format!("HTTP {status}");
                continue;
            }
            // Permanent client errors — log once per host burst, then caller blacklists URL.
            if status.as_u16() >= 400 && status.as_u16() < 500 {
                debug!(%url, status = %status, "image HTTP status");
            } else {
                warn_host_once(&host, "image HTTP status", &status.to_string());
            }
            return Err((url, format!("HTTP {status}")));
        }
        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                last_err = e.to_string();
                warn_host_once(&host, "image body failed", &last_err);
                break;
            }
        };
        if bytes.len() > MAX_IMAGE_BYTES {
            warn!(%url, len = bytes.len(), "image too large");
            return Err((url, "image too large".into()));
        }
        let bytes = bytes.to_vec();
        if looks_like_html(&bytes) {
            return Err((url, "html body".into()));
        }
        if let Some(path) = disk_path_for(&url, source_id) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, &bytes);
        }
        trace!(%url, bytes = bytes.len(), "image fetched");
        return Ok((url, source_id, bytes));
    }
    Err((url, last_err))
}

/// Fetch then downscale on the blocking pool (iced `update` stays light).
pub async fn fetch_image_prepared(
    url: String,
    source_id: Option<Uuid>,
) -> Result<(String, Option<Uuid>, Vec<u8>), (String, String)> {
    fetch_image_prepared_edged(url, source_id, MAX_UI_EDGE_PX).await
}

pub async fn fetch_image_prepared_edged(
    url: String,
    source_id: Option<Uuid>,
    edge_px: u32,
) -> Result<(String, Option<Uuid>, Vec<u8>), (String, String)> {
    let (url, source_id, bytes) = fetch_image_bytes(url, source_id).await?;
    match tokio::task::spawn_blocking(move || downscale_for_ui(bytes, edge_px)).await {
        Ok(prepared) => Ok((url, source_id, prepared)),
        Err(e) => Err((url, format!("decode: {e}"))),
    }
}

fn looks_like_html(bytes: &[u8]) -> bool {
    bytes.starts_with(b"<") || bytes.starts_with(b"<!DOCTYPE") || bytes.starts_with(b"<!doctype")
}

pub(crate) fn downscale_for_ui(bytes: Vec<u8>, edge_px: u32) -> Vec<u8> {
    let edge = edge_px.max(160);
    let Ok(img) = image::load_from_memory(&bytes) else {
        return bytes;
    };
    let (w, h) = img.dimensions();
    if w <= edge && h <= edge {
        return bytes;
    }
    let resized = img.thumbnail(edge, edge);
    let mut out = Vec::new();
    if resized
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Jpeg)
        .is_ok()
        && out.len() >= 64
    {
        return out;
    }
    out.clear();
    if resized
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .is_ok()
        && out.len() >= 64
    {
        return out;
    }
    bytes
}

fn legacy_disk_root() -> PathBuf {
    #[cfg(target_os = "android")]
    {
        if let Some(app) = iced::android::ANDROID_APP.get() {
            if let Some(base) = app.internal_data_path() {
                return base.join("fluxplay").join("images");
            }
        }
    }
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("fluxplay")
        .join("images")
}

fn disk_path_for(url: &str, source_id: Option<Uuid>) -> Option<PathBuf> {
    let mut h = Sha256::new();
    h.update(url.as_bytes());
    let hex = hex::encode(h.finalize());
    let name = format!("{hex}.img");
    Some(match source_id {
        Some(id) => crate::storage::profile_images_dir(id).join(name),
        None => legacy_disk_root().join(name),
    })
}

pub fn disk_path_for_url(url: &str, source_id: Option<Uuid>) -> Option<PathBuf> {
    disk_path_for(url, source_id)
}

pub fn pick_art(
    logo: Option<&str>,
    poster: Option<&str>,
    cover: Option<&str>,
    banner: Option<&str>,
) -> Option<String> {
    [logo, poster, cover, banner]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|u| is_fetchable_image_url(u))
        .map(normalize_image_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_encodes_unicode_path() {
        let raw = "https://upload.wikimedia.org/wikipedia/fr/thumb/a/a5/Mayotte_La_1ère_-_Logo_2018.svg/1200px-Mayotte_La_1ère_-_Logo_2018.svg.png";
        let n = normalize_image_url(raw);
        assert!(n.contains("%C3%A8") || n.contains("%c3%a8"), "got {n}");
        assert!(!n.contains('è'));
    }

    #[test]
    fn host_circuit_trips_after_threshold() {
        let mut c = ImageCache::default();
        let url = "http://dead.example:8080/a.png".to_string();
        for _ in 0..HOST_FAIL_TRIP {
            c.mark_soft_failed(url.clone());
        }
        assert!(c.host_blocked(&url));
        assert!(matches!(c.request(&url, None), RequestOutcome::Skip));
    }
}
