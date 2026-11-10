//! Background soft-RGBA pump — keeps `mpv_render_context_render` off the iced UI thread.
//!
//! Soft present at 1080p+ is otherwise blocked by UI-thread render + iced texture upload
//! (~100–200 ms/frame). The pump renders into a slot; `pull_video_frame` only swaps.
//!
//! Waiting uses [`FrameWake`] (condvar), not `thread::sleep`: on Windows a 2 ms sleep
//! is rounded up to ~15.6 ms and the picture hitchs every frame.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::mpv_ffi::{FrameWake, LibMpv};

/// SoftPump recycle slots fed by UI discards / hold reclaim (W3-01 F1).
/// Matches in-flight upload + pending + mpv scratch headroom without a 6th fresh alloc.
pub const SOFT_RGBA_RECYCLE_POOL: usize = 3;

pub struct SoftFrame {
    pub w: u32,
    pub h: u32,
    pub pixels: Vec<u8>,
    pub gen: u64,
}

/// Shared state between UI and the soft render worker.
pub struct SoftPump {
    stop: Arc<AtomicBool>,
    want_w: Arc<AtomicU32>,
    want_h: Arc<AtomicU32>,
    /// Latest completed frame (UI takes ownership on pull).
    latest: Arc<Mutex<Option<SoftFrame>>>,
    /// Capacity pool fed by UI discards / hold-chain reclaim.
    recycle: Arc<Mutex<Vec<Vec<u8>>>>,
    /// Generation of frame UI has already consumed (for needs_redraw).
    consumed_gen: Arc<AtomicU32>,
    produced_gen: Arc<AtomicU32>,
    /// True while `latest` holds a frame the UI has not taken yet.
    slot_pending: Arc<AtomicBool>,
    /// Wake SoftPump when the UI frees the pending slot.
    slot_free: Arc<(Mutex<()>, Condvar)>,
    join: Option<JoinHandle<()>>,
}

impl SoftPump {
    /// `None` when the worker thread cannot start — callers must not store a
    /// dead pump (that would black the stage forever while audio plays).
    pub fn start(mpv: Arc<Mutex<LibMpv>>) -> Option<Self> {
        // Poison → recover (W4-07 F2). Mutex::lock only fails on poison —
        // never install an orphan FrameWake (W3-01 F5).
        let wake = match mpv.lock() {
            Ok(g) => g.frame_dirty_handle(),
            Err(e) => {
                tracing::warn!("libmpv mutex poisoned at SoftPump::start — recovering");
                e.into_inner().frame_dirty_handle()
            }
        };
        Self::start_with_wake(mpv, wake)
    }

    /// Start with a pre-shared wake — the pump loop never takes the mpv
    /// lock just to check dirtiness (lock-free fast path + condvar).
    pub fn start_with_wake(mpv: Arc<Mutex<LibMpv>>, frame_wake: Arc<FrameWake>) -> Option<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        // Conservative seed until UI/`set_target` lands SoftBudget (W3-01 F6).
        // 854×480 avoids a first-frame 720p SW render on Low SoftBudget devices.
        let want_w = Arc::new(AtomicU32::new(854));
        let want_h = Arc::new(AtomicU32::new(480));
        let latest = Arc::new(Mutex::new(None));
        let recycle = Arc::new(Mutex::new(Vec::new()));
        let consumed_gen = Arc::new(AtomicU32::new(0));
        let produced_gen = Arc::new(AtomicU32::new(0));
        let slot_pending = Arc::new(AtomicBool::new(false));
        let slot_free = Arc::new((Mutex::new(()), Condvar::new()));

        let stop_t = Arc::clone(&stop);
        let want_w_t = Arc::clone(&want_w);
        let want_h_t = Arc::clone(&want_h);
        let latest_t = Arc::clone(&latest);
        let recycle_t = Arc::clone(&recycle);
        let produced_t = Arc::clone(&produced_gen);
        let slot_pending_t = Arc::clone(&slot_pending);
        let slot_free_t = Arc::clone(&slot_free);
        let mpv_t = Arc::clone(&mpv);
        let wake_t = Arc::clone(&frame_wake);

