//! Host / display capability probe → realistic runtime ceilings.
//!
//! ## What we actually detect (and why)
//! - **Monitor**: name, resolution, refresh Hz (xrandr / wlr-randr / DRM sysfs).
//!   → GUI/video tick periods, prefetch pacing, soft-present budget.
//! - **GPU**: PCI name + coarse tier (discrete vs integrated via `lspci`).
//!   → Soft-decode FPS budget, hwdec preference hint. We do **not** invent
//!   “GPU threads”, SM counts, or clocks — those are not reliably exposed and
//!   iced/mpv do not consume them usefully from userspace.
//! - **CPU**: logical cores + arch (`std::thread::available_parallelism`, `ARCH`).
//!   → Cap meta JoinSet / image inflight. Portal HTTP stays at 2 (429 risk).
//! - **Session / OS**: Wayland vs X11 vs Android; optional battery discharging.
//!   → Slightly quieter prefetch on battery; logging / diagnostics.
//!
//! Env overrides still win: `FLUXPLAY_MONITOR_HZ`, `FLUXPLAY_GUI_FPS`,
//! `FLUXPLAY_VIDEO_FPS`, `FLUXPLAY_META_PARALLEL`, `FLUXPLAY_IMAGE_INFLIGHT`.
// Linux probes (xrandr / DRM sysfs / lspci) stay compiled for their tests; Windows uses user32.

#[cfg(not(target_os = "android"))]
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use fluxplay_core::models::{FpsCapPref, GpuTier};
#[cfg(not(target_os = "android"))]
use tracing::debug;
use tracing::{info, warn};

const DEFAULT_HZ: u32 = 60;
const MIN_HZ: u32 = 24;
const MAX_HZ: u32 = 240;
/// Soft-present never needs to exceed this even on 360 Hz panels.
/// Matches `app.soft_present_hz` / `NativePlayer::set_present_hz` (AO-safe ≤60).
const SOFT_PRESENT_MAX: u32 = 60;
const GUI_CAP_MAX: u32 = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplaySession {
    // Linux desktop session types — never probed on Android (tests construct them).
    #[cfg(any(test, not(target_os = "android")))]
    Wayland,
    #[cfg(any(test, not(target_os = "android")))]
    X11,
    #[cfg(target_os = "android")]
    Android,
    #[cfg(any(test, not(target_os = "android")))]
    Unknown,
}

impl DisplaySession {
    pub fn label(self) -> &'static str {
        match self {
            #[cfg(any(test, not(target_os = "android")))]
            Self::Wayland => "Wayland",
            #[cfg(any(test, not(target_os = "android")))]
            Self::X11 => "X11",
            #[cfg(target_os = "android")]
            Self::Android => "Android",
            #[cfg(any(test, not(target_os = "android")))]
            Self::Unknown => "?",
        }
    }
}

use crate::gpu_topology::GpuTopology;
#[cfg(any(test, not(target_os = "android")))]
use crate::gpu_topology::GpuDevice;
// Linux/macOS probe + tests only (Windows builds the topology from user32; the
// function does not exist on Android).
#[cfg(any(test, all(not(windows), not(target_os = "android"))))]
use crate::gpu_topology::topology_from_devices;

#[derive(Debug, Clone)]
pub struct DisplayProbe {
    pub monitor_name: String,
    pub monitor_hz: u32,
    /// Active mode width/height when known (0 = unknown).
    pub monitor_w: u32,
    pub monitor_h: u32,
    pub gpu_name: String,
    pub gpu_tier: GpuTier,
    /// Full multi-GPU map (display caps vs decode preference).
    pub gpu_topology: GpuTopology,
    pub cpu_logical: u32,
    pub cpu_arch: &'static str,
    pub session: DisplaySession,
    pub os_label: String,
    /// Best-effort: laptop on battery (Linux sysfs). False if unknown.
    pub on_battery: bool,
    pub source: &'static str,
}

/// Derived concurrency / quality knobs — small, clamped, evidence-based.
#[derive(Debug, Clone, Copy)]
pub struct RuntimeTuning {
    /// OMDb/TVMaze/iTunes JoinSet size (1..=6).
    pub meta_parallel: usize,
    /// Parallel image HTTP+decode tasks (4..=32).
    pub image_inflight: usize,
    /// Prefetch batch per refresh (4..=24).
    pub image_prefetch: usize,
    /// Virtual mosaic overscan rows (6..=24).
    pub virtual_overscan: usize,
    /// UI image longest edge before GPU upload.
    pub decode_edge_px: u32,
    /// Xtream portal HTTP overlap — kept low on purpose.
    pub portal_parallel: usize,
}

#[derive(Debug, Clone)]
pub struct DisplayCaps {
    pub probe: DisplayProbe,
    pub gui_hz: u32,
    pub video_hz: u32,
    pub tuning: RuntimeTuning,
}

impl DisplayCaps {
    pub fn gui_period_ms(&self) -> u64 {
        period_ms(self.gui_hz)
    }

    pub fn summary_line(&self) -> String {
        let res = if self.probe.monitor_w > 0 && self.probe.monitor_h > 0 {
            format!("{}×{} ", self.probe.monitor_w, self.probe.monitor_h)
        } else {
            String::new()
        };
        let bat = if self.probe.on_battery { " · batterie" } else { "" };
        let gpu = if self.probe.gpu_topology.devices.len() > 1 {
            self.probe.gpu_topology.summary()
        } else {
            format!("{} ({})", self.probe.gpu_name, self.probe.gpu_tier.label())
        };
        format!(
            "{} · {}{}@ {} Hz · {} · {} · CPU {}×{} · GUI {} fps · vidéo {} fps · meta×{} img×{} portal×{}{}",
            self.probe.os_label,
            self.probe.monitor_name,
            res,
            self.probe.monitor_hz,
            self.probe.session.label(),
            gpu,
            self.probe.cpu_logical,
            self.probe.cpu_arch,
            self.gui_hz,
            self.video_hz,
            self.tuning.meta_parallel,
            self.tuning.image_inflight,
            self.tuning.portal_parallel,
            bat,
        )
    }
}

/// Tick period for a refresh rate (GUI / panel polls).
/// Soft present must use [`soft_period_ms`] — AO-safe ≤60 with a 16 ms floor.
pub fn period_ms(hz: u32) -> u64 {
    let hz = hz.max(1);
    ((1000.0 / f64::from(hz)).round() as u64).clamp(8, 1000)
}

/// Soft-present iced tick period. Clamps Hz to [`SOFT_PRESENT_MAX`] and never
/// schedules faster than 16 ms (an 8 ms floor raced the audio clock when callers
/// passed raw panel Hz by mistake).
pub fn soft_period_ms(hz: u32) -> u64 {
    period_ms(hz.min(SOFT_PRESENT_MAX)).max(16)
}

/// Probe once at process start for warm cache; full resolve still re-reads prefs.
pub fn boot_probe() -> DisplayProbe {
    probe_hardware()
}

