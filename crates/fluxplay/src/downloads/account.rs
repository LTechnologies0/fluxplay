//! Streams and connection slots of one IPTV account, shared by every download
//! of it.
//!
//! The panel's `max_connections` counts streams — one per title being
//! fetched, each with its own token ([`super::links`]) — and revokes the
//! oldest when one too many starts. Downloads therefore hold a [`Lease`] for
//! their whole run: one stream fewer than announced (playback needs one), at
//! least one.
//!
//! Connections sharing a stream are not counted by such panels, but CDN nodes
//! cap each connection's speed when busy, and other panels do count them, so
//! the connection count is probed. Every [`Tuning::probe_every`](super::parallel::Tuning)
//! [`Account::probe`] adds a slot when all of them are busy and a download
//! waits for one; the next probe keeps it if the account's throughput grew,
//! and gives it back otherwise (line already full, per-IP caps, panels
//! dropping the oldest stream). CDN nodes throttle each connection when
//! busy and not when idle, and every connection starts with a fast burst, so
//! a useless trial is retried after [`RETRY_NO_GAIN`], doubling up to
//! [`PROBE_HOLD`], and at once when throughput drops well below what it was. A trial slot refused twice at the same count is the panel's
//! limit (once may be a panel still counting a closed connection): the count
//! goes below it with one connection left for playback, and that ceiling
//! holds for [`REFUSAL_MEMORY`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

use super::MAX_CONNECTIONS;

/// A refusal caps the account only this long: panels also answer 503 / 509
/// when briefly overloaded.
const REFUSAL_MEMORY: Duration = Duration::from_secs(30 * 60);
/// Longest wait before another trial.
const PROBE_HOLD: Duration = Duration::from_secs(5 * 60);
/// First wait after a trial that brought nothing.
const RETRY_NO_GAIN: Duration = Duration::from_secs(30);
/// Retry a trial refused once this soon, to tell a limit from a glitch.
const RETRY_REFUSED: Duration = Duration::from_secs(30);
/// Outside a trial, two refusals this close mean a slot is really gone.
const REFUSALS_CLOSE: Duration = Duration::from_secs(60);

pub(crate) struct Account {
    /// Source id, for logs; empty for a download without profile.
    key: String,
    state: Mutex<State>,
    freed: Notify,
    /// Bytes received by every download of the account.
    bytes: AtomicU64,
}

#[derive(Debug)]
struct State {
    /// Slots downloads may hold together now.
    cap: usize,
    /// Never more than this, whatever probing shows.
    max: usize,
    in_use: usize,
    /// Connections waiting for a slot: demand for one more.
    waiting: usize,
    /// Most slots kept after a successful trial.
    served: usize,
    refused_at: Option<(usize, Instant)>,
    /// A trial at this count was refused once.
    suspect: Option<usize>,
    /// Last refusal outside a trial.
    last_refusal: Option<Instant>,
    hold_until: Option<Instant>,
    /// Throughput when the hold started: a clear drop ends it early.
    hold_rate: u64,
    /// Trials in a row that brought nothing.
    no_gain: u32,
    /// Account throughput (bytes/s) before the slot on trial was added.
    trial: Option<u64>,
    last_probe: Option<(Instant, u64)>,
    /// Titles the account's downloads may fetch at once.
    streams_max: usize,
    streams: usize,
}

impl State {
    fn ceiling(&mut self) -> usize {
        if self
            .refused_at
            .is_some_and(|(_, at)| at.elapsed() >= REFUSAL_MEMORY)
        {
            self.refused_at = None;
        }
        self.refused_at
            .map_or(self.max, |(n, _)| cap_below_refusal(n).min(self.max))
    }
}

/// Slots for an account that refused its `n`th connection: one fewer, and one
/// more left for playback.
pub(crate) fn cap_below_refusal(n: usize) -> usize {
    n.saturating_sub(2).clamp(1, MAX_CONNECTIONS)
}

/// One held connection slot; freed on drop.
pub(crate) struct Slot(Arc<Account>);

impl Drop for Slot {
    fn drop(&mut self) {
        {
            let mut st = self.0.state();
            st.in_use = st.in_use.saturating_sub(1);
        }
        self.0.freed.notify_waiters();
    }
}

/// One of the account's streams, held by a download for its whole run.
pub(crate) struct Lease(Arc<Account>);

