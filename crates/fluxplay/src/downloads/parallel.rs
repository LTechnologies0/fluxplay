//! Several ranged connections filling one preallocated `.part`.
//!
//! The missing bytes are a list of spans. A connection owns one span at a
//! time and writes at its offset (positional writes, no shared cursor); an
//! idle connection takes an unowned span, or splits the largest owned one in
//! two — the owner simply stops at the new end. Spans are capped at
//! [`Tuning::max_span`] so connection slots rotate between concurrent
//! downloads of the same account.
//!
//! Every connection holds one of the account's slots ([`Account`]); the file
//! keeps connections waiting for more, which is what lets the account try
//! one more slot.
//!
//! Progress reaches the sidecar only after `sync_data`: the holes are
//! snapshotted, the file synced, then the sidecar replaced atomically. A crash
//! can lose recent progress (re-downloaded), never mark unwritten bytes done.

use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::task::JoinSet;

use super::*;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Tuning {
    /// Remaining bytes below this stay on one connection.
    pub segmented_min: u64,
    /// Never split a span into pieces smaller than this.
    pub min_split: u64,
    /// Longest span one connection takes before giving its slot back.
    pub max_span: u64,
    /// How often synced progress is written to the sidecar.
    pub checkpoint: Duration,
    /// First wait before reusing a server that failed.
    pub server_backoff: Duration,
    /// How often the account settles a trial slot or tries one more.
    pub probe_every: Duration,
}

impl Tuning {
    pub const DEFAULT: Self = Self {
        segmented_min: 32 * 1024 * 1024,
        min_split: 8 * 1024 * 1024,
        max_span: 256 * 1024 * 1024,
        checkpoint: Duration::from_secs(5),
        server_backoff: Duration::from_secs(3),
        probe_every: Duration::from_secs(8),
    };
}

/// The sequential attempt's `206` answer, handed over as the first connection.
pub(crate) struct FirstResponse {
    pub resp: reqwest::Response,
    pub url_idx: usize,
    pub slot: Slot,
    pub pos: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    id: u64,
    pos: u64,
    end: u64,
    owned: bool,
}

/// Missing byte ranges and who is fetching them.
#[derive(Debug, Default)]
struct Work {
    spans: Vec<Span>,
    next_id: u64,
}

impl Work {
    fn new(holes: &[(u64, u64)]) -> Self {
        let mut w = Work::default();
        for &(pos, end) in holes.iter().filter(|(s, e)| s < e) {
            w.push(pos, end, false);
        }
        w
    }

    fn push(&mut self, pos: u64, end: u64, owned: bool) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.spans.push(Span {
            id,
            pos,
            end,
            owned,
        });
        id
    }

    /// Claim work: the first unowned span (its head, at most `max_span`), or
    /// the back half of the largest owned span.
    fn take(&mut self, t: &Tuning) -> Option<(u64, u64, u64)> {
        if let Some(i) = self
            .spans
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.owned)
            .min_by_key(|(_, s)| s.pos)
            .map(|(i, _)| i)
        {
            let s = self.spans[i];
            let cut = s.pos.saturating_add(t.max_span);
            if cut < s.end {
                self.spans[i].end = cut;
                self.push(cut, s.end, false);
            }
            self.spans[i].owned = true;
            let s = self.spans[i];
            return Some((s.id, s.pos, s.end));
        }
        let i = self
            .spans
            .iter()
            .enumerate()
            .max_by_key(|(_, s)| s.end - s.pos)
            .map(|(i, _)| i)?;
        let s = self.spans[i];
        let left = s.end - s.pos;
        if left < 2 * t.min_split {
            return None;
        }
        let mid = s.pos + left / 2;
        self.spans[i].end = mid;
        let id = self.push(mid, s.end, true);
        Some((id, mid, s.end))
    }

    /// Whether a new connection would find something to do.
    fn has_work(&self, t: &Tuning) -> bool {
        self.spans
            .iter()
            .any(|s| !s.owned || s.end - s.pos >= 2 * t.min_split)
    }

    /// How many of `len` bytes at the span's position still belong to it.
    fn clamp(&self, id: u64, len: u64) -> u64 {
        self.spans
            .iter()
            .find(|s| s.id == id)
            .map_or(0, |s| len.min(s.end - s.pos))
    }

    /// `len` bytes of span `id` are written; `true` once the span is complete.
    fn commit(&mut self, id: u64, len: u64) -> bool {
        let Some(i) = self.spans.iter().position(|s| s.id == id) else {
            return true;
        };
        let s = &mut self.spans[i];
        s.pos = (s.pos + len).min(s.end);
        if s.pos >= s.end {
            self.spans.remove(i);
            return true;
        }
        false
    }

    /// Give span `id` back (its connection failed).
    fn release(&mut self, id: u64) {
        if let Some(s) = self.spans.iter_mut().find(|s| s.id == id) {
            s.owned = false;
        }
    }

    fn holes_of(&self, id: u64) -> Option<(u64, u64)> {
        self.spans
            .iter()
            .find(|s| s.id == id)
            .map(|s| (s.pos, s.end))
    }

    fn holes(&self) -> Vec<(u64, u64)> {
        let mut h: Vec<(u64, u64)> = self.spans.iter().map(|s| (s.pos, s.end)).collect();
        h.sort_unstable();
        h
    }

    fn missing(&self) -> u64 {
        self.spans.iter().map(|s| s.end - s.pos).sum()
    }
}