pub fn resolve_caps(
    gui_pref: FpsCapPref,
    video_pref: FpsCapPref,
    stage_wh: Option<(u32, u32)>,
    content_fps: Option<f64>,
    cached: Option<&DisplayProbe>,
) -> DisplayCaps {
    let probe = cached.cloned().unwrap_or_else(probe_hardware);
    let monitor_hz = env_u32("FLUXPLAY_MONITOR_HZ")
        .unwrap_or(probe.monitor_hz)
        .clamp(MIN_HZ, MAX_HZ);

    let soft_budget = soft_video_budget(probe.gpu_topology.display_tier(), stage_wh, probe.on_battery);
    let mut gpu_gui_budget = match probe.gpu_topology.display_tier() {
        GpuTier::Discrete => 240,
        GpuTier::Integrated => 120,
        GpuTier::Unknown => {
            if cfg!(target_os = "android") {
                60
            } else {
                90
            }
        }
    };
    if probe.on_battery {
        gpu_gui_budget = gpu_gui_budget.min(90);
    }
    #[cfg(target_os = "android")]
    {
        // Soft present shares NativeActivity — dynamic SoC/MediaCodec budget.
        if let Some(caps) = crate::android_bridge::poll_android_device_caps() {
            let b = caps.soft_budget();
            if caps.select_present_mode().uses_surface()
                && crate::android_bridge::is_video_surface_ready()
            {
                // Live Surface session: no soft upload tax — track panel GUI Hz.
                gpu_gui_budget = gpu_gui_budget.max(b.gui_hz).min(caps.refresh_hz.max(60).min(120));
            } else {
                gpu_gui_budget = gpu_gui_budget.min(b.gui_hz);
            }
        } else {
            gpu_gui_budget = gpu_gui_budget.min(60);
        }
    }

    let gui_hz = resolve_pref(
        gui_pref,
        env_u32("FLUXPLAY_GUI_FPS"),
        monitor_hz.min(gpu_gui_budget).min(GUI_CAP_MAX),
    );
    let mut video_auto = monitor_hz.min(soft_budget).min(SOFT_PRESENT_MAX);
    // content_fps gates waste — soft path already skips when !dirty. Only apply when the
    // estimate looks stable (≥20); flaky estimated-vf-fps at startup must not pin us to 24.
    if let Some(cfps) = content_fps.filter(|f| *f >= 20.0 && f.is_finite()) {
        // +4 headroom (was +2): a fixed-period tick at exactly ceil(fps) phase-drifts
        // against the content cadence and skips under upload backpressure.
        let capped = (cfps.ceil() as u32).saturating_add(4).max(MIN_HZ);
        video_auto = video_auto.min(capped);
    }
    let video_hz = {
        let base = resolve_pref(video_pref, env_u32("FLUXPLAY_VIDEO_FPS"), video_auto);
        #[cfg(target_os = "android")]
        {
            let mut video_hz = base;
            if let Some(caps) = crate::android_bridge::poll_android_device_caps() {
                if caps.select_present_mode().uses_surface()
                    && crate::android_bridge::is_video_surface_ready()
                {
                    video_hz = video_hz.min(caps.refresh_hz.max(24));
                } else {
                    video_hz = video_hz.min(caps.soft_budget().video_hz);
                }
            } else {
                let cores = probe.cpu_logical.max(1);
                let soft_cap = if cores >= 8 { 30 } else { 24 };
                video_hz = video_hz.min(soft_cap);
            }
            video_hz
        }
        #[cfg(not(target_os = "android"))]
        {
            base
        }
    };

    let probe = DisplayProbe {
        monitor_hz,
        ..probe
    };
    let tuning = derive_tuning(&probe, gui_hz);

    DisplayCaps {
        probe,
        gui_hz: gui_hz.clamp(MIN_HZ, GUI_CAP_MAX),
        video_hz: video_hz.clamp(MIN_HZ, SOFT_PRESENT_MAX),
        tuning,
    }
}

fn derive_tuning(probe: &DisplayProbe, gui_hz: u32) -> RuntimeTuning {
    let cores = probe.cpu_logical.max(1);
    // Meta APIs rate-limit hard — more cores ≠ more parallel beyond ~4–6.
    let meta_auto = match cores {
        1..=2 => 2,
        3..=4 => 3,
        5..=8 => 4,
        _ => 5,
    };
    let meta_parallel = env_u32("FLUXPLAY_META_PARALLEL")
        .map(|v| v as usize)
        .unwrap_or(meta_auto)
        .clamp(1, 6);

    let mut image_inflight: usize = match (probe.gpu_topology.display_tier(), cores) {
        (GpuTier::Discrete, c) if c >= 8 => 28,
        (GpuTier::Discrete, _) => 22,
        (GpuTier::Integrated, c) if c >= 8 => 16,
        (GpuTier::Integrated, _) => 12,
        (GpuTier::Unknown, c) if c >= 8 => 20,
        _ => 14,
    };
    // High refresh → slightly more mosaic headroom while flinging.
    if gui_hz >= 120 {
        image_inflight += 4;
    } else if gui_hz <= 48 {
        image_inflight = image_inflight.saturating_sub(4);
    }
    if probe.on_battery {
        image_inflight = (image_inflight * 2 / 3).max(6);
    }
    let image_inflight = env_u32("FLUXPLAY_IMAGE_INFLIGHT")
        .map(|v| v as usize)
        .unwrap_or(image_inflight)
        .clamp(4, 32);

    let image_prefetch = ((gui_hz / 8).max(6) as usize)
        .min(24)
        .min(image_inflight);

    let mut virtual_overscan: usize = if gui_hz >= 120 {
        18
    } else if gui_hz >= 75 {
        14
    } else {
        10
    };
    if probe.monitor_h >= 1440 {
        virtual_overscan += 2;
    }
    if probe.on_battery {
        virtual_overscan = virtual_overscan.saturating_sub(4usize).max(6usize);
    }

    // Posters render at ~200–300 px tall: decoding bigger only burns RAM/VRAM.
    // Hard cap 300 for the ~100 Mo RSS budget (mosaic still looks fine).
    let decode_edge_px = if probe.monitor_w > 0 && probe.monitor_w <= 1366 {
        260
    } else {
        300
    };

    RuntimeTuning {
        meta_parallel,
        image_inflight,
        image_prefetch,
        virtual_overscan,
        decode_edge_px,
        // Documented hard cap — raising this hammers IPTV panels (429).
        portal_parallel: 2,
    }
}

fn resolve_pref(pref: FpsCapPref, env: Option<u32>, auto_hz: u32) -> u32 {
    if let Some(v) = env {
        return v.clamp(MIN_HZ, MAX_HZ);
    }
    // Fixed prefs are ceilings — never exceed the Auto soft/GPU budget.
    pref.fixed_hz()
        .unwrap_or(auto_hz)
        .min(auto_hz.max(MIN_HZ))
        .clamp(MIN_HZ, MAX_HZ)
}