        let join = match thread::Builder::new()
            .name("fp-soft-pump".into())
            .spawn(move || {
                // SoftPump below AO: nicer + avoid CPU0 (often RT/pipewire on Linux).
                #[cfg(unix)]
                {
                    extern "C" {
                        fn setpriority(which: i32, who: u32, prio: i32) -> i32;
                    }
                    const PRIO_PROCESS: i32 = 0;
                    unsafe {
                        let _ = setpriority(PRIO_PROCESS, 0, 5);
                    }
                }
                #[cfg(target_os = "linux")]
                {
                    if let Ok(n) = thread::available_parallelism() {
                        let n = n.get();
                        if n > 2 {
                            // Best-effort: pin SoftPump to cores 1..n-1 (leave 0 for AO).
                            extern "C" {
                                fn sched_setaffinity(
                                    pid: i32,
                                    cpusetsize: usize,
                                    mask: *const libc_cpu_set,
                                ) -> i32;
                            }
                            #[repr(C)]
                            struct libc_cpu_set {
                                bits: [u64; 16],
                            }
                            let mut set = libc_cpu_set { bits: [0; 16] };
                            for cpu in 1..n.min(128) {
                                set.bits[cpu / 64] |= 1u64 << (cpu % 64);
                            }
                            unsafe {
                                let _ = sched_setaffinity(
                                    0,
                                    size_of::<libc_cpu_set>(),
                                    &set,
                                );
                            }
                        }
                    }
                }
                let mut local_gen: u64 = 0;
                let mut local_recycle: Option<Vec<u8>> = None;
                while !stop_t.load(Ordering::Acquire) {
                    let w = (want_w_t.load(Ordering::Relaxed).clamp(2, 3840) & !1).max(2);
                    let h = (want_h_t.load(Ordering::Relaxed).clamp(2, 2160) & !1).max(2);

                    // One frame in hand until the UI takes it. Overwriting the slot
                    // dropped pictures and made soft mpv surge then hitch.
                    if slot_pending_t.load(Ordering::Acquire) {
                        wait_slot(
                            &slot_free_t,
                            &stop_t,
                            &slot_pending_t,
                            Duration::from_millis(16),
                        );
                        continue;
                    }
                    if !wake_t.is_dirty() {
                        // Park until mpv's render callback (or 16 ms so stop/size changes apply).
                        wake_t.wait_while_clean(&stop_t, Duration::from_millis(16));
                        continue;
                    }

                    if local_recycle.is_none() {
                        if let Ok(mut pool) = recycle_t.lock() {
                            local_recycle = pool.pop();
                        }
                    }

                    let rendered = {
                        let mut guard = match mpv_t.lock() {
                            Ok(g) => g,
                            Err(e) => {
                                // Poison must not kill SoftPump forever (W4-07 F2).
                                tracing::warn!(
                                    "libmpv mutex poisoned in SoftPump render — recovering"
                                );
                                e.into_inner()
                            }
                        };
                        if let Some(buf) = local_recycle.take() {
                            guard.recycle_sw_rgba(buf);
                        }
                        guard.render_sw_rgba(w, h)
                    };

                    match rendered {
                        Some(pixels) => {
                            local_gen = local_gen.wrapping_add(1);
                            let gen_u32 = (local_gen & 0xffff_ffff) as u32;
                            produced_t.store(gen_u32, Ordering::Release);
                            if let Ok(mut slot) = latest_t.lock() {
                                // Replaced unread frame → return capacity to the pool.
                                if let Some(old) = slot.replace(SoftFrame {
                                    w,
                                    h,
                                    pixels,
                                    gen: local_gen,
                                }) {
                                    offer_to_pool(&recycle_t, old.pixels);
                                }
                                slot_pending_t.store(true, Ordering::Release);
                            }
                            // Yield the mpv lock to the UI for property reads; park, don't sleep.
                            wake_t.wait_while_clean(&stop_t, Duration::from_millis(1));
                        }
                        None => {
                            wake_t.wait_while_clean(&stop_t, Duration::from_millis(4));
                        }
                    }
                }
            }) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(error = %e, "soft-pump thread spawn failed");
                return None;
            }
        };

        Some(Self {
            stop,
            want_w,
            want_h,
            latest,
            recycle,
            consumed_gen,
            produced_gen,
            slot_pending,
            slot_free,
            join: Some(join),
        })
    }

    pub fn set_target(&self, w: u32, h: u32) {
        let nw = (w.clamp(2, 3840) & !1).max(2);
        let nh = (h.clamp(2, 2160) & !1).max(2);
        let pw = self.want_w.load(Ordering::Relaxed);
        let ph = self.want_h.load(Ordering::Relaxed);
        // Hysteresis: avoid SoftPump realloc thrash on ±16px layout jitter.
        if pw > 0 && ph > 0 {
            let dw = (pw as i32 - nw as i32).unsigned_abs();
            let dh = (ph as i32 - nh as i32).unsigned_abs();
            if dw <= 16 && dh <= 16 {
                return;
            }
            let aspect_ok = (pw >= ph) == (nw >= nh);
            if aspect_ok {
                let frw = ((nw as f32 - pw as f32) / pw as f32).abs();
                let frh = ((nh as f32 - ph as f32) / ph as f32).abs();
                if frw < 0.08 && frh < 0.08 {
                    return;
                }
            }
        }
        self.want_w.store(nw, Ordering::Relaxed);
        self.want_h.store(nh, Ordering::Relaxed);
    }

    pub fn needs_redraw(&self) -> bool {
        let p = self.produced_gen.load(Ordering::Acquire);
        let c = self.consumed_gen.load(Ordering::Acquire);
        p != c
    }

    /// Take the latest frame if newer than last consume (or any pending).
    pub fn take_frame(&self) -> Option<(u32, u32, Vec<u8>)> {
        let mut slot = self.latest.lock().ok()?;
        let frame = slot.take()?;
        self.consumed_gen
            .store((frame.gen & 0xffff_ffff) as u32, Ordering::Release);
        self.slot_pending.store(false, Ordering::Release);
        drop(slot);
        self.slot_free.1.notify_one();
        Some((frame.w, frame.h, frame.pixels))
    }

    pub fn offer_recycle(&self, buf: Vec<u8>) {
        offer_to_pool(&self.recycle, buf);
        self.slot_free.1.notify_one();
    }

    fn signal_stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.slot_pending.store(false, Ordering::Release);
        self.slot_free.1.notify_all();
    }

    /// Signal the worker to exit and return its [`JoinHandle`] without blocking.
    ///
    /// Callers that will start a replacement SoftPump on the same mpv **must**
    /// join (or wait until [`JoinHandle::is_finished`]) before `start` — otherwise
    /// two workers render into one context. Zap / iced `update` should park the
    /// handle and join off the UI thread (W3-01 F3).
    pub fn stop_async(mut self) -> Option<JoinHandle<()>> {
        self.signal_stop();
        // Return unread pixels + pool buffers to… nowhere here (no LibMpv). The
        // worker Arc keeps LibMpv alive until join; UI still holds recycled offers.
        if let Ok(mut slot) = self.latest.lock() {
            if let Some(frame) = slot.take() {
                offer_to_pool(&self.recycle, frame.pixels);
            }
        }
        self.join.take()
    }

    /// Signal stop and join on a detached helper so the UI/player thread never
    /// waits on mid-`render_sw_rgba`. Prefer [`Self::stop_async`] when the caller
    /// must serialize a replacement pump.
    #[allow(dead_code)] // public API; backend uses `stop_async` + pending join
    pub fn stop(self) {
        if let Some(h) = self.stop_async() {
            if h.is_finished() {
                let _ = h.join();
                return;
            }
            struct JoinGuard(Option<JoinHandle<()>>);
            impl Drop for JoinGuard {
                fn drop(&mut self) {
                    if let Some(h) = self.0.take() {
                        let _ = h.join();
                    }
                }
            }
            let guard = JoinGuard(Some(h));
            if let Err(e) = thread::Builder::new()
                .name("fp-soft-pump-join".into())
                .spawn(move || {
                    let mut guard = guard;
                    if let Some(h) = guard.0.take() {
                        let _ = h.join();
                    }
                })
            {
                tracing::error!(
                    error = %e,
                    "soft-pump stop join spawn failed — Drop will sync-join"
                );
            }
        }
    }
}