impl Drop for Lease {
    fn drop(&mut self) {
        {
            let mut st = self.0.state();
            st.streams = st.streams.saturating_sub(1);
        }
        self.0.freed.notify_waiters();
    }
}

struct Waiting<'a>(&'a Account);

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        let mut st = self.0.state();
        st.waiting = st.waiting.saturating_sub(1);
    }
}

impl Account {
    fn new(key: &str, start: usize, max: usize, streams: usize) -> Arc<Self> {
        let max = max.clamp(1, MAX_CONNECTIONS);
        Arc::new(Self {
            key: key.to_string(),
            state: Mutex::new(State {
                cap: start.clamp(1, max),
                max,
                in_use: 0,
                waiting: 0,
                served: 0,
                refused_at: None,
                suspect: None,
                last_refusal: None,
                hold_until: None,
                hold_rate: 0,
                no_gain: 0,
                trial: None,
                last_probe: None,
                streams_max: streams.max(1),
                streams: 0,
            }),
            freed: Notify::new(),
            bytes: AtomicU64::new(0),
        })
    }

    /// The account's slots, created with `start` of them the first time;
    /// later downloads keep what earlier ones learned. `streams`: titles at
    /// once, as the panel announces now.
    pub(crate) fn shared(key: &str, start: usize, streams: usize) -> Arc<Self> {
        if key.is_empty() {
            return Self::new(key, start, start, streams);
        }
        static ACCOUNTS: OnceLock<Mutex<HashMap<String, Arc<Account>>>> = OnceLock::new();
        let mut map = ACCOUNTS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let account = Arc::clone(
            map.entry(key.to_string())
                .or_insert_with(|| Self::new(key, start, MAX_CONNECTIONS, streams)),
        );
        drop(map);
        account.state().streams_max = streams.max(1);
        account.freed.notify_waiters();
        account
    }

    /// Slots fixed to `start`, growing up to `max`, known to no other download.
    #[cfg(test)]
    pub(crate) fn detached(start: usize, max: usize) -> Arc<Self> {
        Self::new("", start, max, usize::MAX)
    }

    /// Wait until the account may fetch one more title.
    pub(crate) async fn lease(self: &Arc<Self>) -> Lease {
        loop {
            let freed = self.freed.notified();
            tokio::pin!(freed);
            freed.as_mut().enable();
            if let Some(lease) = self.try_lease() {
                return lease;
            }
            freed.await;
        }
    }