#[derive(Debug, Default, Clone)]
struct ServerState {
    /// Cannot serve this file (missing, no byte ranges, other size).
    bad: bool,
    active: usize,
    failures: u32,
    retry_at: Option<Instant>,
}

struct Shared {
    total: u64,
    /// Opened without `O_APPEND` (positional writes ignore offsets on an
    /// appending descriptor on Linux).
    file: std::fs::File,
    work: Mutex<Work>,
    servers: Mutex<Vec<ServerState>>,
    account: Arc<Account>,
    /// Connections whose server answered and that are transferring.
    flowing: AtomicUsize,
    last_error: Mutex<Option<String>>,
    /// A server answered 200 to a range request (switch back to sequential).
    lost_ranges: std::sync::atomic::AtomicBool,
    tuning: Tuning,
}

impl Shared {
    fn work(&self) -> std::sync::MutexGuard<'_, Work> {
        self.work.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn servers(&self) -> std::sync::MutexGuard<'_, Vec<ServerState>> {
        self.servers.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn done(&self) -> u64 {
        self.total.saturating_sub(self.work().missing())
    }

    fn note_error(&self, e: String) {
        *self.last_error.lock().unwrap_or_else(|p| p.into_inner()) = Some(e);
    }

    /// Least busy usable server (ranking order breaks ties).
    fn pick_server(&self) -> Option<usize> {
        let now = Instant::now();
        let mut servers = self.servers();
        let i = servers
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.bad && s.retry_at.is_none_or(|t| t <= now))
            .min_by_key(|(i, s)| (s.active, *i))
            .map(|(i, _)| i)?;
        servers[i].active += 1;
        Some(i)
    }

    fn server_available(&self) -> bool {
        let now = Instant::now();
        self.servers()
            .iter()
            .any(|s| !s.bad && s.retry_at.is_none_or(|t| t <= now))
    }

    fn all_bad(&self) -> bool {
        self.servers().iter().all(|s| s.bad)
    }

    fn server_done(&self, i: usize, outcome: ServerOutcome) {
        let mut servers = self.servers();
        let Some(s) = servers.get_mut(i) else {
            return;
        };
        s.active = s.active.saturating_sub(1);
        match outcome {
            ServerOutcome::Ok => {
                s.failures = 0;
                s.retry_at = None;
            }
            ServerOutcome::Failed => {
                s.failures += 1;
                let factor = 1u32 << (s.failures - 1).min(4);
                let wait = self.tuning.server_backoff.saturating_mul(factor);
                s.retry_at = Some(Instant::now() + wait.min(Duration::from_secs(30)));
            }
            ServerOutcome::Bad => s.bad = true,
        }
    }

}

#[derive(Debug, Clone, Copy)]
enum ServerOutcome {
    Ok,
    Failed,
    Bad,
}

/// Decrements [`Shared::flowing`] however the transfer ends.
struct Flowing<'a>(&'a AtomicUsize);

