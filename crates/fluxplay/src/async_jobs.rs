//! Shared async job progress (atomics) + chunked `spawn_blocking` helpers.
//! Keeps iced `update` off heavy SQLite / CPU work while reporting precise progress.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Default row batch size for chunked SQLite upserts / index builds.
pub const DEFAULT_CHUNK: usize = 4_000;

/// Lock-free progress pair for status bar / logs.
#[derive(Debug, Default)]
pub struct JobProgress {
    pub done: AtomicU64,
    pub total: AtomicU64,
}

impl JobProgress {
    pub fn new(total: u64) -> Self {
        Self {
            done: AtomicU64::new(0),
            total: AtomicU64::new(total),
        }
    }

    pub fn set_total(&self, total: u64) {
        self.total.store(total, Ordering::Relaxed);
    }

    pub fn reset(&self, total: u64) {
        self.total.store(total, Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
    }

    pub fn add_done(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }

    pub fn set_done(&self, n: u64) {
        self.done.store(n, Ordering::Relaxed);
    }

    pub fn tick(&self) {
        self.done.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> (u64, u64) {
        (
            self.done.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
        )
    }

    /// French short label, e.g. `ingest 40%` or `ingest 12/30`.
    pub fn label_fr(&self, name: &str) -> Option<String> {
        let (done, total) = self.snapshot();
        if total == 0 {
            return None;
        }
        if done >= total {
            return Some(format!("{name} ok"));
        }
        let pct = ((done.saturating_mul(100)) / total).min(99);
        Some(format!("{name} {pct}%"))
    }
}

/// Process-wide job meters (UI reads snapshots; workers bump atomics).
#[derive(Debug, Clone, Default)]
pub struct JobMeters {
    pub ingest: Arc<JobProgress>,
    pub browse_index: Arc<JobProgress>,
    pub images: Arc<JobProgress>,
    pub browse_gen: Arc<AtomicU64>,
}

impl JobMeters {
    pub fn new() -> Self {
        Self {
            ingest: Arc::new(JobProgress::new(0)),
            browse_index: Arc::new(JobProgress::new(0)),
            images: Arc::new(JobProgress::new(0)),
            browse_gen: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn bump_browse_gen(&self) -> u64 {
        self.browse_gen.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn browse_gen(&self) -> u64 {
        self.browse_gen.load(Ordering::Relaxed)
    }

    /// Compact FR suffix for the status bar (` · ingest 40% · images 3/24`).
    pub fn status_suffix_fr(&self) -> String {
        let mut parts = Vec::new();
        if let Some(s) = self.ingest.label_fr("ingest") {
            let (d, t) = self.ingest.snapshot();
            if d < t {
                parts.push(s);
            }
        }
        if let Some(s) = self.browse_index.label_fr("index") {
            let (d, t) = self.browse_index.snapshot();
            if d < t {
                parts.push(s);
            }
        }
        let (img_d, img_t) = self.images.snapshot();
        if img_t > 0 && img_d < img_t {
            parts.push(format!("images {img_d}/{img_t}"));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!(" · {}", parts.join(" · "))
        }
    }
}

/// Run CPU-bound work on Tokio's blocking pool.
pub async fn run_blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("spawn_blocking: {e}"))
}

/// Fold `items` in chunks of `chunk`, calling `on_chunk` for each slice.
/// Updates `progress` after every chunk (`done` += chunk len).
#[allow(dead_code)]
pub fn for_each_chunk<T, F>(
    items: &[T],
    chunk: usize,
    progress: Option<&JobProgress>,
    mut on_chunk: F,
) -> Result<(), String>
where
    F: FnMut(&[T]) -> Result<(), String>,
{
    let chunk = chunk.max(1);
    if let Some(p) = progress {
        p.set_total(items.len() as u64);
        p.set_done(0);
    }
    let mut offset = 0usize;
    while offset < items.len() {
        let end = (offset + chunk).min(items.len());
        on_chunk(&items[offset..end])?;
        if let Some(p) = progress {
            p.add_done((end - offset) as u64);
        }
        offset = end;
    }
    Ok(())
}
