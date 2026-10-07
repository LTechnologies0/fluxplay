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
    /// Capacity pool fed by UI discards.
    recycle: Arc<Mutex<Option<Vec<u8>>>>,
    /// Generation of frame UI has already consumed (for needs_redraw).
    consumed_gen: Arc<AtomicU32>,
    produced_gen: Arc<AtomicU32>,
    /// Wake SoftPump when the UI frees the pending slot.
    slot_free: Arc<(Mutex<()>, Condvar)>,
    join: Option<JoinHandle<()>>,
}

impl SoftPump {
    pub fn start(mpv: Arc<Mutex<LibMpv>>) -> Self {
        let wake = mpv
            .lock()
            .map(|g| g.frame_dirty_handle())
            .unwrap_or_else(|_| Arc::new(FrameWake::new(true)));
        Self::start_with_wake(mpv, wake)
    }

    /// Start with a pre-shared wake — the pump loop never takes the mpv
    /// lock just to check dirtiness (lock-free fast path + condvar).
    pub fn start_with_wake(mpv: Arc<Mutex<LibMpv>>, frame_wake: Arc<FrameWake>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let want_w = Arc::new(AtomicU32::new(1280));
        let want_h = Arc::new(AtomicU32::new(720));
        let latest = Arc::new(Mutex::new(None));
        let recycle = Arc::new(Mutex::new(None));
        let consumed_gen = Arc::new(AtomicU32::new(0));
        let produced_gen = Arc::new(AtomicU32::new(0));
        let slot_free = Arc::new((Mutex::new(()), Condvar::new()));

        let stop_t = Arc::clone(&stop);
        let want_w_t = Arc::clone(&want_w);
        let want_h_t = Arc::clone(&want_h);
        let latest_t = Arc::clone(&latest);
        let recycle_t = Arc::clone(&recycle);
        let produced_t = Arc::clone(&produced_gen);
        let slot_free_t = Arc::clone(&slot_free);
        let mpv_t = Arc::clone(&mpv);
        let wake_t = Arc::clone(&frame_wake);

        let join = thread::Builder::new()
            .name("fp-soft-pump".into())
            .spawn(move || {
                let mut local_gen: u64 = 0;
                let mut local_recycle: Option<Vec<u8>> = None;
                while !stop_t.load(Ordering::Acquire) {
                    let w = (want_w_t.load(Ordering::Relaxed).clamp(2, 3840) & !1).max(2);
                    let h = (want_h_t.load(Ordering::Relaxed).clamp(2, 2160) & !1).max(2);

                    // One frame in hand until the UI takes it. Overwriting the slot
                    // dropped pictures and made soft mpv surge then hitch.
                    let pending = latest_t
                        .lock()
                        .map(|g| g.is_some())
                        .unwrap_or(true);
                    if pending {
                        wait_slot(&slot_free_t, &stop_t, Duration::from_millis(16));
                        continue;
                    }
                    if !wake_t.is_dirty() {
                        // Park until mpv's render callback (or 16 ms so stop/size changes apply).
                        wake_t.wait_while_clean(&stop_t, Duration::from_millis(16));
                        continue;
                    }

                    if let Ok(mut slot) = recycle_t.lock() {
                        if let Some(buf) = slot.take() {
                            local_recycle = Some(buf);
                        }
                    }

                    let rendered = {
                        let mut guard = match mpv_t.lock() {
                            Ok(g) => g,
                            Err(_) => break,
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
                                slot.replace(SoftFrame {
                                    w,
                                    h,
                                    pixels,
                                    gen: local_gen,
                                });
                            }
                            // Yield the mpv lock to the UI for property reads; park, don't sleep.
                            wake_t.wait_while_clean(&stop_t, Duration::from_millis(1));
                        }
                        None => {
                            wake_t.wait_while_clean(&stop_t, Duration::from_millis(4));
                        }
                    }
                }
            })
            .ok();

        Self {
            stop,
            want_w,
            want_h,
            latest,
            recycle,
            consumed_gen,
            produced_gen,
            slot_free,
            join,
        }
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
        drop(slot);
        self.slot_free.1.notify_one();
        Some((frame.w, frame.h, frame.pixels))
    }

    pub fn offer_recycle(&self, buf: Vec<u8>) {
        if let Ok(mut slot) = self.recycle.lock() {
            let take = slot
                .as_ref()
                .map(|b| b.capacity() < buf.capacity())
                .unwrap_or(true);
            if take {
                *slot = Some(buf);
            }
        }
        self.slot_free.1.notify_one();
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        self.slot_free.1.notify_all();
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}

fn wait_slot(slot_free: &(Mutex<()>, Condvar), stop: &AtomicBool, timeout: Duration) {
    if stop.load(Ordering::Relaxed) {
        return;
    }
    let Ok(guard) = slot_free.0.lock() else {
        return;
    };
    let _ = slot_free.1.wait_timeout_while(guard, timeout, |_| !stop.load(Ordering::Relaxed));
}

impl Drop for SoftPump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.slot_free.1.notify_all();
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
    }
}