fn soft_video_budget(tier: GpuTier, stage_wh: Option<(u32, u32)>, on_battery: bool) -> u32 {
    let (w, h) = stage_wh.unwrap_or((1280, 720));
    let pixels = w.saturating_mul(h);
    // Soft present is AO-safe ≤60 (`SOFT_PRESENT_MAX` / `set_present_hz`) — never
    // advertise 90/120 here; that lied in settings while iced still clamped.
    let mut base = match tier {
        GpuTier::Discrete | GpuTier::Integrated => 60,
        // Android probe has no lspci — Unknown must not claim high soft RGBA.
        GpuTier::Unknown => {
            if cfg!(target_os = "android") {
                48
            } else {
                60
            }
        }
    };
    if on_battery {
        base = base.min(60);
    }
    // Soft RGBA upload is heavy — step caps, avoid cliff that flaps with ±1px resize.
    if std::env::var_os("FLUXPLAY_SOFT_UHD").is_some() {
        base.min(60)
    } else if pixels > 2560 * 1440 {
        base.min(30)
    } else if pixels > 1920 * 1080 {
        // Slightly above 1080p (letterbox / DPI) — keep 60 on discrete, not a 30 cliff.
        base.min(if matches!(tier, GpuTier::Discrete) {
            60
        } else {
            45
        })
    } else if pixels > 1280 * 720 {
        base.min(60)
    } else {
        base
    }
}

fn env_u32(key: &str) -> Option<u32> {
    std::env::var(key).ok()?.parse().ok()
}

fn probe_hardware() -> DisplayProbe {
    // apply_gpu_topology_opts / ticks call resolve_caps without a cache — memoize the probe.
    static CACHE: OnceLock<DisplayProbe> = OnceLock::new();
    if let Some(p) = CACHE.get() {
        return p.clone();
    }
    let probed = probe_hardware_uncached();
    let _ = CACHE.set(probed.clone());
    probed
}

fn probe_hardware_uncached() -> DisplayProbe {
    static WARNED: OnceLock<()> = OnceLock::new();
    let topo = probe_gpu_topology();
    let gpu_name = topo.display.name.clone();
    let gpu_tier = topo.display_tier();
    let cpu_logical = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(2);
    let cpu_arch = std::env::consts::ARCH;
    let session = probe_session();
    let os_label = probe_os_label();
    let on_battery = probe_on_battery();

    if let Some((name, hz, w, h, source)) = probe_monitor() {
        let probe = DisplayProbe {
            monitor_name: name,
            monitor_hz: hz.clamp(MIN_HZ, MAX_HZ),
            monitor_w: w,
            monitor_h: h,
            gpu_name,
            gpu_tier,
            gpu_topology: topo,
            cpu_logical,
            cpu_arch,
            session,
            os_label,
            on_battery,
            source,
        };
        static LOGGED: OnceLock<()> = OnceLock::new();
        let _ = LOGGED.get_or_init(|| {
            info!(
                monitor = %probe.monitor_name,
                res = %format!("{}x{}", probe.monitor_w, probe.monitor_h),
                hz = probe.monitor_hz,
                gpu = %probe.gpu_topology.summary(),
                cpu = probe.cpu_logical,
                arch = probe.cpu_arch,
                session = %probe.session.label(),
                on_battery,
                source = probe.source,
                "host caps probed"
            );
        });
        return probe;
    }
    let _ = WARNED.get_or_init(|| {
        warn!("monitor refresh undetected — defaulting to {DEFAULT_HZ} Hz");
    });
    DisplayProbe {
        monitor_name: "Écran".into(),
        monitor_hz: DEFAULT_HZ,
        monitor_w: 0,
        monitor_h: 0,
        gpu_name,
        gpu_tier,
        gpu_topology: topo,
        cpu_logical,
        cpu_arch,
        session,
        os_label,
        on_battery,
        source: "fallback",
    }
}

fn probe_session() -> DisplaySession {
    #[cfg(target_os = "android")]
    {
        return DisplaySession::Android;
    }
    #[cfg(not(target_os = "android"))]
    {
        let wayland = std::env::var_os("WAYLAND_DISPLAY")
            .filter(|v| !v.is_empty())
            .is_some();
        let x11 = std::env::var_os("DISPLAY")
            .filter(|v| !v.is_empty())
            .is_some();
        if wayland {
            DisplaySession::Wayland
        } else if x11 {
            DisplaySession::X11
        } else {
            DisplaySession::Unknown
        }
    }
}

fn probe_os_label() -> String {
    #[cfg(target_os = "android")]
    {
        return "Android".into();
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(data) = std::fs::read_to_string("/etc/os-release") {
            let mut name = None;
            let mut ver = None;
            for line in data.lines() {
                if let Some(v) = line.strip_prefix("NAME=") {
                    name = Some(v.trim_matches('"').to_string());
                } else if let Some(v) = line.strip_prefix("VERSION_ID=") {
                    ver = Some(v.trim_matches('"').to_string());
                }
            }
            match (name, ver) {
                (Some(n), Some(v)) => return format!("{n} {v}"),
                (Some(n), None) => return n,
                _ => {}
            }
        }
        "Linux".into()
    }
    #[cfg(target_os = "macos")]
    {
        "macOS".into()
    }
    #[cfg(target_os = "windows")]
    {
        "Windows".into()
    }
    #[cfg(not(any(
        target_os = "android",
        target_os = "linux",
        target_os = "macos",
        target_os = "windows"
    )))]
    {
        std::env::consts::OS.into()
    }
}

