//! User-facing film / episode downloads.
//!
//! Files land in [`crate::storage::downloads_dir`] under a readable name
//! (`Vaiana 2.mkv`, `Série — S01E02 — Titre.mp4`) so the user can move, copy or
//! open them with any program. Bytes stream to `<name>.part` and are renamed
//! only when complete, so a half-written file never looks finished.
//!
//! Interrupted transfers resume: network drops reconnect with an HTTP `Range`
//! request (exponential backoff), and a `.part` left by a closed app is picked
//! up again the next time the same title is downloaded. The sidecar
//! `<name>.part.meta` stores a hash of the URL — never the URL itself, which
//! carries the account credentials.
//!
//! Large files that support ranges are fetched by several connections at once
//! — see [`parallel`] — sharing the stream token of one server ([`links`]);
//! the account's other servers take over when it fails. How many titles and
//! connections the account's downloads hold together is decided by
//! [`account`].

mod account;
mod links;
mod parallel;

use account::{Account, Slot};
use links::Links;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use fluxplay_core::models::MediaSource;
use iced::futures::channel::mpsc;
use iced::futures::{SinkExt, Stream};
use reqwest::header;
use reqwest::StatusCode;
use tokio::io::AsyncWriteExt;

/// Hard ceiling per file (a 4K remux is ~60 GiB). Guards against a server that
/// streams forever.
const MAX_BYTES: u64 = 128 * 1024 * 1024 * 1024;
/// Abort one attempt when the server sends nothing for this long (then retry).
const STALL_TIMEOUT: Duration = Duration::from_secs(45);
const PROGRESS_EVERY: Duration = Duration::from_millis(500);
/// Fewer, larger writes: one `spawn_blocking` per MiB instead of per 16 KiB chunk.
const WRITE_BUFFER: usize = 1024 * 1024;
/// An attempt that moved at least this much resets the retry budget.
const PROGRESS_RESETS_RETRIES: u64 = 1024 * 1024;
/// A `.part` without sidecar touched this recently may still be written by
/// another FluxPlay window (older builds did not lock) — never adopt it.
const LEGACY_PART_IDLE: Duration = Duration::from_secs(30);
const META_MAGIC: &str = "fluxplay-download 1";
/// Segmented `.part` (preallocated, holes listed in `todo=`). A new magic so
/// older builds never mistake the preallocated length for progress.
const META_MAGIC_SEGMENTED: &str = "fluxplay-download 2";
/// Connections per account when the panel does not say (`max_connections`).
const DEFAULT_CONNECTIONS: usize = 2;
/// Never more than this per account, whatever the panel allows.
const MAX_CONNECTIONS: usize = 8;
/// First wait before renewing a stream token another player keeps taking back.
const RENEW_WAIT: Duration = Duration::from_secs(15);
const KNOWN_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "m4v", "avi", "mov", "webm", "ts", "m2ts", "mpg", "mpeg", "flv", "wmv",
];

#[derive(Debug, Clone)]
pub enum DownloadEvent {
    /// Destination chosen; bytes go to `part` until done.
    Started { part: PathBuf },
    /// `rate` in bytes/s (smoothed), 0 until measured.
    Progress {
        done: u64,
        total: Option<u64>,
        rate: u64,
        /// Connections transferring right now.
        connections: usize,
    },
    /// The account already fetches as many titles as its panel allows.
    Waiting,
    /// Connection lost; reconnecting after `wait` (resumes where it stopped).
    Retrying {
        attempt: u32,
        wait: Duration,
        error: String,
    },
    Finished(Result<PathBuf, String>),
}

/// Transfers at once; the rest wait as [`DownloadState::Queued`]. IPTV panels
/// usually allow 1–2 connections per account, and playback needs one.
pub const MAX_PARALLEL: usize = 2;

#[derive(Debug)]
pub enum DownloadState {
    /// Waiting for a free slot (whole-season / whole-series downloads).
    Queued {
        name: String,
    },
    Running {
        name: String,
        done: u64,
        total: Option<u64>,
        part: Option<PathBuf>,
        handle: iced::task::Handle,
    },
    Done {
        path: PathBuf,
    },
    /// The error text goes to the status bar; the button offers a retry
    /// (which resumes from the kept `.part`).
    Failed,
}

/// A resumable `.part` on disk, keyed by [`url_key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partial {
    pub done: u64,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
struct RetryPolicy {
    /// Consecutive failed attempts (without real progress) before giving up.
    max_attempts: u32,
    base: Duration,
    cap: Duration,
}

impl RetryPolicy {
    const DEFAULT: Self = Self {
        max_attempts: 10,
        base: Duration::from_secs(2),
        cap: Duration::from_secs(30),
    };

    fn delay(&self, attempt: u32) -> Duration {
        let factor = 1u32 << attempt.saturating_sub(1).min(16);
        self.base.saturating_mul(factor).min(self.cap)
    }
}

/// One title to download.
#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub name: String,
    /// Catalog URL of the file.
    pub url: String,
    /// Profile the title comes from: its servers, connection limit, User-Agent
    /// and Referer apply.
    pub source: Option<MediaSource>,
    pub user_agent: String,
    pub dir: PathBuf,
    /// Runs while the user watches (next-episode preload): skipped when the
    /// account allows a single connection, which playback already holds.
    pub background: bool,
}

/// Resolved transfer plan: every server of the file, and the account whose
/// streams and connection slots it shares.
pub(crate) struct Plan {
    links: Arc<Links>,
    account: Arc<Account>,
    tuning: parallel::Tuning,
}

/// Stream the download described by `req`, reporting progress.
pub fn run(req: DownloadRequest) -> impl Stream<Item = DownloadEvent> {
    iced::stream::channel(16, async move |mut tx: mpsc::Sender<DownloadEvent>| {
        let (plan, referer, max_connections) = resolve_plan(&req).await;
        // Preloading while the viewer watches is a second stream: the panel
        // would revoke playback's on a single-stream account.
        let lease = if req.background {
            match plan.account.try_lease().filter(|_| max_connections != Some(1)) {
                Some(lease) => lease,
                None => {
                    let _ = tx
                        .send(DownloadEvent::Finished(Err(
                            "connexions du compte déjà occupées".into(),
                        )))
                        .await;
                    return;
                }
            }
        } else {
            match plan.account.try_lease() {
                Some(lease) => lease,
                None => {
                    let _ = tx.send(DownloadEvent::Waiting).await;
                    plan.account.lease().await
                }
            }
        };
        let ua = req
            .source
            .as_ref()
            .and_then(|s| s.user_agent.clone())
            .filter(|u| !u.trim().is_empty())
            .unwrap_or(req.user_agent);
        let result = download_into(
            &req.dir,
            &req.name,
            &plan,
            &ua,
            referer.as_deref(),
            RetryPolicy::DEFAULT,
            &mut tx,
        )
        .await;
        drop(lease);
        let _ = tx.send(DownloadEvent::Finished(result)).await;
    })
}

async fn resolve_plan(req: &DownloadRequest) -> (Plan, Option<String>, Option<u32>) {
    use fluxplay_providers::servers;
    let Some(source) = &req.source else {
        return (
            Plan {
                links: Arc::new(Links::new(vec![req.url.clone()], RENEW_WAIT)),
                account: Account::shared("", DEFAULT_CONNECTIONS, MAX_PARALLEL),
                tuning: parallel::Tuning::DEFAULT,
            },
            None,
            None,
        );
    };
    let ranking = servers::rank_if_stale(source).await;
    let plan = Plan {
        links: Arc::new(Links::new(servers::media_candidates(source, &req.url), RENEW_WAIT)),
        account: Account::shared(
            &source.id.to_string(),
            connections_for(ranking.max_connections),
            streams_for(ranking.max_connections),
        ),
        tuning: parallel::Tuning::DEFAULT,
    };
    let referer = source.http_referer.clone().filter(|r| !r.trim().is_empty());
    (plan, referer, ranking.max_connections)
}

/// Connections a download starts with. They share one stream, so the
/// announced limit only scales the start; probing finds the rest.
fn connections_for(max_connections: Option<u32>) -> usize {
    match max_connections {
        Some(n) => (n as usize).clamp(DEFAULT_CONNECTIONS, MAX_CONNECTIONS),
        None => DEFAULT_CONNECTIONS,
    }
}

/// Titles the account's downloads fetch at once: one stream stays free for
/// playback, unless the panel allows a single one.
fn streams_for(max_connections: Option<u32>) -> usize {
    match max_connections {
        Some(n) => (n as usize).saturating_sub(1).max(1),
        None => MAX_PARALLEL,
    }
}

/// Stable, non-reversible key for a stream URL (the URL embeds credentials).
/// Xtream media paths (`/movie|series|live/{user}/{pass}/{id}.ext`) are keyed
/// without the host, so the same title on another server of the account
/// is still recognised.
pub fn url_key(url: &str) -> String {
    let url = url.trim();
    hash_key(xtream_media_path(url).unwrap_or(url))
}

