//! Remote logo / poster / banner cache for the media-center UI.

use std::collections::{HashMap, HashSet};

use iced::widget::image::Handle;

#[derive(Debug, Default)]
pub struct ImageCache {
    handles: HashMap<String, Handle>,
    inflight: HashSet<String>,
    failed: HashSet<String>,
}

impl ImageCache {
    pub fn get(&self, url: &str) -> Option<&Handle> {
        self.handles.get(url)
    }

    /// Returns a handle if ready; otherwise queues a fetch (caller must spawn Task).
    pub fn request(&mut self, url: &str) -> RequestOutcome {
        if url.is_empty() || self.failed.contains(url) {
            return RequestOutcome::Skip;
        }
        if self.handles.contains_key(url) {
            return RequestOutcome::Ready;
        }
        if self.inflight.contains(url) {
            return RequestOutcome::Pending;
        }
        self.inflight.insert(url.to_string());
        RequestOutcome::Fetch(url.to_string())
    }

    pub fn insert_bytes(&mut self, url: String, bytes: Vec<u8>) {
        self.inflight.remove(&url);
        if bytes.len() < 64 {
            self.failed.insert(url);
            return;
        }
        // Basic sniff — reject HTML error pages.
        if bytes.starts_with(b"<") || bytes.starts_with(b"<!DOCTYPE") {
            self.failed.insert(url);
            return;
        }
        self.handles.insert(url, Handle::from_bytes(bytes));
    }

    pub fn mark_failed(&mut self, url: String) {
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

pub async fn fetch_image_bytes(url: String) -> Result<(String, Vec<u8>), (String, String)> {
    let client = reqwest::Client::builder()
        .user_agent("IPTVSmartersPlayer")
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| (url.clone(), e.to_string()))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| (url.clone(), e.to_string()))?;
    if !resp.status().is_success() {
        return Err((url, format!("HTTP {}", resp.status())));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| (url.clone(), e.to_string()))?;
    if bytes.len() > 4_000_000 {
        return Err((url, "image too large".into()));
    }
    Ok((url, bytes.to_vec()))
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
        .find(|u| u.starts_with("http://") || u.starts_with("https://"))
        .map(str::to_string)
}