impl Drop for Flowing<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Fill the holes of the segmented `.part` in `d`. `Ok` once none is left;
/// `Retry` when every connection stopped with holes remaining.
pub(crate) async fn run(
    client: &reqwest::Client,
    plan: &Plan,
    d: &mut Dest,
    first: Option<FirstResponse>,
    tx: &mut mpsc::Sender<DownloadEvent>,
) -> Result<(), AttemptError> {
    use AttemptError::{Fatal, Retry};

    let total = d.meta.total.ok_or(Fatal("taille inconnue".into()))?;
    let holes = d.meta.todo.clone().unwrap_or_default();
    if holes.is_empty() {
        return Ok(());
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&d.part)
        .map_err(|e| Fatal(format!("écriture disque : {e}")))?;
    if file.metadata().map(|m| m.len()).unwrap_or(0) < total {
        file.set_len(total)
            .map_err(|e| Fatal(format!("écriture disque : {e}")))?;
    }
    let sh = Arc::new(Shared {
        total,
        file,
        work: Mutex::new(Work::new(&holes)),
        servers: Mutex::new(vec![ServerState::default(); plan.urls.len()]),
        account: Arc::clone(&plan.account),
        flowing: AtomicUsize::new(0),
        last_error: Mutex::new(None),
        lost_ranges: Default::default(),
        tuning: plan.tuning,
    });
    let urls = Arc::new(plan.urls.clone());

    let mut set = JoinSet::new();
    let spawn = |set: &mut JoinSet<Result<(), String>>, handed: Option<Handed>| {
        set.spawn(connection(
            Arc::clone(&sh),
            client.clone(),
            Arc::clone(&urls),
            handed,
        ));
    };
    if let Some(f) = first {
        // Claim its span now, before any other connection can.
        let claimed = sh.work().take(&sh.tuning);
        match claimed {
            Some((id, pos, _)) if pos == f.pos => {
                sh.servers()[f.url_idx].active += 1;
                spawn(
                    &mut set,
                    Some(Handed {
                        resp: f.resp,
                        idx: f.url_idx,
                        slot: f.slot,
                        id,
                    }),
                );
            }
            Some((id, ..)) => sh.work().release(id),
            None => {}
        }
    }

    let mut meter = RateMeter::new(sh.done());
    let _ = tx
        .send(DownloadEvent::Progress {
            done: sh.done(),
            total: Some(total),
            rate: 0,
            connections: 0,
        })
        .await;
    let mut ticker = tokio::time::interval(PROGRESS_EVERY);
    let mut last_checkpoint = Instant::now();

    let result = loop {
        // Connections beyond the account's slots wait for one: that demand is
        // what lets the account try another.
        while set.len() < MAX_CONNECTIONS && sh.work().has_work(&sh.tuning) && sh.server_available() {
            spawn(&mut set, None);
        }
        if set.is_empty() {
            break if sh.work().spans.is_empty() {
                Ok(())
            } else if sh.lost_ranges.load(Ordering::SeqCst) && sh.all_bad() {
                Err(Retry("reprise refusée — redémarrage du fichier".into()))
            } else if sh.all_bad() {
                Err(Fatal(
                    sh.last_error
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .clone()
                        .unwrap_or_else(|| "aucun serveur ne fournit ce fichier".into()),
                ))
            } else {
                let e = sh.last_error.lock().unwrap_or_else(|p| p.into_inner()).clone();
                Err(Retry(e.unwrap_or_else(|| "connexion interrompue".into())))
            };
        }
        tokio::select! {
            joined = set.join_next() => {
                if let Some(Ok(Err(fatal))) = joined {
                    break Err(Fatal(fatal));
                }
            }
            _ = ticker.tick() => {
                let done = sh.done();
                if let Some(rate) = meter.tick(done) {
                    let connections = sh.flowing.load(Ordering::SeqCst);
                    let _ = tx
                        .send(DownloadEvent::Progress { done, total: Some(total), rate, connections })
                        .await;
                }
                sh.account.probe(sh.tuning.probe_every);
                if last_checkpoint.elapsed() >= sh.tuning.checkpoint {
                    checkpoint(&sh, d).await;
                    last_checkpoint = Instant::now();
                }
            }
        }
    };
    set.abort_all();
    while set.join_next().await.is_some() {}
    checkpoint(&sh, d).await;
    if matches!(result, Err(Retry(ref m)) if m.starts_with("reprise refusée")) {
        d.truncate().map_err(Fatal)?;
        d.save_meta();
    }
    result
}

/// Record synced progress: snapshot the holes, sync the data, then replace
/// the sidecar.
async fn checkpoint(sh: &Arc<Shared>, d: &mut Dest) {
    let holes = sh.work().holes();
    let file = Arc::clone(sh);
    let synced = tokio::task::spawn_blocking(move || file.file.sync_data()).await;
    if matches!(synced, Ok(Ok(()))) {
        d.meta.todo = Some(holes);
        d.save_meta();
    }
}