/// Key used before servers were interchangeable (whole URL).
fn legacy_url_key(url: &str) -> String {
    hash_key(url.trim())
}

/// Every key `url` may be stored under, current first.
pub fn url_keys(url: &str) -> Vec<String> {
    let mut keys = vec![url_key(url)];
    let legacy = legacy_url_key(url);
    if legacy != keys[0] {
        keys.push(legacy);
    }
    keys
}

fn hash_key(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(s.as_bytes());
    hex::encode(&digest[..16])
}

/// `/movie/u/p/42.mkv` (plus query) for an Xtream media URL, else `None`.
fn xtream_media_path(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let path = &rest[rest.find('/')?..];
    let mut seg = path.trim_start_matches('/').split('/');
    let kind = seg.next()?;
    let deep = seg.filter(|s| !s.is_empty()).count() >= 3;
    (matches!(kind, "movie" | "series" | "live" | "timeshift") && deep).then_some(path)
}

/// The partial download of `url` in `partials`, whatever key it was saved under.
pub fn partial_for<'a>(partials: &'a HashMap<String, Partial>, url: &str) -> Option<&'a Partial> {
    url_keys(url).iter().find_map(|k| partials.get(k))
}

/// Forget the partial download of `url` (under any of its keys).
pub fn forget_partial(partials: &mut HashMap<String, Partial>, url: &str) {
    for k in url_keys(url) {
        partials.remove(&k);
    }
}

/// Resumable downloads left in `dir`, by [`url_key`].
pub fn scan_partials(dir: &Path) -> HashMap<String, Partial> {
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(part) = part_for_meta(&path) else {
            continue;
        };
        let Some(meta) = Meta::read(&path) else {
            continue;
        };
        let Ok(stat) = std::fs::metadata(&part) else {
            continue;
        };
        let done = meta.done().unwrap_or(stat.len());
        out.insert(
            meta.key,
            Partial {
                done,
                total: meta.total,
            },
        );
    }
    out
}

/// Delete a `.part` and its sidecar (user cancelled).
pub fn discard_partial(part: &Path) {
    let _ = std::fs::remove_file(part);
    let _ = std::fs::remove_file(meta_path(part));
}

const LIBRARY_MAGIC: &str = "fluxplay-library 1";

/// Finished downloads, [`url_key`] → file, so a click on a downloaded title
/// plays the file instead of the stream (survives restarts and a changed
/// download folder). Stored as `<key>\t<path>` lines; no URL on disk.
#[derive(Debug, Default)]
pub struct Library {
    files: HashMap<String, PathBuf>,
    path: Option<PathBuf>,
}

impl Library {
    /// Load `path`, dropping entries whose file was deleted or moved.
    pub fn load(path: PathBuf) -> Self {
        let mut files = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            let mut lines = text.lines();
            if lines.next() == Some(LIBRARY_MAGIC) {
                for line in lines {
                    let Some((key, file)) = line.split_once('\t') else {
                        continue;
                    };
                    let file = PathBuf::from(file);
                    if !key.is_empty() && file.is_file() {
                        files.insert(key.to_string(), file);
                    }
                }
            }
        }
        Self {
            files,
            path: Some(path),
        }
    }

    pub fn get(&self, url: &str) -> Option<&Path> {
        url_keys(url)
            .iter()
            .find_map(|k| self.files.get(k))
            .map(PathBuf::as_path)
    }

    pub fn contains(&self, url: &str) -> bool {
        self.get(url).is_some()
    }

    pub fn insert(&mut self, url: &str, file: PathBuf) {
        let mut keys = url_keys(url).into_iter();
        if let Some(key) = keys.next() {
            self.files.insert(key, file);
        }
        for legacy in keys {
            self.files.remove(&legacy);
        }
        self.save();
    }

    pub fn remove(&mut self, url: &str) {
        let mut removed = false;
        for k in url_keys(url) {
            removed |= self.files.remove(&k).is_some();
        }
        if removed {
            self.save();
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let mut text = format!("{LIBRARY_MAGIC}\n");
        for (key, file) in &self.files {
            let Some(file) = file.to_str().filter(|f| !f.contains(['\t', '\n', '\r'])) else {
                continue;
            };
            text.push_str(key);
            text.push('\t');
            text.push_str(file);
            text.push('\n');
        }
        let tmp = path.with_extension("tmp");
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&tmp, text))
            .and_then(|()| std::fs::rename(&tmp, path));
        if let Err(e) = written {
            tracing::warn!(error = %e, "downloads library not saved");
        }
    }
}

/// A finished `<stem>.<ext>` for `name` in `dir` (downloads made before the
/// library existed, or by another profile).
pub fn find_by_name(dir: &Path, name: &str) -> Option<PathBuf> {
    let stem = sanitize_file_name(name);
    KNOWN_EXTENSIONS
        .iter()
        .map(|ext| dir.join(format!("{stem}.{ext}")))
        .find(|p| p.is_file())
}

/// `file://` URL the players accept for `file`. `None` when it cannot be
/// passed verbatim (mpv percent-decodes `file://` paths).
pub fn local_play_url(file: &Path) -> Option<String> {
    let s = file.to_str()?;
    (file.is_absolute() && !s.contains('%') && file.is_file()).then(|| format!("file://{s}"))
}

/// Where an unfinished download lives and the lock that keeps a second
/// window from appending to the same file.
struct Dest {
    path: PathBuf,
    part: PathBuf,
    lock: std::fs::File,
    meta: Meta,
}

impl Dest {
    /// File length (the resume offset of a sequential `.part`).
    fn len(&self) -> Result<u64, String> {
        self.lock
            .metadata()
            .map(|m| m.len())
            .map_err(|e| e.to_string())
    }

    /// Bytes safely on disk: the file length, or total minus the holes of a
    /// segmented `.part`.
    fn done(&self) -> Result<u64, String> {
        match self.meta.done() {
            Some(done) => Ok(done),
            None => self.len(),
        }
    }

    fn truncate(&mut self) -> Result<(), String> {
        self.lock.set_len(0).map_err(|e| e.to_string())?;
        self.meta.etag = None;
        self.meta.last_modified = None;
        self.meta.total = None;
        self.meta.todo = None;
        Ok(())
    }

