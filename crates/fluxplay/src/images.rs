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
/// Handles hold the compressed (downscaled) file, ~15–40 KB each.
pub(crate) const MAX_RAM_HANDLES: usize = 72;
const MAX_IMAGE_BYTES: usize = 2_500_000;
/// Default soft cap when host caps are not applied yet.
pub const MAX_INFLIGHT_IMAGES: usize = 24;
/// Longest edge for GPU/RAM handles (overridden by host decode_edge_px).
pub(crate) const MAX_UI_EDGE_PX: u32 = 300;

const URL_SOFT_FAIL: Duration = Duration::from_secs(12 * 60);
/// Cap permanent-fail set — over cap, clear wholesale (worst case: one retry).
const FAILED_CAP: usize = 4096;
/// Sweep expired soft-fail entries once the map passes this size.
const SOFT_FAIL_SWEEP_AT: usize = 1024;
/// Cap for the `normalize_image_url` memo map — clear-on-full.
const NORMALIZE_CACHE_CAP: usize = 2048;
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
        // Fetches are stored under the normalized URL (`@` → `%40` on IMDb
        // headshots). Callers still pass the original address.
        let url_n = normalized_cached(url);
        let h = self.handles.get(&url_n);
        if h.is_some() {
            trace!(target: "fluxplay::images", url = %url_n, "cache hit");
        }
        h
    }

    pub fn inflight_len(&self) -> usize {
        self.inflight.len()
    }

    /// O(1) MRU bump — safe to call for every visible mosaic tile each frame.
    pub fn promote(&mut self, url: &str) {
        self.clock = self.clock.wrapping_add(1);
        if let Some(gen) = self.touch_gen.get_mut(url) {
            *gen = self.clock;
            return;
        }
        if self.handles.contains_key(url) {
            self.touch_gen.insert(url.to_string(), self.clock);
        }
    }

    /// Drop decoded posters. Disk copies stay, so the grid refills from disk
    /// when the browser comes back. Failed-URL memory is kept (tiny, avoids refetch storms).
    pub fn release_ram(&mut self) {
        self.handles.clear();
        self.touch_gen.clear();
        self.inflight.clear();
        self.inflight_source.clear();
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
        let url_n = normalized_cached(url);
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

    #[allow(dead_code)]
    pub fn take_inflight_source(&mut self, url: &str) -> Option<Uuid> {
        self.inflight_source.remove(url).flatten()
    }

    #[allow(dead_code)]
    pub fn insert_bytes(&mut self, url: String, bytes: Vec<u8>) {
        self.inflight.remove(&url);
        self.inflight_source.remove(&url);
        self.soft_fail.remove(&url);
        if bytes.len() < 64 || looks_like_html(&bytes) {
            self.insert_failed(url);
            return;
        }
        let bytes = downscale_for_ui(bytes, self.decode_edge_px);
        self.insert_ui_bytes(url, bytes);
    }

    /// Insert bytes already prepared for UI (decoded/downscaled off-thread).
    /// New `Handle` so iced/wgpu re-uploads after the shared UI atlas was reused (e.g. soft video).
    pub fn refresh_gpu_texture(&mut self, url: &str, _source_id: Option<Uuid>) {
        let url_n = normalized_cached(url);
        // Same (already downscaled) bytes under a fresh id: the disk copy is the
        // full-size original and would be decoded at full resolution.
        let Some(Handle::Bytes(_, bytes)) = self.handles.get(&url_n) else {
            return;
        };
        let fresh = Handle::from_bytes(bytes.clone());
        self.handles.insert(url_n.clone(), fresh);
        self.promote(&url_n);
        debug!(target: "fluxplay::images", %url_n, "poster GPU texture refreshed");
    }

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
            self.insert_failed(url);
            return;
        }
        // A progressive JPEG decodes in the `image` crate and still uploads as an
        // empty frame. Rewrite it here too, in case the fetch path handed the
        // original bytes through.
        let bytes = if is_progressive_jpeg(&bytes) {
            let rewritten = downscale_for_ui(bytes, self.decode_edge_px);
            if is_progressive_jpeg(&rewritten) {
                warn!(target: "fluxplay::images", %url, "progressive jpeg could not be rewritten");
            }
            rewritten
        } else {
            bytes
        };
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

    fn insert_failed(&mut self, url: String) {
        if self.failed.len() >= FAILED_CAP {
            self.failed.clear();
        }
        self.failed.insert(url);
    }

    pub fn mark_failed(&mut self, url: String) {
        debug!(%url, "image mark failed (permanent)");
        self.inflight.remove(&url);
        self.inflight_source.remove(&url);
        self.soft_fail.remove(&url);
        self.insert_failed(url);
    }

    /// Transient network error: cool the URL + bump host circuit (no retry storm).
    pub fn mark_soft_failed(&mut self, url: String) {
        self.inflight.remove(&url);
        self.inflight_source.remove(&url);
        let now = Instant::now();
        // Amortized O(n) sweep: prune expired entries once the map grows large.
        if self.soft_fail.len() >= SOFT_FAIL_SWEEP_AT {
            self.soft_fail.retain(|_, until| *until > now);
        }
        let until = now + URL_SOFT_FAIL;
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
    #[allow(dead_code)]
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

const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";

/// Amazon, Wikimedia and AniList reject the IPTV user agent and return an empty tile.
fn wants_browser_ua(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    u.contains("media-amazon.com")
        || u.contains("imdb.com")
        || u.contains("wikimedia.org")
        || u.contains("wikipedia.org")
        || u.contains("anilist.co")
        || u.contains("anilistcdn")
}

fn browser_image_http() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent(BROWSER_UA)
                .timeout(Duration::from_secs(15))
                .connect_timeout(Duration::from_secs(6))
                .redirect(reqwest::redirect::Policy::limited(8))
                .gzip(true)
                .no_proxy()
                .build()
                .expect("browser image http")
        })
        .clone()
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
        // Collapse `//` in the path only: the query may legitimately hold `//`
        // (e.g. `?u=http://…`).
        let (path, query) = match rest.find(['?', '#']) {
            Some(i) => rest.split_at(i),
            None => (rest, ""),
        };
        let mut path = path.replace("//", "/");
        while path.contains("//") {
            path = path.replace("//", "/");
        }
        format!("{scheme}://{path}{query}")
    } else {
        url.to_string()
    };
    // `Url::parse` + `as_str()` re-emits a wire-safe percent-encoded form.
    match Url::parse(&collapsed) {
        Ok(u) => u.as_str().to_string(),
        Err(_) => collapsed,
    }
}