/// The first connection: already open, its span already claimed.
struct Handed {
    resp: reqwest::Response,
    idx: usize,
    slot: Slot,
    id: u64,
}

/// One connection: take a span, fetch it, write it, repeat until no work is
/// left. Transient failures end the connection (the span goes back to the
/// pool; the coordinator opens another when a server is available).
/// `Err` only when retrying cannot help.
async fn connection(
    sh: Arc<Shared>,
    client: reqwest::Client,
    urls: Arc<Vec<String>>,
    mut handed: Option<Handed>,
) -> Result<(), String> {
    loop {
        let (slot, id, pos, resp, idx) = if let Some(h) = handed.take() {
            let pos = sh.work().holes_of(h.id).map_or(0, |(p, _)| p);
            (h.slot, h.id, pos, h.resp, h.idx)
        } else {
            let slot = sh.account.acquire().await;
            let claimed = sh.work().take(&sh.tuning);
            let Some((id, pos, end)) = claimed else {
                return Ok(());
            };
            match open(&sh, &client, &urls, id, pos, end).await? {
                Some((resp, idx)) => (slot, id, pos, resp, idx),
                None => return Ok(()),
            }
        };
        sh.flowing.fetch_add(1, Ordering::SeqCst);
        let flowing = Flowing(&sh.flowing);

        let fetched = fetch(&sh, resp, id, pos).await;
        drop(flowing);
        let (outcome, finished) = match fetched {
            Ok(r) => r,
            Err(fatal) => {
                sh.server_done(idx, ServerOutcome::Ok);
                sh.work().release(id);
                return Err(fatal);
            }
        };
        sh.server_done(idx, outcome);
        if !finished {
            sh.work().release(id);
            return Ok(());
        }
        drop(slot);
    }
}

/// Request `[pos, end)` from the least busy server. `Ok(None)`: the span went
/// back to the pool (no server now, or this one failed).
async fn open(
    sh: &Shared,
    client: &reqwest::Client,
    urls: &[String],
    id: u64,
    pos: u64,
    end: u64,
) -> Result<Option<(reqwest::Response, usize)>, String> {
    let Some(idx) = sh.pick_server() else {
        sh.work().release(id);
        return Ok(None);
    };
    let sent = client
        .get(&urls[idx])
        .header(header::RANGE, format!("bytes={pos}-{}", end - 1))
        .send()
        .await;
    let fail = |outcome: ServerOutcome, e: String| {
        sh.note_error(e);
        sh.server_done(idx, outcome);
        sh.work().release(id);
    };
    let resp = match sent {
        Ok(r) => r,
        Err(e) => {
            fail(ServerOutcome::Failed, fluxplay_providers::redact_error(&e));
            return Ok(None);
        }
    };
    let status = resp.status();
    if status == StatusCode::PARTIAL_CONTENT {
        let range = resp
            .headers()
            .get(header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_content_range);
        if range == Some((pos, Some(sh.total))) {
            return Ok(Some((resp, idx)));
        }
        fail(ServerOutcome::Bad, "plage inattendue".into());
    } else if status.is_success() {
        sh.lost_ranges.store(true, Ordering::SeqCst);
        fail(ServerOutcome::Bad, "le serveur ne découpe pas le fichier".into());
    } else {
        let msg = format!("HTTP {status}");
        match status.as_u16() {
            400 | 401 | 404 | 405 | 410 | 416 | 451 => fail(ServerOutcome::Bad, msg),
            // How panels say the account has no free connection.
            403 | 429 | 458 | 503 | 509 => {
                if sh.account.in_use() > 1 {
                    sh.account.refused();
                }
                fail(ServerOutcome::Failed, msg);
            }
            _ => fail(ServerOutcome::Failed, msg),
        }
    }
    if sh.all_bad() && !sh.lost_ranges.load(Ordering::SeqCst) {
        let e = sh.last_error.lock().unwrap_or_else(|p| p.into_inner()).clone();
        return Err(e.unwrap_or_else(|| "aucun serveur ne fournit ce fichier".into()));
    }
    Ok(None)
}