    pub(crate) fn try_lease(self: &Arc<Self>) -> Option<Lease> {
        let mut st = self.state();
        if st.streams >= st.streams_max {
            return None;
        }
        st.streams += 1;
        Some(Lease(Arc::clone(self)))
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Whether a file may use more than one connection.
    pub(crate) fn can_split(&self) -> bool {
        self.state().ceiling() > 1
    }

    #[cfg(test)]
    pub(crate) fn ceiling(&self) -> usize {
        self.state().ceiling()
    }

    #[cfg(test)]
    pub(crate) fn cap(&self) -> usize {
        self.state().cap
    }

    #[cfg(test)]
    pub(crate) fn served(&self) -> usize {
        self.state().served
    }

    pub(crate) fn in_use(&self) -> usize {
        self.state().in_use
    }

    pub(crate) fn add_bytes(&self, n: u64) {
        self.bytes.fetch_add(n, Ordering::Relaxed);
    }

    /// Wait for a free slot.
    pub(crate) async fn acquire(self: &Arc<Self>) -> Slot {
        loop {
            let freed = self.freed.notified();
            tokio::pin!(freed);
            freed.as_mut().enable();
            {
                let mut st = self.state();
                if st.in_use < st.cap {
                    if st.in_use == 0 && st.waiting == 0 {
                        // Idle since the last download: old throughput means nothing.
                        st.trial = None;
                        st.last_probe = None;
                    }
                    st.in_use += 1;
                    return Slot(Arc::clone(self));
                }
                st.waiting += 1;
            }
            let _waiting = Waiting(self);
            freed.await;
        }
    }

    /// The panel refused a connection (403 / 429 / 458 / 503 / 509). A trial
    /// refused twice at the same count is the account's limit. Outside a
    /// trial, a second refusal within [`REFUSALS_CLOSE`] means another stream
    /// (playback) took a slot: give one back for a while.
    pub(crate) fn refused(&self) {
        let mut st = self.state();
        let now = Instant::now();
        if st.trial.take().is_some() && st.suspect != Some(st.cap) {
            st.suspect = Some(st.cap);
            st.cap = st.cap.saturating_sub(1).max(1);
            st.hold_until = Some(now + RETRY_REFUSED);
            st.hold_rate = 0;
            tracing::info!(account = %self.key, cap = st.cap, "downloads: extra connection refused once");
        } else if st.suspect == Some(st.cap) {
            st.suspect = None;
            let n = st.refused_at.map_or(st.cap, |(m, _)| m.min(st.cap));
            st.refused_at = Some((n, now));
            st.served = st.served.min(n.saturating_sub(1));
            st.cap = cap_below_refusal(n);
            tracing::info!(account = %self.key, refused_at = n, cap = st.cap, "downloads: panel limit found");
        } else if st
            .last_refusal
            .replace(now)
            .is_some_and(|t| now.duration_since(t) < REFUSALS_CLOSE)
            && st.cap > 1
        {
            st.cap -= 1;
            st.last_refusal = None;
            st.hold_until = Some(now + PROBE_HOLD);
            st.hold_rate = 0;
            tracing::info!(account = %self.key, cap = st.cap, "downloads: connections refused, one fewer");
        }
    }

    /// Called often by running downloads; acts once per `every`: settle the
    /// slot on trial, then maybe start a new trial.
    pub(crate) fn probe(&self, every: Duration) {
        let now = Instant::now();
        let bytes = self.bytes.load(Ordering::Relaxed);
        let mut st = self.state();
        let Some((t0, b0)) = st.last_probe else {
            st.last_probe = Some((now, bytes));
            return;
        };
        let dt = now.duration_since(t0);
        if dt < every || dt.is_zero() {
            return;
        }
        let rate = (bytes.saturating_sub(b0) as f64 / dt.as_secs_f64()) as u64;
        st.last_probe = Some((now, bytes));

        if let Some(before) = st.trial.take() {
            let per_slot = before / (st.cap.saturating_sub(1).max(1) as u64);
            if rate >= before + per_slot / 2 {
                st.served = st.served.max(st.cap);
                st.no_gain = 0;
                if st.suspect.is_some_and(|n| n <= st.cap) {
                    st.suspect = None;
                }
                tracing::info!(account = %self.key, cap = st.cap, rate, before, "downloads: extra connection kept");
            } else {
                st.cap = st.cap.saturating_sub(1).max(1);
                let wait = RETRY_NO_GAIN.saturating_mul(1 << st.no_gain.min(4)).min(PROBE_HOLD);
                st.no_gain += 1;
                st.hold_until = Some(now + wait);
                st.hold_rate = before.max(rate);
                tracing::info!(account = %self.key, cap = st.cap, rate, before, "downloads: extra connection brought nothing");
                return;
            }
        }
        if st.hold_until.is_some_and(|t| now < t) {
            if rate.saturating_mul(10) >= st.hold_rate.saturating_mul(6) {
                return;
            }
            // Much slower than when the hold began: the node started throttling.
            st.hold_until = None;
            st.no_gain = 0;
        }
        let ceiling = st.ceiling();
        if st.cap > ceiling {
            st.cap = ceiling;
            return;
        }
        if st.cap < ceiling && st.in_use >= st.cap && st.waiting > 0 && rate > 0 {
            st.trial = Some(rate);
            st.cap += 1;
            tracing::info!(account = %self.key, cap = st.cap, rate, "downloads: trying one more connection");
            drop(st);
            self.freed.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn slots_are_exact_and_shared() {
        let a = Account::detached(2, 2);
        let s1 = a.acquire().await;
        let _s2 = a.acquire().await;
        assert_eq!(a.in_use(), 2);
        let waiter = {
            let a = Arc::clone(&a);
            tokio::spawn(async move { a.acquire().await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished(), "third waits");
        drop(s1);
        let _s3 = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("freed slot is handed over")
            .unwrap();
        assert_eq!(a.in_use(), 2);
    }

    #[tokio::test]
    async fn cancelled_waiter_is_not_counted() {
        let a = Account::detached(1, 1);
        let _s = a.acquire().await;
        let _ = tokio::time::timeout(Duration::from_millis(10), a.acquire()).await;
        assert_eq!(a.state().waiting, 0);
    }

    #[test]
    fn refusals_outside_a_trial_give_one_back_when_repeated() {
        let a = Account::detached(3, 8);
        a.refused();
        assert_eq!(a.cap(), 3, "one refusal may be a span handover counted twice");
        a.refused();
        assert_eq!(a.cap(), 2);
        assert_eq!(a.ceiling(), 8, "no limit learned from a refusal outside a trial");
    }

    fn trial_at(a: &Account, cap: usize) {
        let mut st = a.state();
        st.trial = Some(1000);
        st.cap = cap;
        st.hold_until = None;
    }

    #[test]
    fn a_trial_refused_once_is_retried_before_it_counts() {
        let a = Account::detached(4, 8);
        trial_at(&a, 5);
        a.refused();
        assert_eq!(a.cap(), 4);
        assert_eq!(a.ceiling(), 8, "one refusal may be a panel still counting a closed stream");
        trial_at(&a, 5);
        a.refused();
        assert_eq!(a.ceiling(), 3, "refused twice at 5: 4 work, one kept for playback");
    }

    #[test]
    fn refusal_during_a_trial_sets_the_ceiling() {
        let a = Account::detached(4, 8);
        a.state().served = 4;
        trial_at(&a, 5);
        a.refused();
        trial_at(&a, 5);
        a.refused();
        assert_eq!(a.ceiling(), 3, "refused the 5th: 4 work, one kept for playback");
        assert_eq!(a.cap(), 3);
        assert_eq!(a.served(), 4);
    }

    #[test]
    fn strict_single_connection_account_stays_single() {
        let a = Account::detached(1, 8);
        trial_at(&a, 2);
        a.refused();
        trial_at(&a, 2);
        a.refused();
        assert_eq!(a.ceiling(), 1);
        assert!(!a.can_split());
    }

    #[test]
    fn useless_trials_back_off_and_a_slowdown_ends_the_wait() {
        let a = Account::detached(1, 8);
        {
            let mut st = a.state();
            st.cap = 2;
            st.in_use = 2;
            st.waiting = 1;
            st.trial = Some(8_000_000);
            st.last_probe = Some((Instant::now() - Duration::from_secs(1), 0));
        }
        a.add_bytes(8_100_000);
        a.probe(Duration::ZERO);
        {
            let st = a.state();
            assert_eq!(st.cap, 1, "no gain: slot given back");
            assert_eq!(st.no_gain, 1);
            let wait = st.hold_until.unwrap() - Instant::now();
            assert!(wait <= RETRY_NO_GAIN && wait > RETRY_NO_GAIN / 2);
        }
        {
            let mut st = a.state();
            st.in_use = 1;
            st.last_probe = Some((Instant::now() - Duration::from_secs(1), a.bytes.load(Ordering::Relaxed)));
        }
        a.add_bytes(1_500_000);
        a.probe(Duration::ZERO);
        let st = a.state();
        assert_eq!(st.cap, 2, "throughput fell to 1.5 MB/s: try again now");
        assert!(st.trial.is_some());
    }

    #[test]
    fn shared_accounts_keep_what_they_learned() {
        let a = Account::shared("test-shared-account", 1, 1);
        a.state().cap = 4;
        let b = Account::shared("test-shared-account", 1, 2);
        assert_eq!(b.cap(), 4);
        assert_eq!(a.state().streams_max, 2, "the latest announcement applies");
        assert!(Arc::ptr_eq(&a, &b));
        assert!(!Arc::ptr_eq(&Account::shared("", 2, 1), &Account::shared("", 2, 1)));
    }

    #[tokio::test]
    async fn one_title_at_a_time_on_a_single_stream_account() {
        let a = Account::new("", 2, 8, 1);
        let first = a.lease().await;
        assert!(a.try_lease().is_none());
        let second = {
            let a = Arc::clone(&a);
            tokio::spawn(async move { a.lease().await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!second.is_finished(), "the second title waits");
        drop(first);
        let _second = tokio::time::timeout(Duration::from_secs(1), second)
            .await
            .expect("handed over when the first ends")
            .unwrap();
        assert_eq!(a.state().streams, 1);
    }
}