fn probe_on_battery() -> bool {
    #[cfg(target_os = "linux")]
    {
        let root = std::path::Path::new("/sys/class/power_supply");
        let Ok(entries) = std::fs::read_dir(root) else {
            return false;
        };
        for ent in entries.flatten() {
            let p = ent.path();
            let ty = std::fs::read_to_string(p.join("type")).unwrap_or_default();
            if !ty.trim().eq_ignore_ascii_case("Battery") {
                continue;
            }
            let st = std::fs::read_to_string(p.join("status")).unwrap_or_default();
            if st.trim().eq_ignore_ascii_case("Discharging") {
                return true;
            }
        }
        false
    }
    #[cfg(target_os = "macos")]
    {
        // `pmset -g batt` prints "Now drawing from 'Battery Power'" vs 'AC Power'.
        let out = run_cmd("pmset", &["-g", "batt"]);
        parse_macos_on_battery(&out)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

/// Parse `pmset -g batt` for discharging / battery-power draw.
#[cfg(any(target_os = "macos", all(test, not(target_os = "android"))))]
fn parse_macos_on_battery(out: &str) -> bool {
    let lower = out.to_ascii_lowercase();
    if lower.contains("drawing from 'ac power'") || lower.contains("drawing from \"ac power\"") {
        return false;
    }
    lower.contains("drawing from 'battery power'")
        || lower.contains("drawing from \"battery power\"")
        || (lower.contains("discharging") && !lower.contains("charging"))
}

fn probe_monitor() -> Option<(String, u32, u32, u32, &'static str)> {
    // Waydroid / Android expose host DRM via /sys — do not treat laptop eDP as the Activity.
    #[cfg(target_os = "android")]
    {
        // Prefer live device_caps refresh Hz when available.
        if let Some(caps) = crate::android_bridge::poll_android_device_caps() {
            if caps.refresh_hz >= 24 {
                let (sw, sh) = crate::android_bridge::video_surface_size()
                    .unwrap_or((0, 0));
                return Some((
                    format!("{} {}", caps.manufacturer, caps.model),
                    caps.refresh_hz.clamp(MIN_HZ, MAX_HZ),
                    sw,
                    sh,
                    "android-display",
                ));
            }
        }
        return Some(("Android".into(), DEFAULT_HZ, 0, 0, "android"));
    }
    #[cfg(windows)]
    {
        // xrandr / wlr-randr / DRM sysfs are Linux-only: ask user32 directly.
        let (w, h, hz) = win_display::primary_mode()?;
        let name = win_display::adapters()
            .into_iter()
            .find(|a| a.primary)
            .map(|a| a.name)
            .unwrap_or_else(|| "Écran principal".into());
        return Some((name, hz.clamp(MIN_HZ, MAX_HZ), w, h, "win32"));
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(v) = probe_macos_display() {
            return Some((v.0, v.1, v.2, v.3, "macos"));
        }
        None
    }
    #[cfg(all(target_os = "linux", not(target_os = "android")))]
    {
        // Wayland: prefer wlr-randr / DRM active `mode`. xrandr via XWayland often
        // reports a stale `*` mode and lies about panel Hz (NativeGpu resample).
        let wayland = std::env::var_os("WAYLAND_DISPLAY")
            .is_some_and(|v| !v.is_empty());
        if wayland {
            if let Some(v) = parse_wlr_randr(&run_cmd("wlr-randr", &[])) {
                return Some((v.0, v.1, v.2, v.3, "wlr-randr"));
            }
            if let Some(v) = probe_drm_sysfs() {
                return Some((v.0, v.1, v.2, v.3, "drm"));
            }
            if let Some(v) = parse_xrandr(&run_cmd("xrandr", &[])) {
                return Some((v.0, v.1, v.2, v.3, "xrandr"));
            }
        } else {
            if let Some(v) = parse_xrandr(&run_cmd("xrandr", &[])) {
                return Some((v.0, v.1, v.2, v.3, "xrandr"));
            }
            if let Some(v) = probe_drm_sysfs() {
                return Some((v.0, v.1, v.2, v.3, "drm"));
            }
        }
        None
    }
}

#[cfg(not(target_os = "android"))]
fn run_cmd(bin: &str, args: &[&str]) -> String {
    match Command::new(bin).args(args).output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
        Ok(out) => {
            debug!(%bin, status = ?out.status, "probe command failed");
            String::new()
        }
        Err(e) => {
            debug!(%bin, error = %e, "probe command missing");
            String::new()
        }
    }
}

#[cfg(any(test, not(target_os = "android")))]
fn parse_mode_wh(mode: &str) -> (u32, u32) {
    let mode = mode.trim();
    let Some((w, h)) = mode.split_once('x') else {
        return (0, 0);
    };
    // macOS `system_profiler` uses "3024 x 1964 Retina" (spaces around x).
    let w = w.trim().chars().take_while(|c| c.is_ascii_digit()).collect::<String>();
    let h = h
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>();
    (
        w.parse().unwrap_or(0),
        h.parse().unwrap_or(0),
    )
}

/// Parse `xrandr` current mode marked with `*`. Returns (name, hz, w, h).
#[cfg(any(test, not(target_os = "android")))]
pub fn parse_xrandr(out: &str) -> Option<(String, u32, u32, u32)> {
    let mut current: Option<String> = None;
    let mut best: Option<(String, u32, u32, u32)> = None;
    for line in out.lines() {
        if line.contains(" connected") {
            let name = line.split_whitespace().next()?.to_string();
            if line.contains(" primary") || current.is_none() {
                current = Some(name);
            }
            continue;
        }
        let Some(name) = current.as_ref() else {
            continue;
        };
        let trimmed = line.trim();
        if !trimmed.contains('*') {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let mode = parts.next()?;
        let (mw, mh) = parse_mode_wh(mode);
        for tok in parts {
            let rate = tok.trim_end_matches(['*', '+']);
            if let Ok(f) = rate.parse::<f64>() {
                if (20.0..=360.0).contains(&f) {
                    let hz = f.round() as u32;
                    best = Some((name.clone(), hz, mw, mh));
                    if out
                        .lines()
                        .any(|l| l.contains(" primary") && l.starts_with(name.as_str()))
                    {
                        return best;
                    }
                }
            }
        }
    }
    best
}

/// Parse `wlr-randr` current mode. Returns (name, hz, w, h).
#[cfg(any(test, not(target_os = "android")))]
pub fn parse_wlr_randr(out: &str) -> Option<(String, u32, u32, u32)> {
    let mut name: Option<String> = None;
    let mut current_block = false;
    for line in out.lines() {
        if !line.starts_with(' ') && !line.starts_with('\t') && !line.is_empty() {
            name = line.split_whitespace().next().map(|s| s.to_string());
            current_block = false;
            continue;
        }
        let t = line.trim();
        if t == "current" || t.starts_with("current ") {
            current_block = true;
            continue;
        }
        if current_block {
            // "1920x1080 px, 59.961002 Hz (preferred)"
            if let Some(idx) = t.find(" Hz") {
                let before = &t[..idx];
                let (mw, mh) = before
                    .split(',')
                    .next()
                    .map(|s| parse_mode_wh(s.replace(" px", "").trim()))
                    .unwrap_or((0, 0));
                if let Some(rate_s) = before.split(',').next_back() {
                    let rate_s = rate_s.split_whitespace().last()?;
                    if let Ok(f) = rate_s.parse::<f64>() {
                        if let Some(n) = &name {
                            return Some((n.clone(), f.round() as u32, mw, mh));
                        }
                    }
                }
            }
            if t.starts_with("position") || t.starts_with("transform") || t.starts_with("scale") {
                current_block = false;
            }
        }
    }
    None
}

#[cfg(not(target_os = "android"))]
fn probe_drm_sysfs() -> Option<(String, u32, u32, u32)> {
    let drm = std::path::Path::new("/sys/class/drm");
    let entries = std::fs::read_dir(drm).ok()?;
    let mut connected: Vec<(String, u32, u32, u32)> = Vec::new();
    for ent in entries.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        if !name.contains('-') || name.starts_with("render") {
            continue;
        }
        let path = ent.path();
        let status = std::fs::read_to_string(path.join("status")).unwrap_or_default();
        if !status.trim().eq_ignore_ascii_case("connected") {
            continue;
        }
        // Prefer the active CRTC mode (`mode`) over the first listed available mode.
        let mut hz = DEFAULT_HZ;
        let mut w = 0u32;
        let mut h = 0u32;
        if let Ok(cur) = std::fs::read_to_string(path.join("mode")) {
            let cur = cur.trim();
            if !cur.is_empty() {
                let (cw, ch) = parse_mode_wh(cur.split('@').next().unwrap_or(cur));
                if cw > 0 {
                    w = cw;
                    h = ch;
                    hz = parse_drm_mode_hz(cur).unwrap_or(DEFAULT_HZ);
                }
            }
        }
        let modes = std::fs::read_to_string(path.join("modes")).unwrap_or_default();
        if w == 0 {
            if let Some((bh, mw, mh)) = pick_drm_modes_fallback(&modes) {
                hz = bh;
                w = mw;
                h = mh;
            }
        } else if hz == DEFAULT_HZ {
            // Active mode often lacks @Hz — match WxH in `modes` for refresh.
            for line in modes.lines() {
                let (mw, mh) = parse_mode_wh(line.split('@').next().unwrap_or(line));
                if mw == w && mh == h {
                    if let Some(hertz) = parse_drm_mode_hz(line) {
                        hz = hertz;
                        break;
                    }
                }
            }
        }
        let nice = name
            .trim_start_matches("card0-")
            .trim_start_matches("card1-")
            .trim_start_matches("card2-")
            .to_string();
        connected.push((nice, hz, w, h));
    }
    connected.sort_by(|a, b| rank_connector(&a.0).cmp(&rank_connector(&b.0)));
    connected.into_iter().next()
}

/// macOS: `system_profiler SPDisplaysDataType` for panel Hz / resolution.
#[cfg(target_os = "macos")]
fn probe_macos_display() -> Option<(String, u32, u32, u32)> {
    let out = run_cmd("system_profiler", &["SPDisplaysDataType", "-detailLevel", "mini"]);
    parse_macos_system_profiler(&out)
}

#[cfg(any(target_os = "macos", all(test, not(target_os = "android"))))]
fn parse_macos_system_profiler(out: &str) -> Option<(String, u32, u32, u32)> {
    let mut name = String::from("Display");
    let mut hz = DEFAULT_HZ;
    let mut w = 0u32;
    let mut h = 0u32;
    for line in out.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Resolution:") {
            // "3024 x 1964 Retina" or "1920 x 1080 @ 60 Hz"
            let (mw, mh) = parse_mode_wh(rest.trim());
            if mw > 0 {
                w = mw;
                h = mh;
            }
            if let Some(at) = rest.find('@') {
                if let Some(hertz) = parse_drm_mode_hz(&rest[at..]) {
                    hz = hertz;
                }
            }
        }
        if let Some(rest) = t.strip_prefix("UI Looks like:") {
            let (mw, mh) = parse_mode_wh(rest.trim());
            if mw > 0 {
                w = mw;
                h = mh;
            }
        }
        if t.starts_with("Main Display:") && t.contains("Yes") {
            // keep current name
        } else if !t.contains(':') && !t.is_empty() && !t.starts_with("Graphics") {
            // Display product lines are often bare names under the GPU block.
            if t.len() > 2 && t.len() < 64 && !t.starts_with("Chipset") {
                name = t.to_string();
            }
        }
        // ProMotion is adaptive (often 10–120). Never invent 120 when profiler
        // only says "Supported" — keep parsed Hz (usually 60) until an explicit
        // "N Hz" / "Maximum Refresh Rate" token appears below.
        if let Some(idx) = t.find(" Hz") {
            let before = &t[..idx];
            if let Some(tok) = before.split_whitespace().last() {
                if let Ok(f) = tok.parse::<f64>() {
                    if (20.0..=360.0).contains(&f) {
                        hz = f.round() as u32;
                    }
                }
            }
        }
    }
    if w == 0 && hz == DEFAULT_HZ && name == "Display" {
        return None;
    }
    Some((name, hz.clamp(MIN_HZ, MAX_HZ), w, h))
}

