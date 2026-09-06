//! Logo / poster / banner cache — memory LRU + disk persistence (zero re-download).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::OnceLock;

use iced::widget::image::Handle;
use sha2::{Digest, Sha256};
use tracing::{debug, error, trace, warn};

/// Cap RAM handles — mosaic only needs the visible window.
const MAX_RAM_HANDLES: usize = 96;
const MAX_IMAGE_BYTES: usize = 2_500_000;

#[derive(Debug, Default)]
pub struct ImageCache {
    handles: HashMap<String, Handle>,
    order: VecDeque<String>,
    inflight: HashSet<String>,
    failed: HashSet<String>,
}

impl ImageCache {
    pub fn get(&self, url: &str) -> Option<&Handle> {
        self.handles.get(url)
    }

    /// Try disk before scheduling a network fetch.
    pub fn request(&mut self, url: &str) -> RequestOutcome {
        if url.is_empty() || !is_fetchable_image_url(url) || self.failed.contains(url) {
            return RequestOutcome::Skip;
        }
        if self.handles.contains_key(url) {
            trace!(%url, "image ram hit");
            return RequestOutcome::Ready;
        }
        if self.inflight.contains(url) {
            return RequestOutcome::Pending;
        }
        // Disk hit → load into RAM without network.
        if let Some(path) = disk_path_for(url) {
            if let Ok(bytes) = std::fs::read(&path) {
                if bytes.len() >= 64 && !looks_like_html(&bytes) {
                    debug!(%url, "image disk hit");
                    self.insert_bytes(url.to_string(), bytes);
                    return RequestOutcome::Ready;
                }
            }
        }
        trace!(%url, "image fetch enqueue");
        self.inflight.insert(url.to_string());
        RequestOutcome::Fetch(url.to_string())
    }

    pub fn insert_bytes(&mut self, url: String, bytes: Vec<u8>) {
        self.inflight.remove(&url);
        if bytes.len() < 64 || looks_like_html(&bytes) {
            self.failed.insert(url);
            return;
        }
        // Persist to disk (best-effort).
        if let Some(path) = disk_path_for(&url) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, &bytes);
        }
        if self.handles.contains_key(&url) {
            self.handles.insert(url, Handle::from_bytes(bytes));
            return;
        }
        while self.handles.len() >= MAX_RAM_HANDLES {
            if let Some(old) = self.order.pop_front() {
                self.handles.remove(&old);
            } else {
                break;
            }
        }
        self.order.push_back(url.clone());
        self.handles.insert(url, Handle::from_bytes(bytes));
    }

    pub fn mark_failed(&mut self, url: String) {
        debug!(%url, "image mark failed");
        self.inflight.remove(&url);
        self.failed.insert(url);
    }
}

#[derive(Debug)]
pub enum RequestOutcome {
    Ready,
    Pending,
    Skip,
    Fetch(String),
}

fn shared_http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        match reqwest::Client::builder()
            .user_agent("IPTVSmartersPlayer")
            .timeout(std::time::Duration::from_secs(15))
            .connect_timeout(std::time::Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::limited(5))
            .pool_max_idle_per_host(4)
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                error!(error = %e, "image http client build failed");
                panic!("image http client: {e}");
            }
        }
    })
}

/// Reject incomplete CDN roots (e.g. TMDB size path without poster id → HTTP 404).
pub fn is_fetchable_image_url(url: &str) -> bool {
    let url = url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return false;
    }
    if let Some(rest) = url.split("/t/p/").nth(1) {
        let segs: Vec<_> = rest
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();
        // Need size token + file id: /t/p/w600_…/abc.jpg
        if segs.len() < 2 {
            return false;
        }
    }
    true
}

pub async fn fetch_image_bytes(url: String) -> Result<(String, Vec<u8>), (String, String)> {
    if !is_fetchable_image_url(&url) {
        return Err((url, "invalid image url".into()));
    }
    // Second chance disk (race with another task).
    if let Some(path) = disk_path_for(&url) {
        if let Ok(bytes) = std::fs::read(&path) {
            if bytes.len() >= 64 && !looks_like_html(&bytes) && bytes.len() <= MAX_IMAGE_BYTES {
                trace!(%url, "fetch_image_bytes disk race hit");
                return Ok((url, bytes));
            }
        }
    }
    let t0 = std::time::Instant::now();
    tracing::info!(target: "fluxplay::net", method = "GET", kind = "image", url = %url, "net.request");
    let resp = shared_http()
        .get(&url)
        .send()
        .await
        .map_err(|e| {
            warn!(%url, error = %e, "image HTTP send failed");
            (url.clone(), e.to_string())
        })?;
    let status = resp.status();
    tracing::info!(
        target: "fluxplay::net",
        method = "GET",
        kind = "image",
        status = status.as_u16(),
        elapsed_ms = format!("{:.1}", t0.elapsed().as_secs_f64() * 1000.0),
        "net.response"
    );
    if !status.is_success() {
        warn!(%url, status = %status, "image HTTP status");
        return Err((url, format!("HTTP {status}")));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| {
            warn!(%url, error = %e, "image body failed");
            (url.clone(), e.to_string())
        })?;
    if bytes.len() > MAX_IMAGE_BYTES {
        warn!(%url, len = bytes.len(), "image too large");
        return Err((url, "image too large".into()));
    }
    trace!(%url, bytes = bytes.len(), "image fetched");
    Ok((url, bytes.to_vec()))
}

fn looks_like_html(bytes: &[u8]) -> bool {
    bytes.starts_with(b"<") || bytes.starts_with(b"<!DOCTYPE") || bytes.starts_with(b"<!doctype")
}

fn disk_root() -> PathBuf {
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

fn disk_path_for(url: &str) -> Option<PathBuf> {
    let mut h = Sha256::new();
    h.update(url.as_bytes());
    let hex = hex::encode(h.finalize());
    Some(disk_root().join(format!("{hex}.img")))
}

/// Best art URL for a channel / VOD / series row.
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
        .map(str::to_string)
}