/// Memoized `normalize_image_url` — pure function called per tile per frame.
/// Global `Mutex` map (same pattern as `warn_host_once`); clear-on-full cap.
fn normalized_cached(url: &str) -> String {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let map = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut g) = map.lock() {
        if let Some(hit) = g.get(url) {
            return hit.clone();
        }
        let out = normalize_image_url(url);
        if g.len() >= NORMALIZE_CACHE_CAP {
            g.clear();
        }
        g.insert(url.to_string(), out.clone());
        out
    } else {
        normalize_image_url(url)
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
    let primary = if wants_browser_ua(url) {
        browser_image_http()
    } else {
        shared_http()
    };
    let mut req = primary.get(url);
    if url.contains("media-amazon.com") || url.contains("imdb.com") {
        req = req.header("Referer", "https://www.imdb.com/");
    }
    match req.send().await {
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
    let url = normalized_cached(&url);
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
        let mut resp = match image_get(&url).await {
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
            // The resized IMDb portrait 404s for some people. The original file still exists.
            if let Some(alt) = url
                .find("._V1_QL75_")
                .map(|i| format!("{}._V1_.jpg", &url[..i]))
            {
                if let Ok(alt_resp) = image_get(&alt).await {
                    if alt_resp.status().is_success() {
                        resp = alt_resp;
                    } else {
                        return Err((url, format!("HTTP {status}")));
                    }
                } else {
                    return Err((url, format!("HTTP {status}")));
                }
            } else {
                if status.as_u16() >= 400 && status.as_u16() < 500 {
                    debug!(%url, status = %status, "image HTTP status");
                } else {
                    warn_host_once(&host, "image HTTP status", &status.to_string());
                }
                return Err((url, format!("HTTP {status}")));
            }
        }
        if resp.content_length().is_some_and(|n| n > MAX_IMAGE_BYTES as u64) {
            warn!(%url, "image too large");
            return Err((url, "image too large".into()));
        }
        // Read under the cap: a server lying about (or omitting) the length must not
        // make us buffer a video file as a "logo".
        let mut bytes = Vec::new();
        let mut too_large = false;
        let mut body_err = None;
        loop {
            match resp.chunk().await {
                Ok(Some(c)) => {
                    if bytes.len() + c.len() > MAX_IMAGE_BYTES {
                        too_large = true;
                        break;
                    }
                    bytes.extend_from_slice(&c);
                }
                Ok(None) => break,
                Err(e) => {
                    body_err = Some(fluxplay_providers::redact_error(&e));
                    break;
                }
            }
        }
        if let Some(e) = body_err {
            last_err = e;
            warn_host_once(&host, "image body failed", &last_err);
            break;
        }
        if too_large {
            warn!(%url, "image too large");
            return Err((url, "image too large".into()));
        }
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
#[allow(dead_code)]
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

/// SOF2 (`0xC2`) is a progressive JPEG. Baseline is SOF0 (`0xC0`).
fn is_progressive_jpeg(bytes: &[u8]) -> bool {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return false;
    }
    let mut i = 2;
    while i + 3 < bytes.len() {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        if marker == 0xC2 || marker == 0xC6 {
            return true;
        }
        if marker == 0xC0 || marker == 0xC1 || marker == 0xDA {
            return false;
        }
        if marker == 0xD8 || marker == 0xD9 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        let seglen = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        if seglen < 2 {
            return false;
        }
        i += 2 + seglen;
    }
    false
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
    // Keep a small PNG or WebP: re-encoding a logo flattens its transparency.
    // Every JPEG is rewritten below. Some IMDb portraits decode here and still
    // never appear in iced when the original bytes are handed to the renderer.
    if w <= edge && h <= edge && !bytes.starts_with(&[0xFF, 0xD8]) {
        return bytes;
    }
    let resized = img.thumbnail(edge, edge);
    let mut out = Vec::new();
    // Headshots stay under the frame size. Write them as PNG: iced drops some
    // of the JPEGs IMDb returns, even after a baseline re-encode.
    let small_jpeg = bytes.starts_with(&[0xFF, 0xD8]) && w <= edge && h <= edge;
    let format = if resized.color().has_alpha() || small_jpeg {
        image::ImageFormat::Png
    } else {
        image::ImageFormat::Jpeg
    };
    if resized
        .write_to(&mut Cursor::new(&mut out), format)
        .is_ok()
        && out.len() >= 64
    {
        return out;
    }
    if format != image::ImageFormat::Png {
        out.clear();
        if resized
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .is_ok()
            && out.len() >= 64
        {
            return out;
        }
    }
    bytes
}

/// Width / height from the container header. Used while painting so a landscape
/// banner is not center-cropped into a poster, and a real poster still fills.
pub fn raster_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() >= 24 && bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        return (w > 0 && h > 0).then_some((w, h));
    }
    if bytes.len() >= 10 && bytes.starts_with(b"GIF8") {
        let w = u16::from_le_bytes(bytes[6..8].try_into().ok()?) as u32;
        let h = u16::from_le_bytes(bytes[8..10].try_into().ok()?) as u32;
        return (w > 0 && h > 0).then_some((w, h));
    }
    if bytes.len() >= 30 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return webp_size(bytes);
    }
    if bytes.len() > 4 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        return jpeg_size(bytes);
    }
    None
}