fn offer_to_pool(pool: &Mutex<Vec<Vec<u8>>>, buf: Vec<u8>) {
    let Ok(mut slot) = pool.lock() else {
        return;
    };
    // Prefer larger capacity; keep at most SOFT_RGBA_RECYCLE_POOL buffers.
    if slot.len() < SOFT_RGBA_RECYCLE_POOL {
        slot.push(buf);
        return;
    }
    if let Some(worst) = slot.iter().enumerate().min_by_key(|(_, b)| b.capacity()) {
        let idx = worst.0;
        if slot[idx].capacity() < buf.capacity() {
            slot[idx] = buf;
        }
    }
}

fn wait_slot(
    slot_free: &(Mutex<()>, Condvar),
    stop: &AtomicBool,
    slot_pending: &AtomicBool,
    timeout: Duration,
) {
    if stop.load(Ordering::Relaxed) {
        return;
    }
    let Ok(guard) = slot_free.0.lock() else {
        return;
    };
    // Exit early when UI clears the slot (notify) — do not burn a full 16 ms.
    let _ = slot_free.1.wait_timeout_while(guard, timeout, |_| {
        slot_pending.load(Ordering::Acquire) && !stop.load(Ordering::Relaxed)
    });
}

impl Drop for SoftPump {
    /// Never hold `MutexGuard<LibMpv>` across SoftPump Drop (worker may own that lock).
    /// Prefer detached join so iced/zap never hitch; if the helper cannot spawn,
    /// [`JoinGuard`] Drop sync-joins (same pattern as external `ExtReap`).
    fn drop(&mut self) {
        self.signal_stop();
        if let Some(h) = self.join.take() {
            if h.is_finished() {
                let _ = h.join();
                return;
            }
            // On spawn Err the unexecuted closure drops → JoinGuard joins sync.
            struct JoinGuard(Option<JoinHandle<()>>);
            impl Drop for JoinGuard {
                fn drop(&mut self) {
                    if let Some(h) = self.0.take() {
                        let _ = h.join();
                    }
                }
            }
            let guard = JoinGuard(Some(h));
            if let Err(e) = thread::Builder::new()
                .name("fp-soft-pump-join".into())
                .spawn(move || {
                    let mut guard = guard;
                    if let Some(h) = guard.0.take() {
                        let _ = h.join();
                    }
                })
            {
                tracing::error!(
                    error = %e,
                    "soft-pump join spawn failed — Drop will sync-join"
                );
            }
        }
    }
}
