//! Interaction / GUI profiler: wall time, CPU, RSS, and FPS.
//!
//! Enable with:
//! - `FLUXPLAY_PROFILE=1` — measure every update/view/Stopwatch, FPS overlay, INFO logs
//! - `FLUXPLAY_FPS=1` — FPS overlay only (measurements if profiler target on)
//! - `RUST_LOG=profiler=trace` — TRACE samples without overlay

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::Instant;

static PROFILE_ENV: OnceLock<bool> = OnceLock::new();
static FPS_ENV: OnceLock<bool> = OnceLock::new();
static OVERLAY: OnceLock<bool> = OnceLock::new();
static RUST_LOG_PROFILER: OnceLock<bool> = OnceLock::new();

fn profile_env() -> bool {
    *PROFILE_ENV.get_or_init(|| {
        match std::env::var("FLUXPLAY_PROFILE") {
            Ok(v) => {
                let v = v.trim();
                !(v.is_empty()
                    || v == "0"
                    || v.eq_ignore_ascii_case("false")
                    || v.eq_ignore_ascii_case("off"))
            }
            // Default OFF — profiling every PlayerTick/view kills 4K smoothness.
            Err(_) => false,
        }
    })
}

fn fps_env() -> bool {
    *FPS_ENV.get_or_init(|| env_truthy("FLUXPLAY_FPS"))
}

fn rust_log_profiler() -> bool {
    *RUST_LOG_PROFILER.get_or_init(|| {
        std::env::var("RUST_LOG")
            .ok()
            .map(|v| {
                v.split(',').any(|p| {
                    let p = p.trim();
                    p.starts_with("profiler=trace")
                        || p.starts_with("profiler=debug")
                        || p.starts_with("profiler=info")
                })
            })
            .unwrap_or(false)
    })
}

/// True when interaction profiling is active (CPU/RSS/wall on interactions).
pub fn profiling_enabled() -> bool {
    profile_env() || rust_log_profiler()
}

/// Show FPS / last-sample overlay in the UI status strip.
pub fn overlay_enabled() -> bool {
    *OVERLAY.get_or_init(|| {
        // Explicit FLUXPLAY_PROFILE / FLUXPLAY_FPS only (default profiling stays quiet in UI).
        fps_env()
            || std::env::var("FLUXPLAY_PROFILE")
                .map(|v| {
                    let v = v.trim();
                    !(v.is_empty()
                        || v == "0"
                        || v.eq_ignore_ascii_case("false")
                        || v.eq_ignore_ascii_case("off"))
                })
                .unwrap_or(false)
    })
}

fn env_truthy(key: &str) -> bool {
    match std::env::var(key) {
        Ok(v) => {
            let v = v.trim();
            !(v.is_empty()
                || v == "0"
                || v.eq_ignore_ascii_case("false")
                || v.eq_ignore_ascii_case("off"))
        }
        Err(_) => false,
    }
}

thread_local! {
    static GUI: RefCell<GuiProfiler> = RefCell::new(GuiProfiler::default());
}

pub fn with_gui_profiler<R>(f: impl FnOnce(&mut GuiProfiler) -> R) -> R {
    GUI.with(|g| f(&mut g.borrow_mut()))
}

pub fn overlay_status_line() -> Option<String> {
    if !overlay_enabled() {
        return None;
    }
    // Ensure FPS advances even if view profiling is off (FPS-only mode).
    Some(with_gui_profiler(|g| g.status_line()))
}

#[derive(Debug, Clone, Copy)]
pub struct ProcSnapshot {
    pub wall: Instant,
    /// User + system CPU time in nanoseconds.
    pub cpu_ns: u128,
    /// Resident set size in bytes (0 if unavailable).
    pub rss_bytes: u64,
}