fn webp_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() >= 30 && &bytes[12..16] == b"VP8X" {
        let w = 1 + u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0]);
        let h = 1 + u32::from_le_bytes([bytes[27], bytes[28], bytes[29], 0]);
        return (w > 1 && h > 1).then_some((w, h));
    }
    if bytes.len() >= 30 && &bytes[12..16] == b"VP8 " {
        // Lossy: width/height are 14-bit fields at the start of the frame tag.
        let w = u16::from_le_bytes(bytes[26..28].try_into().ok()?) as u32 & 0x3FFF;
        let h = u16::from_le_bytes(bytes[28..30].try_into().ok()?) as u32 & 0x3FFF;
        return (w > 0 && h > 0).then_some((w, h));
    }
    if bytes.len() >= 25 && &bytes[12..16] == b"VP8L" {
        let b = &bytes[21..25];
        let bits = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let w = (bits & 0x3FFF) + 1;
        let h = ((bits >> 14) & 0x3FFF) + 1;
        return Some((w, h));
    }
    None
}

fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2usize;
    while i + 9 < bytes.len() {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        while i < bytes.len() && bytes[i] == 0xFF {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let marker = bytes[i];
        i += 1;
        if marker == 0xD9 || marker == 0xDA {
            break;
        }
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        if i + 1 >= bytes.len() {
            break;
        }
        let len = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as usize;
        if len < 2 || i + len > bytes.len() {
            break;
        }
        if matches!(marker, 0xC0 | 0xC1 | 0xC2) && len >= 7 {
            let h = u16::from_be_bytes([bytes[i + 3], bytes[i + 4]]) as u32;
            let w = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
            if w > 0 && h > 0 {
                return Some((w, h));
            }
        }
        i += len;
    }
    None
}