#[cfg(not(target_os = "android"))]
fn rank_connector(name: &str) -> u8 {
    let u = name.to_ascii_uppercase();
    if u.contains("EDP") {
        0
    } else if u.contains("DP") {
        1
    } else if u.contains("HDMI") {
        2
    } else {
        3
    }
}

#[cfg(not(target_os = "android"))]
fn parse_drm_mode_hz(line: &str) -> Option<u32> {
    if let Some((_, rest)) = line.split_once('@') {
        let num: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if let Ok(f) = num.parse::<f64>() {
            if f >= 20.0 {
                return Some(f.round() as u32);
            }
        }
    }
    None
}

/// When DRM sysfs `mode` is empty, pick a honest fallback from `modes`.
/// Never advertise the max listed Hz (144 listed while the session is 60).
/// Prefer marked preferred (`+`/`*`), else first ≤60, else first clamped to DEFAULT.
#[cfg(not(target_os = "android"))]
fn pick_drm_modes_fallback(modes: &str) -> Option<(u32, u32, u32)> {
    let mut preferred: Option<(u32, u32, u32)> = None;
    let mut first_ok: Option<(u32, u32, u32)> = None;
    let mut first_any: Option<(u32, u32, u32)> = None;
    for line in modes.lines() {
        let line_hz = parse_drm_mode_hz(line).unwrap_or(DEFAULT_HZ);
        let (mw, mh) = parse_mode_wh(line.split('@').next().unwrap_or(line));
        if mw == 0 {
            continue;
        }
        let marked = line.contains('+') || line.contains('*');
        if marked && preferred.is_none() {
            preferred = Some((line_hz, mw, mh));
        }
        if line_hz <= 60 && first_ok.is_none() {
            first_ok = Some((line_hz, mw, mh));
        }
        if first_any.is_none() {
            first_any = Some((line_hz.min(DEFAULT_HZ), mw, mh));
        }
    }
    let used_preferred = preferred.is_some();
    preferred.or(first_ok).or(first_any).map(|(bh, mw, mh)| {
        let hz = if used_preferred {
            bh
        } else {
            bh.min(DEFAULT_HZ).max(24)
        };
        (hz, mw, mh)
    })
}