/// Stream `resp` into span `id` from `pos`. Returns how the server behaved and
/// whether the span is complete (stolen tail included); `Err` on a disk error.
async fn fetch(
    sh: &Arc<Shared>,
    mut resp: reqwest::Response,
    id: u64,
    mut pos: u64,
) -> Result<(ServerOutcome, bool), String> {
    let mut buf: Vec<u8> = Vec::with_capacity(WRITE_BUFFER);
    let mut moved = 0u64;
    loop {
        let chunk = match resp.chunk().await {
            Ok(Some(c)) => Some(c),
            Ok(None) => None,
            Err(e) => {
                sh.note_error(fluxplay_providers::redact_error(&e));
                let finished = flush(sh, id, &mut pos, &mut buf).await?;
                return Ok((outcome_after(moved), finished));
            }
        };
        let eof = chunk.is_none();
        if let Some(c) = chunk {
            buf.extend_from_slice(&c);
            moved += c.len() as u64;
            sh.account.add_bytes(c.len() as u64);
        }
        if eof || buf.len() >= WRITE_BUFFER {
            if flush(sh, id, &mut pos, &mut buf).await? {
                return Ok((ServerOutcome::Ok, true));
            }
            if eof {
                sh.note_error("connexion interrompue".into());
                return Ok((outcome_after(moved), false));
            }
        }
    }
}

/// A connection that moved real data is not the server's fault.
fn outcome_after(moved: u64) -> ServerOutcome {
    if moved >= PROGRESS_RESETS_RETRIES {
        ServerOutcome::Ok
    } else {
        ServerOutcome::Failed
    }
}

/// Write the buffered bytes that still belong to span `id` at `pos`.
/// `Ok(true)` once the span is complete.
async fn flush(sh: &Arc<Shared>, id: u64, pos: &mut u64, buf: &mut Vec<u8>) -> Result<bool, String> {
    let n = sh.work().clamp(id, buf.len() as u64) as usize;
    if n == 0 {
        buf.clear();
        return Ok(sh.work().clamp(id, 1) == 0);
    }
    let data = std::mem::take(buf);
    let at = *pos;
    let file = Arc::clone(sh);
    let (written, mut data) = tokio::task::spawn_blocking(move || {
        let r = write_at(&file.file, &data[..n], at);
        (r, data)
    })
    .await
    .map_err(|e| e.to_string())?;
    written.map_err(|e| format!("écriture disque : {e}"))?;
    *pos += n as u64;
    let finished = sh.work().commit(id, n as u64);
    data.clear();
    *buf = data;
    Ok(finished)
}

#[cfg(unix)]
fn write_at(file: &std::fs::File, data: &[u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(data, offset)
}

#[cfg(windows)]
fn write_at(file: &std::fs::File, data: &[u8], offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0;
    while done < data.len() {
        let n = file.seek_write(&data[done..], offset + done as u64)?;
        if n == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        done += n;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Tuning = Tuning {
        segmented_min: 0,
        min_split: 10,
        max_span: 100,
        checkpoint: Duration::from_secs(5),
        server_backoff: Duration::from_millis(1),
        probe_every: Duration::from_secs(60),
    };

    #[test]
    fn take_caps_spans_then_splits_the_largest() {
        let mut w = Work::new(&[(0, 250)]);
        assert_eq!(w.take(&T), Some((0, 0, 100)));
        assert_eq!(w.take(&T), Some((1, 100, 200)));
        assert_eq!(w.take(&T), Some((2, 200, 250)));
        // Everything owned: split the largest remaining (span 1, 100 bytes).
        let (_, start, end) = w.take(&T).unwrap();
        assert_eq!((start, end), (150, 200));
        assert_eq!(w.clamp(1, 80), 50, "owner stops at the thief's start");
        assert_eq!(w.missing(), 250);
    }

    #[test]
    fn commit_release_and_holes() {
        let mut w = Work::new(&[(0, 40), (60, 100)]);
        let (a, ..) = w.take(&T).unwrap();
        assert!(!w.commit(a, 15));
        w.release(a);
        assert_eq!(w.holes(), vec![(15, 40), (60, 100)]);
        let (b, pos, end) = w.take(&T).unwrap();
        assert_eq!((pos, end), (15, 40));
        assert!(w.commit(b, 25));
        assert_eq!(w.holes(), vec![(60, 100)]);
        assert!(w.has_work(&T));
        let (c, ..) = w.take(&T).unwrap();
        assert!(w.has_work(&T), "40 bytes left: splittable");
        assert!(w.commit(c, 40));
        assert!(w.spans.is_empty());
        assert!(!w.has_work(&T));
    }
}