impl ProcSnapshot {
    pub fn capture() -> Self {
        Self {
            wall: Instant::now(),
            cpu_ns: process_cpu_ns(),
            rss_bytes: process_rss_bytes(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct InteractionSample {
    pub kind: &'static str,
    pub name: String,
    pub wall_ns: u128,
    pub wall_ms: f64,
    pub cpu_ns: u128,
    /// CPU time as % of one logical core over the wall interval.
    pub cpu_pct_core: f64,
    /// CPU time as % of all logical cores.
    pub cpu_pct_machine: f64,
    pub rss_before: u64,
    pub rss_after: u64,
    pub rss_delta: i64,
    pub cores: u32,
}

impl InteractionSample {
    pub fn from_delta(
        kind: &'static str,
        name: impl Into<String>,
        before: &ProcSnapshot,
        after: &ProcSnapshot,
    ) -> Self {
        let wall_ns = after.wall.duration_since(before.wall).as_nanos().max(1);
        let cpu_ns = after.cpu_ns.saturating_sub(before.cpu_ns);
        let cores = logical_cores();
        let cpu_pct_core = (cpu_ns as f64 / wall_ns as f64) * 100.0;
        let cpu_pct_machine = if cores > 0 {
            cpu_pct_core / cores as f64
        } else {
            cpu_pct_core
        };
        Self {
            kind,
            name: name.into(),
            wall_ns,
            wall_ms: wall_ns as f64 / 1_000_000.0,
            cpu_ns,
            cpu_pct_core,
            cpu_pct_machine,
            rss_before: before.rss_bytes,
            rss_after: after.rss_bytes,
            rss_delta: after.rss_bytes as i64 - before.rss_bytes as i64,
            cores,
        }
    }

    pub fn emit(&self) {
        tracing::trace!(
            target: "profiler",
            kind = self.kind,
            name = %self.name,
            wall_ns = self.wall_ns,
            wall_ms = format!("{:.3}", self.wall_ms),
            cpu_ns = self.cpu_ns,
            cpu_pct_core = format!("{:.2}", self.cpu_pct_core),
            cpu_pct_machine = format!("{:.3}", self.cpu_pct_machine),
            cores = self.cores,
            rss_before = %format_bytes(self.rss_before),
            rss_after = %format_bytes(self.rss_after),
            rss_delta = %format_bytes_signed(self.rss_delta),
            rss_before_b = self.rss_before,
            rss_after_b = self.rss_after,
            rss_delta_b = self.rss_delta,
            "interaction"
        );
        if profile_env() {
            let hot = self.kind == "view"
                || self.name.starts_with("tick.")
                || self.name == "async.image"
                || self.name == "input.pointer"
                || self.name.starts_with("layout.");
            // Hot path → debug (still visible with profiler=info via trace? no — use info every N)
            // Emit ALL samples at info so `RUST_LOG` default shows background work.
            // Cap hot spam: still info but shorter field set is fine for diagnosis.
            if hot {
                tracing::debug!(
                    target: "fluxplay::profile",
                    kind = self.kind,
                    name = %self.name,
                    wall_ms = format!("{:.3}", self.wall_ms),
                    wall_ns = self.wall_ns,
                    cpu_pct_core = format!("{:.1}", self.cpu_pct_core),
                    rss_delta = %format_bytes_signed(self.rss_delta),
                    "interaction"
                );
            } else {
                tracing::info!(
                    target: "fluxplay::profile",
                    kind = self.kind,
                    name = %self.name,
                    wall_ns = self.wall_ns,
                    wall_ms = format!("{:.3}", self.wall_ms),
                    cpu_ns = self.cpu_ns,
                    cpu_pct_core = format!("{:.2}", self.cpu_pct_core),
                    cpu_pct_machine = format!("{:.3}", self.cpu_pct_machine),
                    cores = self.cores,
                    rss = %format_bytes(self.rss_after),
                    rss_delta = %format_bytes_signed(self.rss_delta),
                    "interaction"
                );
            }
        }
    }
}

/// RAII guard: profiles one interaction on drop when profiling is enabled.
pub struct InteractionGuard {
    kind: &'static str,
    name: String,
    before: Option<ProcSnapshot>,
    track_fps: bool,
}

impl InteractionGuard {
    pub fn begin(kind: &'static str, name: impl Into<String>) -> Self {
        Self::begin_ex(kind, name, false)
    }

    /// Profile a view/paint and advance the GUI FPS counter.
    pub fn begin_view(name: impl Into<String>) -> Self {
        Self::begin_ex("view", name, true)
    }

    fn begin_ex(kind: &'static str, name: impl Into<String>, track_fps: bool) -> Self {
        if !profiling_enabled() && !(track_fps && overlay_enabled()) {
            return Self {
                kind,
                name: String::new(),
                before: None,
                track_fps: track_fps && overlay_enabled(),
            };
        }
        let measure = profiling_enabled();
        Self {
            kind,
            name: name.into(),
            before: measure.then(ProcSnapshot::capture),
            track_fps: track_fps && (overlay_enabled() || profiling_enabled()),
        }
    }
}

impl Drop for InteractionGuard {
    fn drop(&mut self) {
        let sample = self.before.take().map(|before| {
            let after = ProcSnapshot::capture();
            let sample =
                InteractionSample::from_delta(self.kind, std::mem::take(&mut self.name), &before, &after);
            sample.emit();
            sample
        });

        if sample.is_some() || self.track_fps {
            with_gui_profiler(|g| {
                if let Some(sample) = sample {
                    match sample.kind {
                        "view" => g.mark_view_sample(sample),
                        _ => g.mark_update_sample(sample),
                    }
                } else if self.track_fps {
                    let fps = g.fps.mark_frame();
                    tracing::trace!(
                        target: "profiler",
                        kind = "fps",
                        fps = format!("{:.2}", fps),
                        frame_ns = g.fps.frame_ns(),
                        frame_ms = format!("{:.3}", g.fps.frame_ms()),
                        "gui.fps"
                    );
                }
            });
        }
    }
}

/// Rolling GUI FPS from view/paint calls.
#[derive(Debug)]
pub struct FpsCounter {
    stamps: VecDeque<Instant>,
    last_frame_ns: u128,
    fps: f32,
    window_secs: f32,
}

impl Default for FpsCounter {
    fn default() -> Self {
        Self {
            stamps: VecDeque::with_capacity(128),
            last_frame_ns: 0,
            fps: 0.0,
            window_secs: 1.0,
        }
    }
}

impl FpsCounter {
    pub fn mark_frame(&mut self) -> f32 {
        let now = Instant::now();
        if let Some(prev) = self.stamps.back() {
            self.last_frame_ns = now.duration_since(*prev).as_nanos();
        }
        self.stamps.push_back(now);
        let cutoff = now
            .checked_sub(std::time::Duration::from_secs_f32(self.window_secs))
            .unwrap_or(now);
        while self.stamps.front().is_some_and(|t| *t < cutoff) {
            self.stamps.pop_front();
        }
        let n = self.stamps.len();
        self.fps = if n >= 2 {
            let span = self
                .stamps
                .back()
                .unwrap()
                .duration_since(*self.stamps.front().unwrap())
                .as_secs_f32()
                .max(1e-6);
            (n - 1) as f32 / span
        } else {
            0.0
        };
        self.fps
    }

    pub fn fps(&self) -> f32 {
        self.fps
    }

    pub fn frame_ms(&self) -> f64 {
        self.last_frame_ns as f64 / 1_000_000.0
    }

    pub fn frame_ns(&self) -> u128 {
        self.last_frame_ns
    }

    pub fn overlay_line(&self) -> String {
        format!(
            "FPS {fps:.1} · frame {ms:.2}ms ({ns} ns)",
            fps = self.fps,
            ms = self.frame_ms(),
            ns = self.last_frame_ns
        )
    }
}

/// Thread-local last samples + FPS for the iced UI thread.
pub struct GuiProfiler {
    pub fps: FpsCounter,
    pub last_update: Option<InteractionSample>,
    pub last_view: Option<InteractionSample>,
}

impl Default for GuiProfiler {
    fn default() -> Self {
        Self {
            fps: FpsCounter::default(),
            last_update: None,
            last_view: None,
        }
    }
}

impl GuiProfiler {
    pub fn mark_view_sample(&mut self, sample: InteractionSample) {
        let fps = self.fps.mark_frame();
        tracing::trace!(
            target: "profiler",
            kind = "fps",
            fps = format!("{:.2}", fps),
            frame_ns = self.fps.frame_ns(),
            frame_ms = format!("{:.3}", self.fps.frame_ms()),
            "gui.fps"
        );
        self.last_view.replace(sample);
    }

    pub fn mark_update_sample(&mut self, sample: InteractionSample) {
        self.last_update.replace(sample);
    }

    pub fn status_line(&self) -> String {
        let mut parts = vec![self.fps.overlay_line()];
        if let Some(u) = &self.last_update {
            parts.push(format!(
                "upd {} {:.2}ms ({ns} ns) cpu={pct:.0}%/core rss {rss} ({delta})",
                u.name,
                u.wall_ms,
                ns = u.wall_ns,
                pct = u.cpu_pct_core,
                rss = format_bytes(u.rss_after),
                delta = format_bytes_signed(u.rss_delta)
            ));
        }
        if let Some(v) = &self.last_view {
            parts.push(format!(
                "view {:.2}ms cpu={:.0}% Δ{}",
                v.wall_ms,
                v.cpu_pct_core,
                format_bytes_signed(v.rss_delta)
            ));
        }
        parts.join(" · ")
    }
}

/// Human-readable byte size (B / KiB / MiB / GiB).
pub fn format_bytes(n: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let x = n as f64;
    if x >= GIB {
        format!("{:.2} GiB", x / GIB)
    } else if x >= MIB {
        format!("{:.2} MiB", x / MIB)
    } else if x >= KIB {
        format!("{:.2} KiB", x / KIB)
    } else {
        format!("{n} B")
    }
}

pub fn format_bytes_signed(delta: i64) -> String {
    if delta >= 0 {
        format!("+{}", format_bytes(delta as u64))
    } else {
        format!("-{}", format_bytes((-delta) as u64))
    }
}

fn logical_cores() -> u32 {
    static CORES: OnceLock<u32> = OnceLock::new();
    *CORES.get_or_init(|| {
        std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1)
            .max(1)
    })
}

#[cfg(unix)]
fn process_cpu_ns() -> u128 {
    // SAFETY: getrusage with RUSAGE_SELF is well-defined.
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) != 0 {
            return 0;
        }
        timeval_to_ns(usage.ru_utime) + timeval_to_ns(usage.ru_stime)
    }
}

#[cfg(unix)]
fn timeval_to_ns(tv: libc::timeval) -> u128 {
    (tv.tv_sec as u128) * 1_000_000_000 + (tv.tv_usec as u128) * 1_000
}

#[cfg(not(unix))]
fn process_cpu_ns() -> u128 {
    0
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn process_rss_bytes() -> u64 {
    let Ok(raw) = std::fs::read_to_string("/proc/self/statm") else {
        return 0;
    };
    let mut parts = raw.split_whitespace();
    let _size = parts.next();
    let Some(resident) = parts.next() else {
        return 0;
    };
    let Ok(pages) = resident.parse::<u64>() else {
        return 0;
    };
    pages.saturating_mul(page_size())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn page_size() -> u64 {
    static PAGE: OnceLock<u64> = OnceLock::new();
    *PAGE.get_or_init(|| {
        // SAFETY: sysconf(_SC_PAGESIZE) is safe.
        let n = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if n > 0 {
            n as u64
        } else {
            4096
        }
    })
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn process_rss_bytes() -> u64 {
    0
}

/// Enhanced stopwatch: wall + CPU + RSS when profiling is on.
pub struct ResourceStopwatch {
    name: &'static str,
    before: ProcSnapshot,
    active: bool,
}

impl ResourceStopwatch {
    pub fn start(name: &'static str) -> Self {
        let active = profiling_enabled();
        if active {
            tracing::trace!(target: "profiler", name, "start");
        }
        Self {
            name,
            before: if active {
                ProcSnapshot::capture()
            } else {
                ProcSnapshot {
                    wall: Instant::now(),
                    cpu_ns: 0,
                    rss_bytes: 0,
                }
            },
            active,
        }
    }

    pub fn lap(&self, step: &str) {
        if !self.active {
            return;
        }
        let now = ProcSnapshot::capture();
        let sample =
            InteractionSample::from_delta("task", format!("{}::{step}", self.name), &self.before, &now);
        tracing::trace!(
            target: "profiler",
            name = self.name,
            step,
            wall_ns = sample.wall_ns,
            wall_ms = format!("{:.3}", sample.wall_ms),
            cpu_pct_core = format!("{:.2}", sample.cpu_pct_core),
            rss_delta = %format_bytes_signed(sample.rss_delta),
            "lap"
        );
    }
}

impl Drop for ResourceStopwatch {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let after = ProcSnapshot::capture();
        let sample = InteractionSample::from_delta("task", self.name, &self.before, &after);
        sample.emit();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_scales() {
        assert_eq!(format_bytes(500), "500 B");
        assert!(format_bytes(2048).contains("KiB"));
        assert!(format_bytes(5 * 1024 * 1024).contains("MiB"));
    }

    #[test]
    fn fps_marks() {
        let mut fps = FpsCounter::default();
        fps.mark_frame();
        std::thread::sleep(std::time::Duration::from_millis(10));
        fps.mark_frame();
        assert!(fps.frame_ns() > 0);
    }
}