    fn save_meta(&self) {
        let _ = self.meta.write(&meta_path(&self.part));
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Meta {
    key: String,
    total: Option<u64>,
    etag: Option<String>,
    last_modified: Option<String>,
    /// Segmented `.part`: byte ranges `[start, end)` still missing. Only
    /// ranges whose data was synced to disk are ever dropped from here.
    todo: Option<Vec<(u64, u64)>>,
}

impl Meta {
    fn read(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut lines = text.lines();
        let segmented = match lines.next()? {
            META_MAGIC => false,
            META_MAGIC_SEGMENTED => true,
            _ => return None,
        };
        let mut meta = Meta::default();
        for line in lines {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim();
            match k {
                "key" => meta.key = v.to_string(),
                "total" => meta.total = v.parse().ok(),
                "etag" if !v.is_empty() => meta.etag = Some(v.to_string()),
                "last_modified" if !v.is_empty() => meta.last_modified = Some(v.to_string()),
                "todo" if segmented => meta.todo = parse_ranges(v),
                _ => {}
            }
        }
        if segmented {
            // Holes must fit the preallocated file; otherwise the sidecar is
            // unusable and the `.part` cannot be trusted.
            let total = meta.total?;
            let todo = meta.todo.as_ref()?;
            if todo.iter().any(|&(s, e)| s >= e || e > total) {
                return None;
            }
        }
        (!meta.key.is_empty()).then_some(meta)
    }

    fn done(&self) -> Option<u64> {
        let todo = self.todo.as_ref()?;
        let missing: u64 = todo.iter().map(|(s, e)| e - s).sum();
        Some(self.total?.saturating_sub(missing))
    }

    /// Replace the sidecar atomically (tmp + rename): a crash leaves either
    /// the old or the new list of holes, never a torn one.
    fn write(&self, path: &Path) -> std::io::Result<()> {
        let one_line = |s: &Option<String>| {
            s.as_deref()
                .unwrap_or("")
                .chars()
                .filter(|c| !c.is_control())
                .collect::<String>()
        };
        let mut text = format!(
            "{}\nkey={}\ntotal={}\netag={}\nlast_modified={}\n",
            if self.todo.is_some() {
                META_MAGIC_SEGMENTED
            } else {
                META_MAGIC
            },
            self.key,
            self.total.map(|t| t.to_string()).unwrap_or_default(),
            one_line(&self.etag),
            one_line(&self.last_modified),
        );
        if let Some(todo) = &self.todo {
            let list: Vec<String> = todo.iter().map(|(s, e)| format!("{s}-{e}")).collect();
            text.push_str(&format!("todo={}\n", list.join(",")));
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)
    }

    /// `If-Range` validator: the server sends the whole file again (200) if
    /// the content changed, instead of splicing two versions together.
    fn validator(&self) -> Option<&str> {
        self.etag
            .as_deref()
            .filter(|e| !e.starts_with("W/"))
            .or(self.last_modified.as_deref())
    }
}

fn meta_path(part: &Path) -> PathBuf {
    let mut s = part.as_os_str().to_owned();
    s.push(".meta");
    PathBuf::from(s)
}

fn part_for_meta(meta: &Path) -> Option<PathBuf> {
    let name = meta.file_name()?.to_str()?;
    let part = name.strip_suffix(".meta")?;
    part.ends_with(".part").then(|| meta.with_file_name(part))
}

/// `Film.mkv.part` → `Film.mkv`.
fn final_path_for(part: &Path) -> Option<PathBuf> {
    let name = part.file_name()?.to_str()?;
    Some(part.with_file_name(name.strip_suffix(".part")?))
}

enum AttemptError {
    /// Worth reconnecting (network drop, stall, busy panel slot, 5xx).
    Retry(String),
    /// Retrying cannot help (missing file, auth, disk).
    Fatal(String),
}

async fn download_into(
    dir: &Path,
    name: &str,
    plan: &Plan,
    user_agent: &str,
    referer: Option<&str>,
    policy: RetryPolicy,
    tx: &mut mpsc::Sender<DownloadEvent>,
) -> Result<PathBuf, String> {
    let url = plan.links.first().ok_or("URL vide")?;
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| format!("dossier {} : {e}", dir.display()))?;

    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(STALL_TIMEOUT)
        .tcp_keepalive(Duration::from_secs(30))
        // Compressed transfer hides the real length and breaks byte ranges;
        // video does not compress anyway.
        .no_gzip()
        .user_agent(user_agent);
    if let Some(referer) = referer.and_then(|r| header::HeaderValue::from_str(r).ok()) {
        let mut headers = header::HeaderMap::new();
        headers.insert(header::REFERER, referer);
        builder = builder.default_headers(headers);
    }
    if let Some(px) = fluxplay_providers::required_proxy() {
        let proxy = reqwest::Proxy::all(px).map_err(|e| format!("proxy tunnel : {e}"))?;
        builder = builder.proxy(proxy);
    }
    let client = builder.build().map_err(|e| e.to_string())?;

    let keys = url_keys(url);
    let stem = sanitize_file_name(name);
    let mut dest = find_resumable(dir, &keys, &stem)?;
    if let Some(d) = &dest {
        let _ = tx
            .send(DownloadEvent::Started {
                part: d.part.clone(),
            })
            .await;
    }

    let mut session = Session {
        url_idx: 0,
        // A validator read back from disk came from an unknown server: only
        // trust it when there is a single one.
        validator_from: (plan.links.len() == 1).then_some(0),
    };
    let mut failures = 0u32;
    loop {
        let before = match &dest {
            Some(d) => d.done()?,
            None => 0,
        };
        let outcome = match dest.as_mut().filter(|d| d.meta.todo.is_some()) {
            Some(d) => parallel::run(&client, plan, d, None, tx).await,
            None => attempt(&client, plan, &mut session, dir, &stem, &keys[0], &mut dest, tx).await,
        };
        let after = match &dest {
            Some(d) => d.done().unwrap_or(before),
            None => 0,
        };
        match outcome {
            Ok(()) => {
                let d = dest.take().ok_or("destination perdue")?;
                return finish(d).await;
            }
            Err(AttemptError::Fatal(e)) => {
                if let Some(d) = dest.take() {
                    if after == 0 {
                        discard_partial(&d.part);
                    }
                }
                return Err(e);
            }
            Err(AttemptError::Retry(e)) => {
                if after >= before.saturating_add(PROGRESS_RESETS_RETRIES) {
                    failures = 0;
                }
                failures += 1;
                // Next server of the account for the next sequential attempt.
                session.url_idx = (session.url_idx + 1) % plan.links.len();
                if failures > policy.max_attempts {
                    // The `.part` stays: the next click resumes from here.
                    return Err(e);
                }
                let wait = policy.delay(failures);
                let _ = tx
                    .send(DownloadEvent::Retrying {
                        attempt: failures,
                        wait,
                        error: e,
                    })
                    .await;
                tokio::time::sleep(wait).await;
            }
        }
    }
}

/// Which server the sequential attempts use, and which one produced the
/// `If-Range` validator (another server's ETag would force a restart).
struct Session {
    url_idx: usize,
    validator_from: Option<usize>,
}

/// One HTTP request, appending to the `.part` from its current length. A
/// large file served with byte ranges switches to [`parallel::run`], this
/// response becoming its first connection.
#[allow(clippy::too_many_arguments)]
async fn attempt(
    client: &reqwest::Client,
    plan: &Plan,
    session: &mut Session,
    dir: &Path,
    stem: &str,
    key: &str,
    dest: &mut Option<Dest>,
    tx: &mut mpsc::Sender<DownloadEvent>,
) -> Result<(), AttemptError> {
    use AttemptError::{Fatal, Retry};

    let url_idx = session.url_idx.min(plan.links.len() - 1);
    let offset = match dest {
        Some(d) => d.len().map_err(Fatal)?,
        None => 0,
    };
    let slot = plan.account.acquire().await;
    let validator = dest
        .as_ref()
        .filter(|_| offset > 0 && session.validator_from == Some(url_idx))
        .and_then(|d| d.meta.validator().map(str::to_owned));
    let sent = plan
        .links
        .send(client, url_idx, |req| {
            // `bytes=0-` too: a 206 answer proves the server can split the file.
            let req = req.header(header::RANGE, format!("bytes={offset}-"));
            match &validator {
                Some(v) => req.header(header::IF_RANGE, v.as_str()),
                None => req,
            }
        })
        .await
        .map_err(|e| Retry(fluxplay_providers::redact_error(&e)))?;
    let mut resp = sent.resp;
    let status = resp.status();

    if status == StatusCode::RANGE_NOT_SATISFIABLE && offset > 0 {
        let d = dest.as_mut().ok_or(Fatal("destination perdue".into()))?;
        let server_total = resp
            .headers()
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit('/').next())
            .and_then(|t| t.parse::<u64>().ok());
        if server_total.or(d.meta.total) == Some(offset) {
            return Ok(());
        }
        d.truncate().map_err(Fatal)?;
        return Err(Retry("reprise refusée — redémarrage du fichier".into()));
    }
    if !status.is_success() {
        let msg = format!("HTTP {status}");
        return match status.as_u16() {
            400 | 401 | 404 | 405 | 410 | 451 if sent.fresh => Err(Fatal(msg)),
            _ => Err(Retry(msg)),
        };
    }

    let headers = resp.headers().clone();
    let etag = header_string(&headers, header::ETAG);
    let last_modified = header_string(&headers, header::LAST_MODIFIED);
    let body_len = resp.content_length();

    let mut total = body_len;
    if status == StatusCode::PARTIAL_CONTENT {
        let range = headers
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_range);
        match range {
            Some((start, full)) if start == offset => {
                total = full.or(body_len.map(|n| offset + n));
            }
            _ => {
                if let Some(d) = dest.as_mut() {
                    d.truncate().map_err(Fatal)?;
                }
                return Err(Retry("plage inattendue — redémarrage du fichier".into()));
            }
        }
    } else if offset > 0 {
        // Plain 200 to a range request: the server restarts from byte 0.
        if let Some(d) = dest.as_mut() {
            d.truncate().map_err(Fatal)?;
        }
    }
    if total.is_some_and(|t| t > MAX_BYTES) {
        return Err(Fatal("fichier trop volumineux".into()));
    }

    if dest.is_none() {
        let ext = extension_for(resp.url().path(), header_str(&headers, header::CONTENT_TYPE));
        let d = create_destination(dir, stem, ext, key).await.map_err(Fatal)?;
        let _ = tx
            .send(DownloadEvent::Started {
                part: d.part.clone(),
            })
            .await;
        *dest = Some(d);
    }
    let d = dest.as_mut().ok_or(Fatal("destination perdue".into()))?;
    d.meta.total = total;
    d.meta.etag = etag;
    d.meta.last_modified = last_modified;
    session.validator_from = Some(url_idx);

    if status == StatusCode::PARTIAL_CONTENT && plan.account.can_split() {
        if let Some(total) = total.filter(|t| t - offset >= plan.tuning.segmented_min) {
            // Preallocate (sparse) so every connection writes at its offset.
            d.lock.set_len(total).map_err(|e| Fatal(format!("écriture disque : {e}")))?;
            d.meta.todo = Some(vec![(offset, total)]);
            d.save_meta();
            let first = parallel::FirstResponse {
                resp,
                url_idx,
                slot,
                pos: offset,
            };
            return parallel::run(client, plan, d, Some(first), tx).await;
        }
    }
    d.save_meta();

    let file = d.lock.try_clone().map_err(|e| Fatal(e.to_string()))?;
    let mut out = tokio::io::BufWriter::with_capacity(WRITE_BUFFER, tokio::fs::File::from_std(file));
    let mut done = d.len().map_err(Fatal)?;
    let mut meter = RateMeter::new(done);
    let _ = tx
        .send(DownloadEvent::Progress {
            done,
            total,
            rate: 0,
            connections: 1,
        })
        .await;

    let body = loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                done += chunk.len() as u64;
                if done > MAX_BYTES {
                    break Err(Fatal("fichier trop volumineux".into()));
                }
                if let Err(e) = out.write_all(&chunk).await {
                    break Err(Fatal(format!("écriture disque : {e}")));
                }
                plan.account.add_bytes(chunk.len() as u64);
                if let Some(rate) = meter.tick(done) {
                    let _ = tx
                        .send(DownloadEvent::Progress {
                            done,
                            total,
                            rate,
                            connections: 1,
                        })
                        .await;
                }
            }
            Ok(None) => break Ok(()),
            Err(e) => break Err(Retry(fluxplay_providers::redact_error(&e))),
        }
    };
    drop(slot);
    // Flush even on failure so the file length is the exact resume offset.
    let flushed = out.flush().await;
    body?;
    flushed.map_err(|e| Fatal(format!("écriture disque : {e}")))?;
    if total.is_some_and(|t| done < t) {
        return Err(Retry("connexion interrompue".into()));
    }
    Ok(())
}