/// Portrait art fills the poster frame. Logos, squares and banners stay whole.
pub fn art_content_fit(handle: &Handle) -> iced::ContentFit {
    let Handle::Bytes(_, bytes) = handle else {
        return iced::ContentFit::Contain;
    };
    match raster_size(bytes) {
        Some((w, h)) if h as f32 >= w as f32 * 1.2 => iced::ContentFit::Cover,
        _ => iced::ContentFit::Contain,
    }
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
        .map(normalized_cached)
}

/// VOD mosaic/detail poster URL (list payloads often only have `stream_icon`).
pub fn vod_poster_url(v: &fluxplay_core::models::VodItem) -> Option<String> {
    pick_art(None, v.poster.as_deref(), None, None)
}

/// Series mosaic/detail art (`cover` / `cover_big` banner).
pub fn series_cover_url(s: &fluxplay_core::models::SeriesItem) -> Option<String> {
    pick_art(None, None, s.cover.as_deref(), s.banner.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imdb_headshot_at_sign_is_percent_encoded() {
        let raw = "https://m.media-amazon.com/images/M/MV5BMjI0MTg3MzI0M15BMl5BanBnXkFtZTcwMzQyODU2Mw@@._V1_.jpg";
        let n = normalize_image_url(raw);
        assert!(n.contains("Mw@@") || n.contains("%40"), "{n}");
        let mut cache = ImageCache::default();
        cache.insert_ui_bytes(n, vec![0xFF; 80]);
        assert!(cache.get(raw).is_some());
    }

    #[test]
    fn normalize_encodes_unicode_path() {
        let raw = "https://upload.wikimedia.org/wikipedia/fr/thumb/a/a5/Mayotte_La_1ère_-_Logo_2018.svg/1200px-Mayotte_La_1ère_-_Logo_2018.svg.png";
        let n = normalize_image_url(raw);
        assert!(n.contains("%C3%A8") || n.contains("%c3%a8"), "got {n}");
        assert!(!n.contains('è'));
    }

    #[test]
    fn portrait_bytes_cover_landscape_bytes_contain() {
        let poster = Handle::from_bytes(fake_png(600, 900));
        assert_eq!(art_content_fit(&poster), iced::ContentFit::Cover);
        let banner = Handle::from_bytes(fake_png(1280, 720));
        assert_eq!(art_content_fit(&banner), iced::ContentFit::Contain);
        let logo = Handle::from_bytes(fake_png(256, 256));
        assert_eq!(art_content_fit(&logo), iced::ContentFit::Contain);
    }

    #[test]
    fn downscale_preserves_small_png_and_alpha() {
        let tiny = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            24,
            24,
            image::Rgba([8, 16, 32, 0]),
        ));
        let mut raw = Vec::new();
        tiny.write_to(&mut Cursor::new(&mut raw), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(downscale_for_ui(raw.clone(), 160), raw);

        let wide = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            400,
            120,
            image::Rgba([8, 16, 32, 128]),
        ));
        let mut big = Vec::new();
        wide.write_to(&mut Cursor::new(&mut big), image::ImageFormat::Png)
            .unwrap();
        let out = downscale_for_ui(big, 160);
        assert!(
            out.starts_with(&[0x89, b'P', b'N', b'G']),
            "alpha downscale must stay PNG"
        );
        assert!(image::load_from_memory(&out).unwrap().color().has_alpha());
    }

    fn fake_png(w: u32, h: u32) -> Vec<u8> {
        let mut b = vec![0u8; 24];
        b[0..8].copy_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        b[16..20].copy_from_slice(&w.to_be_bytes());
        b[20..24].copy_from_slice(&h.to_be_bytes());
        b
    }

    #[test]
    fn progressive_headshot_is_rewritten_to_baseline() {
        let path = r"C:\Users\dxtrp\AppData\Local\Temp\steve-golin.jpg";
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        assert!(is_progressive_jpeg(&bytes));
        let out = downscale_for_ui(bytes, 480);
        assert!(!is_progressive_jpeg(&out), "cast photo stayed progressive");
        let img = image::load_from_memory(&out).unwrap().to_rgba8();
        let p = *img.get_pixel(img.width() / 2, img.height() / 5);
        assert!(
            p[0] > 15 || p[1] > 15 || p[2] > 15,
            "rewritten portrait decoded blank: {p:?}"
        );
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