fn probe_gpu_topology() -> GpuTopology {
    #[cfg(target_os = "android")]
    {
        // One SoC GPU + MediaCodec blocks — not two GL adapters. Caps = display SoC.
        if let Some((name, tier)) = crate::android_bridge::poll_device_gpu() {
            return GpuTopology::single(name, tier);
        }
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let name = if cores >= 8 {
            "Android SoC (flagship)"
        } else {
            "Android GPU"
        };
        return GpuTopology::single(name, GpuTier::Integrated);
    }
    #[cfg(windows)]
    {
        // `adapters()` lists the adapter driving the primary display first.
        // Hybrid laptops often expose a discrete second adapter — NativeGpu
        // zero-copy across adapters blacks; force copy when tiers diverge.
        // `render_node` carries the full DXGI/GDI description for mpv
        // `--d3d11-adapter=` (prefix match). UI `name` may be truncated; never
        // pin the ellipsis-truncated form (would fail soft_set / no-op CycleGpu).
        let devices: Vec<GpuDevice> = win_display::adapters()
            .into_iter()
            .map(|a| {
                let (tier, _) = classify_gpu(&a.name.to_ascii_lowercase());
                GpuDevice {
                    name: truncate_gpu_name(&a.name),
                    tier,
                    render_node: Some(a.name),
                }
            })
            .collect();
        let Some(display) = devices.first().cloned() else {
            return GpuTopology::single("GPU", GpuTier::Unknown);
        };
        let hybrid = devices.len() > 1
            && devices.iter().any(|d| d.tier == GpuTier::Discrete)
            && devices.iter().any(|d| d.tier == GpuTier::Integrated);
        let decode = if hybrid {
            devices
                .iter()
                .find(|d| d.tier == GpuTier::Discrete)
                .cloned()
                .unwrap_or_else(|| display.clone())
        } else {
            display.clone()
        };
        let policy = if hybrid && decode.name != display.name {
            crate::gpu_topology::GpuDecodePolicy::StrongDecodeCopy
        } else {
            crate::gpu_topology::GpuDecodePolicy::SameAsDisplay
        };
        return GpuTopology {
            devices,
            decode,
            display,
            policy,
        };
    }
    #[cfg(target_os = "macos")]
    {
        // lspci is absent on Darwin — classify Apple Silicon / Metal GPU as Integrated.
        let out = run_cmd("system_profiler", &["SPDisplaysDataType", "-detailLevel", "mini"]);
        let lower = out.to_ascii_lowercase();
        let name = if lower.contains("apple m") || lower.contains("apple gpu") {
            "Apple GPU".to_string()
        } else if let Some(line) = out.lines().find(|l| {
            let t = l.trim();
            t.starts_with("Chipset Model:")
        }) {
            truncate_gpu_name(line.split(':').nth(1).unwrap_or("GPU").trim())
        } else {
            "GPU".to_string()
        };
        let tier = if name.to_ascii_lowercase().contains("apple")
            || lower.contains("apple m")
        {
            GpuTier::Integrated
        } else {
            let (t, _) = classify_gpu(&name.to_ascii_lowercase());
            t
        };
        return GpuTopology::single(name, tier);
    }
    #[cfg(all(target_os = "linux", not(target_os = "android")))]
    {
        let out = run_cmd("lspci", &["-nn"]);
        let mut devices = parse_lspci_gpus(&out);
        attach_drm_render_nodes(&mut devices, &lspci_gpu_slots(&out));
        if devices.is_empty() {
            return GpuTopology::single("GPU", GpuTier::Unknown);
        }
        topology_from_devices(devices)
    }
}

/// All VGA/3D controllers from lspci (multi-GPU).
#[cfg(any(test, not(target_os = "android")))]
pub fn parse_lspci_gpus(out: &str) -> Vec<GpuDevice> {
    let mut devices = Vec::new();
    for line in out.lines() {
        let lower = line.to_ascii_lowercase();
        if !is_lspci_gpu_line(&lower) {
            continue;
        }
        let name = line
            .split(':')
            .nth(2)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| line.trim().to_string());
        let short = truncate_gpu_name(&name);
        let (tier, _) = classify_gpu(&lower);
        devices.push(GpuDevice {
            name: short,
            tier,
            render_node: None,
        });
    }
    devices
}

#[cfg(test)]
pub fn parse_lspci_gpu(out: &str) -> Option<(String, GpuTier)> {
    let devices = parse_lspci_gpus(out);
    if devices.is_empty() {
        return None;
    }
    let topo = topology_from_devices(devices);
    let tier = topo.display_tier();
    Some((topo.display.name, tier))
}

#[cfg(any(test, not(target_os = "android")))]
fn is_lspci_gpu_line(lower: &str) -> bool {
    lower.contains("vga compatible")
        || lower.contains("3d controller")
        || lower.contains("display controller")
}

/// PCI slot (`01:00.0`) of each GPU line, same order as [`parse_lspci_gpus`].
#[cfg(any(test, not(target_os = "android")))]
fn lspci_gpu_slots(out: &str) -> Vec<String> {
    out.lines()
        .filter(|line| is_lspci_gpu_line(&line.to_ascii_lowercase()))
        .map(|line| line.split_whitespace().next().unwrap_or("").to_string())
        .collect()
}

/// Map each GPU to its `/dev/dri/renderD*` by PCI slot (sysfs `device` link).
/// Node numbering does not follow lspci order (hybrid laptops list the dGPU
/// first while the iGPU owns renderD128). Unknown stays `None` — a wrong node
/// makes libva load the other vendor's driver and decode falls back to CPU.
#[cfg(not(target_os = "android"))]
fn attach_drm_render_nodes(devices: &mut [GpuDevice], slots: &[String]) {
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return;
    };
    let nodes: Vec<(String, String)> = entries
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().into_string().ok()?;
            if !n.starts_with("renderD") {
                return None;
            }
            let pci = std::fs::read_link(e.path().join("device")).ok()?;
            let pci = pci.file_name()?.to_str()?.to_string();
            Some((pci, format!("/dev/dri/{n}")))
        })
        .collect();
    for (dev, slot) in devices.iter_mut().zip(slots) {
        dev.render_node = render_node_for_slot(&nodes, slot);
    }
}

/// `slot` is lspci's short form (`01:00.0`); sysfs uses `0000:01:00.0`.
#[cfg(any(test, not(target_os = "android")))]
fn render_node_for_slot(nodes: &[(String, String)], slot: &str) -> Option<String> {
    if slot.is_empty() {
        return None;
    }
    nodes
        .iter()
        .find(|(pci, _)| pci == slot || pci.ends_with(&format!(":{slot}")))
        .map(|(_, node)| node.clone())
}

#[cfg(any(test, not(target_os = "android")))]
fn classify_gpu(lower: &str) -> (GpuTier, u8) {
    // Whole words: "ati" is also in "Corporation".
    let word = |w: &str| lower.split(|c: char| !c.is_ascii_alphanumeric()).any(|t| t == w);
    let nvidia = lower.contains("nvidia");
    let amd = word("amd") || word("ati") || lower.contains("radeon");
    let intel = lower.contains("intel");
    // Apple Silicon (macOS probe + chipset name fallback).
    if lower.contains("apple") || word("m1") || word("m2") || word("m3") || word("m4") {
        return (GpuTier::Integrated, 2);
    }
    if nvidia {
        return (GpuTier::Discrete, 0);
    }
    if intel && (word("arc") || lower.contains("dg2") || lower.contains("battlemage")) {
        return (GpuTier::Discrete, 1);
    }
    if amd
        && (lower.contains("radeon 6")
            || lower.contains("radeon 7")
            || lower.contains("rembrandt")
            || lower.contains("raphael")
            || lower.contains("phoenix")
            || lower.contains("graphics")
            || lower.contains("apu")
            || lower.contains("680m")
            || lower.contains("780m")
            || lower.contains("vega"))
        && !lower.contains("rx ")
        && !lower.contains("[radeon pro")
    {
        return (GpuTier::Integrated, 2);
    }
    if amd && (lower.contains("rx ") || lower.contains("navi") || lower.contains("radeon pro")) {
        return (GpuTier::Discrete, 1);
    }
    if intel {
        return (GpuTier::Integrated, 3);
    }
    if amd {
        return (GpuTier::Integrated, 2);
    }
    (GpuTier::Unknown, 4)
}