/// Rename `.part` to its final name (never over an existing file) and drop
/// the sidecar.
async fn finish(d: Dest) -> Result<PathBuf, String> {
    let Dest {
        path, part, lock, ..
    } = d;
    let _ = lock.sync_data();
    drop(lock);
    let target = if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        free_final_name(&path).await?
    } else {
        path
    };
    tokio::fs::rename(&part, &target)
        .await
        .map_err(|e| e.to_string())?;
    let _ = tokio::fs::remove_file(meta_path(&part)).await;
    Ok(target)
}

async fn free_final_name(path: &Path) -> Result<PathBuf, String> {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("FluxPlay");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("mp4");
    for n in 2..1000 {
        let candidate = path.with_file_name(format!("{stem} ({n}).{ext}"));
        if !tokio::fs::try_exists(&candidate).await.unwrap_or(false) {
            return Ok(candidate);
        }
    }
    Err("aucun nom de fichier libre".into())
}

/// A `.part` to continue: first by sidecar key (same title, any of `keys`),
/// else a legacy `.part` of the same title left by an older build.
fn find_resumable(dir: &Path, keys: &[String], stem: &str) -> Result<Option<Dest>, String> {
    let key = keys.first().ok_or("clé vide")?;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(None);
    };
    let mut legacy = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(part) = part_for_meta(&path) {
            if let Some(mut meta) = Meta::read(&path).filter(|m| keys.contains(&m.key)) {
                if part.exists() {
                    meta.key = key.clone();
                    return open_existing(part, meta).map(Some);
                }
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(base) = name.strip_suffix(".part") else {
            continue;
        };
        let Some((s, ext)) = base.rsplit_once('.') else {
            continue;
        };
        if s != stem || !KNOWN_EXTENSIONS.contains(&ext) || meta_path(&path).exists() {
            continue;
        }
        let idle = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age >= LEGACY_PART_IDLE);
        if idle {
            legacy = Some(path);
        }
    }
    match legacy {
        Some(part) => open_existing(
            part,
            Meta {
                key: key.to_string(),
                ..Meta::default()
            },
        )
        .map(Some),
        None => Ok(None),
    }
}

fn open_existing(part: PathBuf, meta: Meta) -> Result<Dest, String> {
    let path = final_path_for(&part).ok_or("nom de fichier invalide")?;
    let lock = std::fs::OpenOptions::new()
        .append(true)
        .open(&part)
        .map_err(|e| e.to_string())?;
    lock_exclusive(&lock)?;
    let dest = Dest {
        path,
        part,
        lock,
        meta,
    };
    dest.save_meta();
    Ok(dest)
}

async fn create_destination(dir: &Path, stem: &str, ext: &str, key: &str) -> Result<Dest, String> {
    let (path, part) = reserve_destination(dir, stem, ext).await?;
    let lock = std::fs::OpenOptions::new()
        .append(true)
        .open(&part)
        .map_err(|e| e.to_string())?;
    lock_exclusive(&lock)?;
    Ok(Dest {
        path,
        part,
        lock,
        meta: Meta {
            key: key.to_string(),
            ..Meta::default()
        },
    })
}

fn lock_exclusive(file: &std::fs::File) -> Result<(), String> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(std::fs::TryLockError::WouldBlock) => {
            Err("déjà en cours de téléchargement dans une autre fenêtre".into())
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e.to_string()),
    }
}

/// Pick a free `<stem>.<ext>` (then `<stem> (2).<ext>`, …) and create its `.part`
/// atomically so two concurrent downloads of the same title never collide.
async fn reserve_destination(
    dir: &Path,
    stem: &str,
    ext: &str,
) -> Result<(PathBuf, PathBuf), String> {
    for n in 1..1000 {
        let file = if n == 1 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem} ({n}).{ext}")
        };
        let path = dir.join(&file);
        let part = dir.join(format!("{file}.part"));
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            continue;
        }
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part)
            .await
        {
            Ok(_) => return Ok((path, part)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("aucun nom de fichier libre".into())
}

fn header_str(headers: &header::HeaderMap, name: header::HeaderName) -> &str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

fn header_string(headers: &header::HeaderMap, name: header::HeaderName) -> Option<String> {
    let v = header_str(headers, name).trim();
    (!v.is_empty()).then(|| v.to_string())
}

/// `0-100,200-300` → `[(0, 100), (200, 300)]`; empty → no holes left.
fn parse_ranges(v: &str) -> Option<Vec<(u64, u64)>> {
    v.split(',')
        .filter(|r| !r.trim().is_empty())
        .map(|r| {
            let (s, e) = r.trim().split_once('-')?;
            Some((s.parse().ok()?, e.parse().ok()?))
        })
        .collect()
}

/// `bytes 100-199/1000` → `(100, Some(1000))`; `bytes 100-199/*` → `(100, None)`.
fn parse_content_range(v: &str) -> Option<(u64, Option<u64>)> {
    let rest = v.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (start, _end) = range.split_once('-')?;
    let start = start.trim().parse().ok()?;
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    Some((start, total))
}

/// Smoothed bytes/s, emitted at most every [`PROGRESS_EVERY`].
struct RateMeter {
    last: Instant,
    last_done: u64,
    rate: f64,
}

impl RateMeter {
    fn new(done: u64) -> Self {
        Self {
            last: Instant::now(),
            last_done: done,
            rate: 0.0,
        }
    }

    fn tick(&mut self, done: u64) -> Option<u64> {
        let dt = self.last.elapsed();
        if dt < PROGRESS_EVERY {
            return None;
        }
        let inst = done.saturating_sub(self.last_done) as f64 / dt.as_secs_f64();
        self.rate = if self.rate == 0.0 {
            inst
        } else {
            self.rate * 0.7 + inst * 0.3
        };
        self.last = Instant::now();
        self.last_done = done;
        Some(self.rate as u64)
    }
}

/// Container extension from the URL path, else Content-Type, else `mp4`.
pub fn extension_for(url_path: &str, content_type: &str) -> &'static str {
    let last = url_path.rsplit('/').next().unwrap_or("");
    if let Some((_, ext)) = last.rsplit_once('.') {
        let ext = ext.to_ascii_lowercase();
        if let Some(k) = KNOWN_EXTENSIONS.iter().find(|k| **k == ext) {
            return k;
        }
    }
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("matroska") {
        "mkv"
    } else if ct.contains("mp2t") {
        "ts"
    } else if ct.contains("webm") {
        "webm"
    } else if ct.contains("x-msvideo") {
        "avi"
    } else if ct.contains("quicktime") {
        "mov"
    } else {
        "mp4"
    }
}

/// Title → portable file stem: no path separators, reserved or control chars,
/// no leading dots, bounded length. `%` goes too: `file://` URLs of the result
/// must not need percent-encoding (see [`local_play_url`]).
pub fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '%' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let collapsed = cleaned
        .split_whitespace()
        .filter(|w| !w.chars().all(|c| c == '.'))
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed = collapsed.trim_start_matches('.').trim_end_matches(['.', ' ']);
    let mut out: String = trimmed.chars().take(150).collect();
    if out.is_empty() {
        out = "FluxPlay".into();
    }
    out
}

/// `Série — S01E02 — Titre`. Panel titles often already carry `S01E02` (and
/// the series name); keep those as-is instead of doubling the tag.
pub fn episode_title(series: &str, season: u32, episode: u32, title: &str) -> String {
    let tag = format!("S{season:02}E{episode:02}");
    let title = title.trim();
    if title.to_ascii_uppercase().contains(&tag) {
        if title.contains(series.trim()) {
            title.to_string()
        } else {
            format!("{series} — {title}")
        }
    } else if title.is_empty() {
        format!("{series} — {tag}")
    } else {
        format!("{series} — {tag} — {title}")
    }
}

/// `1,4 Go` / `350 Mo` — French short units.
pub fn human_size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} Go", b / GB).replace('.', ",")
    } else {
        format!("{:.0} Mo", b / MB)
    }
}

/// Short progress text for a running download (`42 %` or `350 Mo`).
pub fn progress_text(done: u64, total: Option<u64>) -> String {
    match total.filter(|t| *t > 0) {
        Some(t) => format!("{} %", (done.saturating_mul(100) / t).min(100)),
        None => human_size(done),
    }
}

/// Status-bar detail: `42 % · 1,8 Mo/s · 12 min`.
pub fn progress_detail(done: u64, total: Option<u64>, rate: u64) -> String {
    let mut s = progress_text(done, total);
    if rate > 0 {
        let mb = rate as f64 / (1024.0 * 1024.0);
        s.push_str(&format!(" · {:.1} Mo/s", mb).replace('.', ","));
        if let Some(t) = total.filter(|t| *t > done) {
            s.push_str(" · ");
            s.push_str(&eta_text((t - done) / rate.max(1)));
        }
    }
    s
}