#[cfg(any(test, not(target_os = "android")))]
fn truncate_gpu_name(name: &str) -> String {
    let mut s = name.to_string();
    if let Some(idx) = s.rfind('[') {
        if s[idx..].contains(':') {
            s.truncate(idx);
            s = s.trim().to_string();
        }
    }
    if s.chars().count() > 64 {
        s = s.chars().take(61).collect();
        s.push('…');
    }
    s
}

/// How long before we re-probe monitor (hotplug / move to another screen).
pub fn probe_stale_after() -> Duration {
    Duration::from_secs(5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xrandr_primary_star() {
        let out = r#"
Screen 0: minimum 16 x 16, current 1920 x 1080
eDP-1 connected primary 1920x1080+0+0
   1920x1080     59.96*+
   1280x720      59.90
HDMI-1 connected 3840x2160+1920+0
   3840x2160     60.00*
"#;
        let (name, hz, w, h) = parse_xrandr(out).unwrap();
        assert_eq!(name, "eDP-1");
        assert_eq!(hz, 60);
        assert_eq!((w, h), (1920, 1080));
    }

    #[test]
    fn xrandr_high_refresh() {
        let out = r#"
DP-1 connected primary 2560x1440+0+0
   2560x1440    144.00*+
"#;
        let (_, hz, w, h) = parse_xrandr(out).unwrap();
        assert_eq!(hz, 144);
        assert_eq!((w, h), (2560, 1440));
    }

    #[test]
    fn wlr_randr_current() {
        let out = r#"
eDP-1 "BOE 0x0A1C"
  Make: BOE
  current
    1920x1080 px, 59.961002 Hz (preferred)
  position 0,0
"#;
        let (name, hz, w, h) = parse_wlr_randr(out).unwrap();
        assert_eq!(name, "eDP-1");
        assert_eq!(hz, 60);
        assert_eq!((w, h), (1920, 1080));
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn macos_system_profiler_promotion() {
        let out = r#"
Graphics/Displays:

    Apple M2:

      Chipset Model: Apple M2
      Displays:
        Color LCD:
          Resolution: 3024 x 1964 Retina
          UI Looks like: 1512 x 982
          Main Display: Yes
          Mirror: Off
          Online: Yes
          Automatically Adjust Brightness: Yes
          Connection Type: Internal
          ProMotion: Supported
"#;
        let (_name, hz, w, h) = parse_macos_system_profiler(out).unwrap();
        // "ProMotion: Supported" alone must not invent 120 — adaptive range.
        assert_eq!(hz, 60, "ProMotion without explicit Hz stays at default 60");
        assert!(w >= 1512);
        assert!(h >= 982);
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn macos_pmset_battery_power() {
        let batt = "Now drawing from 'Battery Power'\n -InternalBattery-0\t50%; discharging; 3:00 remaining present: true\n";
        assert!(parse_macos_on_battery(batt));
        let ac = "Now drawing from 'AC Power'\n -InternalBattery-0\t80%; charged; 0:00 remaining present: true\n";
        assert!(!parse_macos_on_battery(ac));
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn classify_gpu_apple_silicon_integrated() {
        assert_eq!(classify_gpu("apple m2").0, GpuTier::Integrated);
        assert_eq!(classify_gpu("chipset model: apple m3 pro").0, GpuTier::Integrated);
        assert_eq!(classify_gpu("apple gpu").0, GpuTier::Integrated);
    }

    #[test]
    fn lspci_prefers_nvidia_discrete() {
        let out = r#"
01:00.0 VGA compatible controller: NVIDIA Corporation AD107M [GeForce RTX 4050 Max-Q / Mobile] [10de:28e1]
75:00.0 VGA compatible controller: Advanced Micro Devices, Inc. [AMD/ATI] Rembrandt [Radeon 680M] [1002:1681]
"#;
        let (name, tier) = parse_lspci_gpu(out).unwrap();
        // Display role prefers iGPU on hybrid; decode can be discrete.
        assert_eq!(tier, GpuTier::Integrated);
        assert!(name.to_ascii_lowercase().contains("radeon") || name.contains("Rembrandt") || name.contains("680M"));
        let topo = topology_from_devices(parse_lspci_gpus(out));
        assert!(topo.is_hybrid());
        assert_eq!(topo.display.tier, GpuTier::Integrated);
        assert_eq!(topo.decode.tier, GpuTier::Integrated);
        assert!(!topo.requires_copy_path());
    }

    #[test]
    fn period_ms_round() {
        assert_eq!(period_ms(60), 17);
        assert_eq!(period_ms(120), 8);
        assert_eq!(period_ms(30), 33);
        assert_eq!(soft_period_ms(60), 17);
        assert_eq!(soft_period_ms(120), 17, "soft clamps to ≤60 before period");
        assert_eq!(soft_period_ms(240), 17);
        assert!(soft_period_ms(90) >= 16);
    }

    #[test]
    #[cfg(not(target_os = "android"))]
    fn drm_modes_fallback_never_picks_max_hz() {
        // Empty active `mode` with 144 listed must not advertise 144.
        let modes = "\
3840x2160@144.00
1920x1080@60.00
1280x720@60.00
";
        let (hz, w, h) = pick_drm_modes_fallback(modes).unwrap();
        assert_eq!(hz, 60, "must not pick max listed 144 without active mode");
        assert_eq!((w, h), (1920, 1080));

        let preferred = "\
3840x2160@144.00+
1920x1080@60.00
";
        let (hz, _, _) = pick_drm_modes_fallback(preferred).unwrap();
        assert_eq!(hz, 144, "marked preferred may exceed 60");

        let only_high = "\
2560x1440@165.00
2560x1440@144.00
";
        let (hz, _, _) = pick_drm_modes_fallback(only_high).unwrap();
        assert_eq!(hz, 60, "no ≤60 mode → clamp first to DEFAULT, never max");
    }

    #[test]
    fn soft_video_budget_clamps_above_720p_on_igpu() {
        // Soft RGBA path: >720p on iGPU must not claim 120 Hz (jank / thermal).
        let hz_720 = soft_video_budget(GpuTier::Integrated, Some((1280, 720)), false);
        let hz_1080 = soft_video_budget(GpuTier::Integrated, Some((1920, 1080)), false);
        let hz_uhd = soft_video_budget(GpuTier::Integrated, Some((3840, 2160)), false);
        assert!(hz_720 <= 60);
        assert!(hz_1080 <= 60);
        assert!(hz_uhd <= 30);
        assert_eq!(SOFT_PRESENT_MAX, 60);
    }

    #[test]
    fn decode_edge_stays_at_or_under_300_for_rss_plan() {
        let probe = DisplayProbe {
            monitor_name: "t".into(),
            monitor_hz: 60,
            monitor_w: 1920,
            monitor_h: 1080,
            gpu_name: "g".into(),
            gpu_tier: GpuTier::Integrated,
            gpu_topology: GpuTopology::single("g", GpuTier::Integrated),
            cpu_logical: 8,
            cpu_arch: "x86_64",
            session: DisplaySession::Wayland,
            os_label: "Test".into(),
            on_battery: false,
            source: "test",
        };
        let t = derive_tuning(&probe, 60);
        assert!(
            t.decode_edge_px <= 300,
            "decode_edge_px={} — plan caps posters at 300px",
            t.decode_edge_px
        );
    }

    #[test]
    fn soft_video_budget_discrete_keeps_headroom_at_1080p() {
        let hz = soft_video_budget(GpuTier::Discrete, Some((1920, 1080)), false);
        assert!(hz >= 60, "discrete 1080p soft present too low: {hz}");
        // Budget may report >60; resolve_caps clamps video_hz with SOFT_PRESENT_MAX.
        assert!(hz.min(SOFT_PRESENT_MAX) <= 60);
    }

    #[test]
    fn tuning_clamps_meta() {
        let probe = DisplayProbe {
            monitor_name: "t".into(),
            monitor_hz: 60,
            monitor_w: 1920,
            monitor_h: 1080,
            gpu_name: "g".into(),
            gpu_tier: GpuTier::Integrated,
            gpu_topology: GpuTopology::single("g", GpuTier::Integrated),
            cpu_logical: 16,
            cpu_arch: "x86_64",
            session: DisplaySession::Wayland,
            os_label: "Test".into(),
            on_battery: false,
            source: "test",
        };
        let t = derive_tuning(&probe, 60);
        assert!(t.meta_parallel <= 6);
        assert_eq!(t.portal_parallel, 2);
        assert!(t.image_inflight >= 4);
    }

    #[test]
    fn render_nodes_follow_pci_slot_not_lspci_order() {
        let out = "01:00.0 VGA compatible controller [0300]: NVIDIA Corporation AD107M [GeForce RTX 4050 Max-Q / Mobile] [10de:28a1] (rev a1)\n\
                   75:00.0 VGA compatible controller [0300]: Advanced Micro Devices, Inc. [AMD/ATI] Rembrandt [Radeon 680M] [1002:1681] (rev 0b)\n";
        let slots = lspci_gpu_slots(out);
        assert_eq!(slots, vec!["01:00.0".to_string(), "75:00.0".to_string()]);
        let nodes = vec![
            ("0000:75:00.0".to_string(), "/dev/dri/renderD128".to_string()),
            ("0000:01:00.0".to_string(), "/dev/dri/renderD129".to_string()),
        ];
        assert_eq!(
            render_node_for_slot(&nodes, &slots[0]).as_deref(),
            Some("/dev/dri/renderD129")
        );
        assert_eq!(
            render_node_for_slot(&nodes, &slots[1]).as_deref(),
            Some("/dev/dri/renderD128")
        );
        assert_eq!(render_node_for_slot(&nodes, "02:00.0"), None);
    }
}

/// Monitor mode and display adapters through user32 (no external tools on Windows).
#[cfg(windows)]
mod win_display {
    /// `DEVMODEW` (220 bytes); only `dmSize` and the display fields are touched.
    #[repr(C, align(4))]
    struct DevModeW([u8; 220]);

    #[repr(C)]
    struct DisplayDeviceW {
        cb: u32,
        device_name: [u16; 32],
        device_string: [u16; 128],
        state_flags: u32,
        device_id: [u16; 128],
        device_key: [u16; 128],
    }

    #[link(name = "user32")]
    extern "system" {
        fn EnumDisplaySettingsW(device: *const u16, mode_num: u32, mode: *mut DevModeW) -> i32;
        fn EnumDisplayDevicesW(
            device: *const u16,
            index: u32,
            out: *mut DisplayDeviceW,
            flags: u32,
        ) -> i32;
    }

    const ENUM_CURRENT_SETTINGS: u32 = 0xFFFF_FFFF;
    const DISPLAY_DEVICE_ATTACHED_TO_DESKTOP: u32 = 0x1;
    const DISPLAY_DEVICE_PRIMARY_DEVICE: u32 = 0x4;
    const DISPLAY_DEVICE_MIRRORING_DRIVER: u32 = 0x8;

    fn read_u32(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    }

    /// (width, height, refresh Hz) of the primary display's current mode.
    pub fn primary_mode() -> Option<(u32, u32, u32)> {
        let mut dm = DevModeW([0; 220]);
        dm.0[68..70].copy_from_slice(&220u16.to_le_bytes()); // dmSize
        // SAFETY: `dm` is a zeroed DEVMODEW-sized buffer with dmSize set.
        if unsafe { EnumDisplaySettingsW(std::ptr::null(), ENUM_CURRENT_SETTINGS, &mut dm) } == 0 {
            return None;
        }
        let (w, h, hz) = (read_u32(&dm.0, 172), read_u32(&dm.0, 176), read_u32(&dm.0, 184));
        // 0/1 mean "hardware default" — not a usable rate.
        (hz > 1).then_some((w, h, hz))
    }

    pub struct Adapter {
        pub name: String,
        pub primary: bool,
    }

    /// Physical display adapters, primary first; mirror/basic-render drivers skipped.
    pub fn adapters() -> Vec<Adapter> {
        let mut out: Vec<Adapter> = Vec::new();
        for index in 0..16 {
            let mut dd = DisplayDeviceW {
                cb: std::mem::size_of::<DisplayDeviceW>() as u32,
                device_name: [0; 32],
                device_string: [0; 128],
                state_flags: 0,
                device_id: [0; 128],
                device_key: [0; 128],
            };
            // SAFETY: `dd.cb` is set to the struct size as the API requires.
            if unsafe { EnumDisplayDevicesW(std::ptr::null(), index, &mut dd, 0) } == 0 {
                break;
            }
            if dd.state_flags & DISPLAY_DEVICE_MIRRORING_DRIVER != 0 {
                continue;
            }
            let end = dd.device_string.iter().position(|&c| c == 0).unwrap_or(128);
            let name = String::from_utf16_lossy(&dd.device_string[..end]).trim().to_string();
            let lower = name.to_ascii_lowercase();
            if name.is_empty() || lower.contains("basic render") || lower.contains("remote display")
            {
                continue;
            }
            let primary = dd.state_flags & DISPLAY_DEVICE_PRIMARY_DEVICE != 0;
            let attached = dd.state_flags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP != 0;
            if let Some(a) = out.iter_mut().find(|a| a.name == name) {
                a.primary |= primary;
                continue;
            }
            if attached || primary || out.is_empty() {
                out.push(Adapter { name, primary });
            }
        }
        out.sort_by_key(|a| !a.primary);
        out
    }
}