fn eta_text(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs} s"),
        60..=3599 => format!("{} min", secs.div_ceil(60)),
        _ => format!("{} h {:02}", secs / 3600, (secs % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    const FAST: RetryPolicy = RetryPolicy {
        max_attempts: 4,
        base: Duration::from_millis(10),
        cap: Duration::from_millis(40),
    };

    #[test]
    fn sanitize_strips_separators_and_reserved() {
        assert_eq!(
            sanitize_file_name("Série: S01/E02 — \"Pilote\"?"),
            "Série S01 E02 — Pilote"
        );
        assert_eq!(sanitize_file_name("../../etc/passwd"), "etc passwd");
        assert_eq!(sanitize_file_name("..."), "FluxPlay");
        assert_eq!(sanitize_file_name("a\u{0}b\nc"), "a b c");
        assert_eq!(sanitize_file_name(&"x".repeat(400)).chars().count(), 150);
        assert_eq!(sanitize_file_name("100% Loup"), "100 Loup");
    }

    #[test]
    fn library_survives_reload_and_forgets_deleted_files() {
        let dir = scratch_dir("library");
        std::fs::create_dir_all(&dir).unwrap();
        let film = dir.join("Vaiana 2.mkv");
        let episode = dir.join("Dark — S01E02 — Mensonges.mp4");
        std::fs::write(&film, b"x").unwrap();
        std::fs::write(&episode, b"x").unwrap();
        let index = dir.join("downloads.index");
        let film_url = "http://panel.example/movie/user/secret/1.mkv";
        let ep_url = "http://panel.example/series/user/secret/2.mp4";

        let mut lib = Library::load(index.clone());
        lib.insert(film_url, film.clone());
        lib.insert(ep_url, episode.clone());
        let on_disk = std::fs::read_to_string(&index).unwrap();
        assert!(!on_disk.contains("secret"), "no URL / credentials on disk");

        std::fs::remove_file(&episode).unwrap();
        let lib = Library::load(index);
        assert_eq!(lib.get(film_url), Some(film.as_path()));
        assert!(!lib.contains(ep_url));
        assert!(lib.get("http://panel.example/movie/user/secret/3.mkv").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn finds_downloads_by_title_and_builds_file_urls() {
        let dir = scratch_dir("byname");
        std::fs::create_dir_all(&dir).unwrap();
        let film = dir.join("Vaiana 2.mkv");
        std::fs::write(&film, b"x").unwrap();
        std::fs::write(dir.join("Wish.mp4.part"), b"x").unwrap();
        assert_eq!(find_by_name(&dir, "Vaiana 2"), Some(film.clone()));
        assert_eq!(find_by_name(&dir, "Wish"), None, "unfinished .part is not a download");
        assert_eq!(
            local_play_url(&film).as_deref(),
            Some(format!("file://{}", film.display()).as_str())
        );
        let odd = dir.join("50%.mkv");
        std::fs::write(&odd, b"x").unwrap();
        assert_eq!(local_play_url(&odd), None);
        assert_eq!(local_play_url(&dir.join("missing.mkv")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extension_prefers_url_then_content_type() {
        assert_eq!(extension_for("/movie/u/p/123.mkv", ""), "mkv");
        assert_eq!(extension_for("/movie/u/p/123.MP4", "video/x-matroska"), "mp4");
        assert_eq!(extension_for("/movie/u/p/123", "video/x-matroska"), "mkv");
        assert_eq!(extension_for("/a.b.c/123", "video/mp2t"), "ts");
        assert_eq!(extension_for("/stream", ""), "mp4");
        assert_eq!(extension_for("/x.php", ""), "mp4");
    }

    #[test]
    fn episode_titles_do_not_double_tags() {
        assert_eq!(episode_title("Dark", 1, 2, "Mensonges"), "Dark — S01E02 — Mensonges");
        assert_eq!(episode_title("Dark", 1, 2, "Dark - S01E02"), "Dark - S01E02");
        assert_eq!(episode_title("Dark", 1, 2, "s01e02 Mensonges"), "Dark — s01e02 Mensonges");
        assert_eq!(episode_title("Dark", 3, 10, " "), "Dark — S03E10");
    }

    #[test]
    fn progress_and_sizes() {
        assert_eq!(progress_text(50, Some(200)), "25 %");
        assert_eq!(progress_text(300, Some(200)), "100 %");
        assert_eq!(progress_text(350 * 1024 * 1024, None), "350 Mo");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024 / 2), "1,5 Go");
        let mb = 1024 * 1024;
        assert_eq!(
            progress_detail(100 * mb, Some(400 * mb), 2 * mb),
            "25 % · 2,0 Mo/s · 3 min"
        );
        assert_eq!(progress_detail(10, Some(100), 0), "10 %");
        assert_eq!(eta_text(3 * 3600 + 5 * 60), "3 h 05");
    }

    #[test]
    fn content_range_parsing() {
        assert_eq!(parse_content_range("bytes 100-199/1000"), Some((100, Some(1000))));
        assert_eq!(parse_content_range("bytes 5-9/*"), Some((5, None)));
        assert_eq!(parse_content_range("items 1-2/3"), None);
    }

    #[test]
    fn url_key_hides_credentials() {
        let k = url_key("http://h/movie/user/secret/1.mkv");
        assert_eq!(k.len(), 32);
        assert!(!k.contains("secret"));
        assert_eq!(k, url_key(" http://h/movie/user/secret/1.mkv "));
        assert_ne!(k, url_key("http://h/movie/user/secret/2.mkv"));
    }

    #[test]
    fn retry_delay_backs_off_and_caps() {
        let p = RetryPolicy::DEFAULT;
        assert_eq!(p.delay(1), Duration::from_secs(2));
        assert_eq!(p.delay(2), Duration::from_secs(4));
        assert_eq!(p.delay(4), Duration::from_secs(16));
        assert_eq!(p.delay(9), Duration::from_secs(30));
    }

    /// How the test server answers one connection.
    #[derive(Clone, Copy)]
    enum Reply {
        /// Honour `Range`; stop after `cut` bytes of the file (connection drop).
        Serve { cut: Option<usize> },
        /// Ignore `Range` and send the whole file with 200.
        IgnoreRange,
        Status(u16),
    }

    struct Server {
        url: String,
        requests: Arc<Mutex<Vec<String>>>,
    }

    fn serve(body: Vec<u8>, script: Vec<Reply>) -> Server {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&requests);
        std::thread::spawn(move || {
            for reply in script {
                let Ok((mut s, _)) = listener.accept() else {
                    return;
                };
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => req.extend_from_slice(&buf[..n]),
                    }
                }
                let req = String::from_utf8_lossy(&req).to_ascii_lowercase();
                log.lock().unwrap().push(req.clone());
                let range_start = req
                    .lines()
                    .find_map(|l| l.strip_prefix("range: bytes="))
                    .and_then(|r| r.trim().trim_end_matches('-').parse::<usize>().ok());
                let len = body.len();
                match reply {
                    Reply::Status(code) => {
                        let _ = write!(
                            s,
                            "HTTP/1.1 {code} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                    }
                    Reply::IgnoreRange => {
                        let _ = write!(
                            s,
                            "HTTP/1.1 200 OK\r\nContent-Type: video/x-matroska\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
                        );
                        let _ = s.write_all(&body);
                    }
                    Reply::Serve { cut } => {
                        let start = range_start.unwrap_or(0).min(len);
                        let end = cut.map_or(len, |c| c.clamp(start, len));
                        let head = if range_start.is_some() {
                            format!(
                                "HTTP/1.1 206 Partial Content\r\nContent-Type: video/x-matroska\r\nContent-Range: bytes {start}-{}/{len}\r\nContent-Length: {}\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n",
                                len - 1,
                                len - start
                            )
                        } else {
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: video/x-matroska\r\nContent-Length: {len}\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n"
                            )
                        };
                        let _ = s.write_all(head.as_bytes());
                        let _ = s.write_all(&body[start..end]);
                    }
                }
            }
        });
        Server {
            url: format!("http://{addr}/movie/u/p/42"),
            requests,
        }
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fluxplay-dl-test-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn body(n: u32) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8).collect()
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".part") || n.ends_with(".meta"))
            .collect()
    }

    #[tokio::test]
    async fn downloads_to_named_file_and_never_overwrites() {
        let body = body(300_000);
        let srv = serve(
            body.clone(),
            vec![Reply::Serve { cut: None }, Reply::Serve { cut: None }],
        );
        let dir = scratch_dir("ok");
        let (mut tx, _rx) = mpsc::channel(256);

        let first = download_into(&dir, "Vaiana 2: le film", &plan1(&srv.url), "UA", None, FAST, &mut tx)
            .await
            .unwrap();
        assert_eq!(first, dir.join("Vaiana 2 le film.mkv"));
        assert_eq!(std::fs::read(&first).unwrap(), body);

        let second = download_into(&dir, "Vaiana 2: le film", &plan1(&srv.url), "UA", None, FAST, &mut tx)
            .await
            .unwrap();
        assert_eq!(second, dir.join("Vaiana 2 le film (2).mkv"));
        assert_eq!(std::fs::read(&first).unwrap(), body);
        assert!(leftovers(&dir).is_empty());
        let reqs = srv.requests.lock().unwrap();
        assert!(reqs.iter().all(|r| !r.contains("accept-encoding: gzip")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn reconnects_and_resumes_after_a_drop() {
        let body = body(400_000);
        let srv = serve(
            body.clone(),
            vec![Reply::Serve { cut: Some(150_000) }, Reply::Serve { cut: None }],
        );
        let dir = scratch_dir("resume");
        let (mut tx, _rx) = mpsc::channel(256);

        let path = download_into(&dir, "Coupé", &plan1(&srv.url), "UA", None, FAST, &mut tx)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), body);
        let reqs = srv.requests.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        assert!(reqs[1].contains("range: bytes=150000-"));
        assert!(reqs[1].contains("if-range: \"v1\""));
        assert!(leftovers(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn gives_up_keeps_part_then_resumes_on_next_click() {
        let body = body(300_000);
        let no_retry = RetryPolicy {
            max_attempts: 0,
            ..FAST
        };
        let srv = serve(
            body.clone(),
            vec![Reply::Serve { cut: Some(100_000) }, Reply::Serve { cut: None }],
        );
        let dir = scratch_dir("later");
        let (mut tx, _rx) = mpsc::channel(256);

        assert!(download_into(&dir, "Plus tard", &plan1(&srv.url), "UA", None, no_retry, &mut tx)
            .await
            .is_err());
        let partials = scan_partials(&dir);
        assert_eq!(
            partials.get(&url_key(&srv.url)),
            Some(&Partial {
                done: 100_000,
                total: Some(300_000)
            })
        );

        let path = download_into(&dir, "Plus tard", &plan1(&srv.url), "UA", None, FAST, &mut tx)
            .await
            .unwrap();
        assert_eq!(path, dir.join("Plus tard.mkv"));
        assert_eq!(std::fs::read(&path).unwrap(), body);
        assert!(leftovers(&dir).is_empty());
        assert!(srv.requests.lock().unwrap()[1].contains("range: bytes=100000-"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn restarts_cleanly_when_server_ignores_range() {
        let body = body(200_000);
        let srv = serve(
            body.clone(),
            vec![Reply::Serve { cut: Some(50_000) }, Reply::IgnoreRange],
        );
        let dir = scratch_dir("norange");
        let (mut tx, _rx) = mpsc::channel(256);

        let path = download_into(&dir, "Sans plage", &plan1(&srv.url), "UA", None, FAST, &mut tx)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn adopts_an_idle_legacy_part_of_the_same_title() {
        let body = body(250_000);
        let srv = serve(body.clone(), vec![Reply::Serve { cut: None }]);
        let dir = scratch_dir("legacy");
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("Ancien.mkv.part");
        std::fs::write(&part, &body[..80_000]).unwrap();
        let old = SystemTime::now() - Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&part)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let (mut tx, _rx) = mpsc::channel(256);

        let path = download_into(&dir, "Ancien", &plan1(&srv.url), "UA", None, FAST, &mut tx)
            .await
            .unwrap();
        assert_eq!(path, dir.join("Ancien.mkv"));
        assert_eq!(std::fs::read(&path).unwrap(), body);
        assert!(srv.requests.lock().unwrap()[0].contains("range: bytes=80000-"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn missing_file_fails_at_once_without_leftovers() {
        let srv = serve(Vec::new(), vec![Reply::Status(404), Reply::Status(404)]);
        let dir = scratch_dir("404");
        let (mut tx, _rx) = mpsc::channel(256);

        let res = download_into(&dir, "Absent", &plan1(&srv.url), "UA", None, FAST, &mut tx).await;
        assert_eq!(res.unwrap_err(), "HTTP 404 Not Found");
        assert_eq!(srv.requests.lock().unwrap().len(), 1);
        assert!(leftovers(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn busy_panel_slot_is_retried() {
        let body = body(120_000);
        let srv = serve(
            body.clone(),
            vec![Reply::Status(509), Reply::Serve { cut: None }],
        );
        let dir = scratch_dir("busy");
        let (mut tx, _rx) = mpsc::channel(256);

        let path = download_into(&dir, "Occupé", &plan1(&srv.url), "UA", None, FAST, &mut tx)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn second_window_cannot_append_to_a_locked_part() {
        let dir = scratch_dir("lock");
        std::fs::create_dir_all(&dir).unwrap();
        let keys = url_keys("http://x/1");
        let held = create_destination(&dir, "Verrou", "mkv", &keys[0]).await.unwrap();
        held.save_meta();
        let err = find_resumable(&dir, &keys, "Verrou").err().unwrap();
        assert!(err.contains("autre fenêtre"));
        drop(held);
        assert!(find_resumable(&dir, &keys, "Verrou").unwrap().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn plan1(url: &str) -> Plan {
        plan_with(vec![url.to_string()], 1, parallel::Tuning::DEFAULT)
    }

    fn plan_with(urls: Vec<String>, connections: usize, tuning: parallel::Tuning) -> Plan {
        Plan {
            links: Arc::new(Links::new(urls, Duration::from_millis(1))),
            account: Account::detached(connections, connections),
            tuning,
        }
    }

    const SMALL: parallel::Tuning = parallel::Tuning {
        segmented_min: 0,
        min_split: 64 * 1024,
        max_span: 512 * 1024,
        checkpoint: Duration::from_millis(50),
        server_backoff: Duration::from_millis(5),
        probe_every: Duration::from_secs(60),
        recycle_window: Duration::from_secs(3),
        recycle_age: Duration::from_secs(3600),
    };

    /// Concurrent range server: one thread per connection, slow enough that
    /// connections overlap. Beyond `cap` simultaneous connections it answers 509.
    struct RangeServer {
        url: String,
        ranges: Arc<Mutex<Vec<String>>>,
        peak: Arc<std::sync::atomic::AtomicUsize>,
    }

    fn range_server(body: Arc<Vec<u8>>, cap: usize) -> RangeServer {
        range_server_paced(body, cap, Duration::from_millis(2))
    }

    /// `pause` after every 32 KiB sent: 20 ms ≈ 1.6 MB/s per connection.
    fn range_server_paced(body: Arc<Vec<u8>>, cap: usize, pause: Duration) -> RangeServer {
        range_server_gated(body, cap, Arc::new(move |_| pause), Arc::new(|_: &str| true))
    }

    /// Pause after each 32 KiB piece, given the bytes this connection already sent.
    type Pace = Arc<dyn Fn(usize) -> Duration + Send + Sync>;

    /// Like [`range_server_paced`], answering 509 when `admit` rejects the
    /// request path.
    fn range_server_gated(
        body: Arc<Vec<u8>>,
        cap: usize,
        pace: Pace,
        admit: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    ) -> RangeServer {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let peak = Arc::new(AtomicUsize::new(0));
        let live = Arc::new(AtomicUsize::new(0));
        let (log, top) = (Arc::clone(&ranges), Arc::clone(&peak));
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut s) = conn else { return };
                let (body, log, top, live, admit, pace) = (
                    Arc::clone(&body),
                    Arc::clone(&log),
                    Arc::clone(&top),
                    Arc::clone(&live),
                    Arc::clone(&admit),
                    Arc::clone(&pace),
                );
                std::thread::spawn(move || {
                    let mut req = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                        match s.read(&mut buf) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => req.extend_from_slice(&buf[..n]),
                        }
                    }
                    let req = String::from_utf8_lossy(&req).to_ascii_lowercase();
                    let path = req.split_whitespace().nth(1).unwrap_or("");
                    if !admit(path) {
                        let _ = write!(s, "HTTP/1.1 509 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        return;
                    }
                    let range = req
                        .lines()
                        .find_map(|l| l.strip_prefix("range: bytes="))
                        .unwrap_or("0-")
                        .trim()
                        .to_string();
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    if now > cap {
                        live.fetch_sub(1, Ordering::SeqCst);
                        let _ = write!(s, "HTTP/1.1 509 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        return;
                    }
                    top.fetch_max(now, Ordering::SeqCst);
                    log.lock().unwrap().push(range.clone());
                    let len = body.len();
                    let (a, b) = range.split_once('-').unwrap();
                    let start: usize = a.parse().unwrap();
                    let end: usize = b.parse::<usize>().map_or(len, |e| e + 1).min(len);
                    let head = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Type: video/x-matroska\r\nContent-Range: bytes {start}-{}/{len}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        end - 1,
                        end - start
                    );
                    if s.write_all(head.as_bytes()).is_ok() {
                        let mut sent = 0;
                        for piece in body[start..end].chunks(32 * 1024) {
                            if s.write_all(piece).is_err() {
                                break;
                            }
                            sent += piece.len();
                            std::thread::sleep(pace(sent));
                        }
                    }
                    live.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        RangeServer {
            url: format!("http://{addr}/movie/u/p/7.mkv"),
            ranges,
            peak,
        }
    }

    #[tokio::test]
    async fn parallel_download_uses_several_connections_of_one_server() {
        use std::sync::atomic::Ordering;
        let body = Arc::new(body(3_000_000));
        let a = range_server(Arc::clone(&body), 8);
        let b = range_server(Arc::clone(&body), 8);
        let dir = scratch_dir("parallel");
        let (mut tx, _rx) = mpsc::channel(1024);
        let plan = plan_with(vec![a.url.clone(), b.url.clone()], 4, SMALL);

        let path = download_into(&dir, "Parallèle", &plan, "UA", None, FAST, &mut tx)
            .await
            .unwrap();
        assert_eq!(path, dir.join("Parallèle.mkv"));
        assert!(std::fs::read(&path).unwrap() == *body, "bytes identical");
        assert!(leftovers(&dir).is_empty());
        assert!(a.peak.load(Ordering::SeqCst) >= 3);
        assert!(b.ranges.lock().unwrap().is_empty(), "a mirror would be one more stream");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_failing_server_hands_over_to_the_next() {
        let body = Arc::new(body(2_000_000));
        let a = range_server(Arc::clone(&body), 0);
        let b = range_server(Arc::clone(&body), 8);
        let dir = scratch_dir("failover");
        let (mut tx, _rx) = mpsc::channel(1024);
        let plan = plan_with(vec![a.url.clone(), b.url.clone()], 3, SMALL);

        let path = download_into(&dir, "Relais", &plan, "UA", None, FAST, &mut tx)
            .await
            .unwrap();
        assert!(std::fs::read(&path).unwrap() == *body);
        assert!(!b.ranges.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// XUI-style panel: every media request is redirected to the node with a
    /// new token, and the node only honours the latest token issued.
    struct XuiPanel {
        url: String,
        tokens: Arc<std::sync::atomic::AtomicUsize>,
        node: RangeServer,
    }

    fn xui_panel(body: Arc<Vec<u8>>, pace: Pace) -> XuiPanel {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let tokens = Arc::new(AtomicUsize::new(0));
        let latest = Arc::clone(&tokens);
        let node = range_server_gated(
            body,
            16,
            pace,
            Arc::new(move |path: &str| {
                let n = latest.load(Ordering::SeqCst);
                path.split('/').any(|seg| seg == format!("tok{n}"))
            }),
        );
        let node_base = node.url.split("/movie/").next().unwrap().to_string();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let issued = Arc::clone(&tokens);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut s) = conn else { return };
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => req.extend_from_slice(&buf[..n]),
                    }
                }
                let n = issued.fetch_add(1, Ordering::SeqCst) + 1;
                let _ = write!(
                    s,
                    "HTTP/1.1 302 Found\r\nLocation: {node_base}/live/play/tok{n}/7\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
            }
        });
        XuiPanel {
            url: format!("http://{addr}/movie/u/p/7.mkv"),
            tokens,
            node,
        }
    }

    #[tokio::test]
    async fn connections_share_the_stream_token_of_the_panel() {
        use std::sync::atomic::Ordering;
        let body = Arc::new(body(4_000_000));
        let panel = xui_panel(Arc::clone(&body), Arc::new(|_| Duration::from_millis(5)));
        let dir = scratch_dir("xui");
        let (mut tx, _rx) = mpsc::channel(4096);
        let plan = plan_with(vec![panel.url.clone()], 4, SMALL);

        let path = download_into(&dir, "Jeton", &plan, "UA", None, FAST, &mut tx)
            .await
            .unwrap();
        assert!(std::fs::read(&path).unwrap() == *body);
        assert_eq!(panel.tokens.load(Ordering::SeqCst), 1, "one token for every connection and span");
        assert!(panel.node.peak.load(Ordering::SeqCst) >= 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn throttled_connections_reopen_on_the_same_token() {
        use std::sync::atomic::Ordering;
        let body = Arc::new(body(3_000_000));
        // Busy node: 256 KiB at full speed per connection, then ~160 KB/s.
        let panel = xui_panel(
            Arc::clone(&body),
            Arc::new(|sent| Duration::from_millis(if sent < 256 * 1024 { 1 } else { 200 })),
        );
        let dir = scratch_dir("recycle");
        let (mut tx, _rx) = mpsc::channel(4096);
        let tuning = parallel::Tuning {
            max_span: 64 * 1024 * 1024,
            recycle_window: Duration::from_millis(100),
            recycle_age: Duration::from_millis(150),
            ..SMALL
        };
        let plan = plan_with(vec![panel.url.clone()], 2, tuning);

        let started = Instant::now();
        let path = download_into(&dir, "Recyclé", &plan, "UA", None, FAST, &mut tx)
            .await
            .unwrap();
        assert!(std::fs::read(&path).unwrap() == *body);
        // Without reopening: ~1.4 MB per connection at 160 KB/s, ~9 s.
        assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
        assert!(panel.node.ranges.lock().unwrap().len() > 4, "connections reopened");
        assert_eq!(panel.tokens.load(Ordering::SeqCst), 1, "on the same token");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_revoked_token_is_renewed_once() {
        use std::sync::atomic::Ordering;
        let body = Arc::new(body(4_000_000));
        let panel = xui_panel(Arc::clone(&body), Arc::new(|_| Duration::from_millis(20)));
        let dir = scratch_dir("xui-revoked");
        let (mut tx, _rx) = mpsc::channel(4096);
        let plan = plan_with(vec![panel.url.clone()], 4, SMALL);
        let tokens = Arc::clone(&panel.tokens);
        // Another stream of the account (playback) takes the token over.
        let revoke = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            tokens.fetch_add(1, Ordering::SeqCst);
        });

        let path = download_into(&dir, "Révoqué", &plan, "UA", None, FAST, &mut tx)
            .await
            .unwrap();
        revoke.await.unwrap();
        assert!(std::fs::read(&path).unwrap() == *body);
        assert_eq!(
            panel.tokens.load(Ordering::SeqCst),
            3,
            "first token, the other stream's, one renewal shared by every connection"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn probing_adds_connections_until_the_panel_refuses() {
        use std::sync::atomic::Ordering;
        let body = Arc::new(body(8_000_000));
        let srv = range_server_paced(Arc::clone(&body), 3, Duration::from_millis(20));
        let dir = scratch_dir("probe");
        let (mut tx, _rx) = mpsc::channel(4096);
        let tuning = parallel::Tuning {
            max_span: 1024 * 1024,
            probe_every: Duration::from_millis(400),
            ..SMALL
        };
        let mut plan = plan_with(vec![srv.url.clone()], 1, tuning);
        plan.account = Account::detached(1, 6);

        let path = download_into(&dir, "Sonde", &plan, "UA", None, FAST, &mut tx)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), *body);
        let peak = srv.peak.load(Ordering::SeqCst);
        assert!((2..=3).contains(&peak), "peak {peak}: grew from 1, never past the panel's 3");
        let a = &plan.account;
        assert!(a.cap() < 3, "the refused trial slot was given back (cap {})", a.cap());
        assert!(a.served() >= 2, "served {} peak {peak}", a.served());
    }

    #[tokio::test]
    async fn probing_reaches_the_ceiling_when_the_announced_limit_is_not_enforced() {
        use std::sync::atomic::Ordering;
        let body = Arc::new(body(12_000_000));
        let srv = range_server_paced(Arc::clone(&body), 16, Duration::from_millis(20));
        let dir = scratch_dir("probe-open");
        let (mut tx, _rx) = mpsc::channel(4096);
        let tuning = parallel::Tuning {
            max_span: 64 * 1024 * 1024,
            probe_every: Duration::from_millis(400),
            ..SMALL
        };
        let mut plan = plan_with(vec![srv.url.clone()], 1, tuning);
        plan.account = Account::detached(1, 4);

        let path = download_into(&dir, "Ouvert", &plan, "UA", None, FAST, &mut tx)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), *body);
        // The server still counts a connection for a moment after the client drops it.
        assert!(srv.peak.load(Ordering::SeqCst) >= 4, "grew from 1 to the ceiling");
        let a = &plan.account;
        assert_eq!(a.ceiling(), 4, "nothing refused");
        assert!(a.served() >= 3, "served {}", a.served());
    }

    #[tokio::test]
    async fn parallel_resume_fetches_only_the_holes() {
        let body = Arc::new(body(2_000_000));
        let srv = range_server(Arc::clone(&body), 8);
        let dir = scratch_dir("holes");
        std::fs::create_dir_all(&dir).unwrap();
        // A crashed segmented download: [0, 700000) and [1200000, 2000000) on
        // disk (marked, to see they are never rewritten), the middle zeros.
        let mut partial: Vec<u8> = body.iter().map(|b| b ^ 0x55).collect();
        partial[700_000..1_200_000].fill(0);
        let part = dir.join("Trous.mkv.part");
        std::fs::write(&part, &partial).unwrap();
        let meta = Meta {
            key: url_key(&srv.url),
            total: Some(2_000_000),
            todo: Some(vec![(700_000, 1_200_000)]),
            ..Meta::default()
        };
        meta.write(&meta_path(&part)).unwrap();
        assert_eq!(
            scan_partials(&dir).get(&url_key(&srv.url)),
            Some(&Partial {
                done: 1_500_000,
                total: Some(2_000_000)
            })
        );
        let (mut tx, _rx) = mpsc::channel(1024);
        let plan = plan_with(vec![srv.url.clone()], 3, SMALL);

        let path = download_into(&dir, "Trous", &plan, "UA", None, FAST, &mut tx)
            .await
            .unwrap();
        let mut expected = partial.clone();
        expected[700_000..1_200_000].copy_from_slice(&body[700_000..1_200_000]);
        assert!(std::fs::read(&path).unwrap() == expected, "only the hole is written");
        for r in srv.ranges.lock().unwrap().iter() {
            let start: u64 = r.split_once('-').unwrap().0.parse().unwrap();
            assert!((700_000..1_200_000).contains(&start), "only the hole is requested: {r}");
        }
        assert!(leftovers(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn refused_connections_lower_the_count() {
        let body = Arc::new(body(2_500_000));
        let srv = range_server(Arc::clone(&body), 2);
        let dir = scratch_dir("refused");
        let (mut tx, _rx) = mpsc::channel(1024);
        let plan = plan_with(vec![srv.url.clone()], 5, SMALL);

        let path = download_into(&dir, "Limite", &plan, "UA", None, FAST, &mut tx)
            .await
            .unwrap();
        assert!(std::fs::read(&path).unwrap() == *body);
        assert!(srv.peak.load(std::sync::atomic::Ordering::SeqCst) <= 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_ignore_the_server_of_xtream_media() {
        let a = "http://a.example/movie/u/p/1.mkv";
        let b = "https://b.example:8443/movie/u/p/1.mkv";
        assert_eq!(url_key(a), url_key(b));
        assert_ne!(url_key(a), url_key("http://a.example/movie/u/p/2.mkv"));
        assert_ne!(url_key("http://a.example/x.mp4"), url_key("http://b.example/x.mp4"));
        assert_eq!(url_keys("http://a.example/x.mp4").len(), 1);

        let dir = scratch_dir("legacykey");
        std::fs::create_dir_all(&dir).unwrap();
        let film = dir.join("Film.mkv");
        std::fs::write(&film, b"x").unwrap();
        let index = dir.join("downloads.index");
        std::fs::write(
            &index,
            format!("{LIBRARY_MAGIC}\n{}\t{}\n", legacy_url_key(a), film.display()),
        )
        .unwrap();
        let mut lib = Library::load(index.clone());
        assert_eq!(lib.get(b), None, "legacy key is host-bound");
        assert_eq!(lib.get(a), Some(film.as_path()));
        lib.insert(a, film.clone());
        assert_eq!(Library::load(index).get(b), Some(film.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn segmented_meta_round_trips_and_rejects_bad_holes() {
        let dir = scratch_dir("meta2");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.part.meta");
        let meta = Meta {
            key: "k".into(),
            total: Some(100),
            etag: Some("\"e\"".into()),
            last_modified: None,
            todo: Some(vec![(10, 20), (50, 100)]),
        };
        meta.write(&path).unwrap();
        let back = Meta::read(&path).unwrap();
        assert_eq!(back, meta);
        assert_eq!(back.done(), Some(40));
        std::fs::write(&path, "fluxplay-download 2\nkey=k\ntotal=100\ntodo=90-120\n").unwrap();
        assert!(Meta::read(&path).is_none());
        std::fs::write(&path, "fluxplay-download 2\nkey=k\ntotal=100\ntodo=\n").unwrap();
        assert_eq!(Meta::read(&path).unwrap().done(), Some(100));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Live check against the first profile of `state.json` (never run by CI):
    /// `FLUXPLAY_DIAG_TITLE=Matrix cargo test -p fluxplay --lib live_panel -- --ignored --nocapture`.
    /// Prints rate and connection count only — no URL.
    #[tokio::test]
    #[ignore]
    async fn live_panel_connection_probing() {
        use iced::futures::StreamExt;
        let _ = tracing_subscriber::fmt()
            .with_env_filter("fluxplay::downloads=debug")
            .with_test_writer()
            .try_init();
        let state = crate::storage::load();
        let source = state.sources.into_iter().find(|s| s.enabled).expect("a profile");
        let title = std::env::var("FLUXPLAY_DIAG_TITLE").unwrap_or_else(|_| "Matrix".into());
        let db = crate::storage::data_dir()
            .join("profiles")
            .join(source.id.to_string())
            .join("catalog.sqlite3");
        let conn = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let payload: String = conn
            .query_row(
                "SELECT payload FROM vod WHERE name LIKE ?1 LIMIT 1",
                [format!("%{title}%")],
                |r| r.get(0),
            )
            .unwrap();
        let url = serde_json::from_str::<serde_json::Value>(&payload).unwrap()["stream_url"]
            .as_str()
            .unwrap()
            .to_string();
        let dir = std::env::temp_dir().join("fp-live-diag");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let events = run(DownloadRequest {
            name: "diag".into(),
            url,
            source: Some(source),
            user_agent: "VLC/3.0.20 LibVLC/3.0.20".into(),
            dir: dir.clone(),
            background: false,
        });
        tokio::pin!(events);
        let started = Instant::now();
        let mut last_print = Instant::now();
        while started.elapsed() < Duration::from_secs(
            std::env::var("FLUXPLAY_DIAG_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(70),
        ) {
            match tokio::time::timeout(Duration::from_secs(5), events.next()).await {
                Ok(Some(DownloadEvent::Progress { done, rate, connections, .. })) => {
                    if last_print.elapsed() >= Duration::from_secs(4) {
                        last_print = Instant::now();
                        eprintln!(
                            "t={:>3}s  {:>6.1} Mo  {:>5.2} Mo/s  {connections} connexion(s)",
                            started.elapsed().as_secs(),
                            done as f64 / 1e6,
                            rate as f64 / 1e6
                        );
                    }
                }
                Ok(Some(DownloadEvent::Retrying { attempt, error, .. })) => {
                    eprintln!("retry {attempt}: {error}");
                }
                Ok(Some(DownloadEvent::Finished(r))) => {
                    eprintln!("finished: {:?}", r.map(|_| ()));
                    break;
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => eprintln!("(no event for 5 s)"),
            }
        }
        drop(events);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn streams_keep_one_for_playback_and_connections_share_them() {
        assert_eq!(streams_for(None), MAX_PARALLEL);
        assert_eq!(streams_for(Some(1)), 1);
        assert_eq!(streams_for(Some(2)), 1);
        assert_eq!(streams_for(Some(4)), 3);
        assert_eq!(connections_for(None), DEFAULT_CONNECTIONS);
        assert_eq!(connections_for(Some(1)), DEFAULT_CONNECTIONS);
        assert_eq!(connections_for(Some(4)), 4);
        assert_eq!(connections_for(Some(100)), MAX_CONNECTIONS);
    }
}
