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
const SOFT_PRESENT_MAX: u32 = 240;
const GUI_CAP_MAX: u32 = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplaySession {
    // Linux desktop session types — never probed on Android (tests construct them).
    #[cfg(any(test, not(target_os = "android")))]
    Wayland,
    #[cfg(any(test, not(target_os = "android")))]
    X11,
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
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
            Self::Android => "Android",
            #[cfg(any(test, not(target_os = "android")))]
            Self::Unknown => "?",
        }
    }
}

use crate::gpu_topology::GpuTopology;
#[cfg(any(test, not(target_os = "android")))]
use crate::gpu_topology::{topology_from_devices, GpuDevice};

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

    pub fn video_period_ms(&self) -> u64 {
        period_ms(self.video_hz)
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

pub fn period_ms(hz: u32) -> u64 {
    let hz = hz.max(1);
    ((1000.0 / f64::from(hz)).round() as u64).clamp(8, 1000)
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
    #[allow(unused_mut)] // mutated only inside the Android cfg block below
    let mut video_hz = resolve_pref(video_pref, env_u32("FLUXPLAY_VIDEO_FPS"), video_auto);
    #[cfg(target_os = "android")]
    {
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
    }

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

    let decode_edge_px = if probe.monitor_w >= 2560
        || probe.gpu_topology.display_tier() == GpuTier::Discrete
    {
        480
    } else if probe.monitor_w > 0 && probe.monitor_w <= 1366 {
        320
    } else {
        400
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
    let mut base = match tier {
        GpuTier::Discrete => 120,
        GpuTier::Integrated => {
            if cfg!(target_os = "android") {
                // Flagship MediaCodec-copy soft present targets 60fps @ ≤1080p.
                60
            } else {
                60
            }
        }
        // Android probe has no lspci — Unknown must not claim 90Hz soft RGBA.
        GpuTier::Unknown => {
            if cfg!(target_os = "android") {
                48
            } else {
                90
            }
        }
    };
    if on_battery {
        base = base.min(60);
    }
    // Soft RGBA upload is heavy — step caps, avoid cliff that flaps with ±1px resize.
    // Discrete can sustain 1080p @ 60–120; UHD stays conservative unless FLUXPLAY_SOFT_UHD.
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
        base.min(if matches!(tier, GpuTier::Discrete) {
            120
        } else {
            60
        })
    } else {
        base
    }
}

fn env_u32(key: &str) -> Option<u32> {
    std::env::var(key).ok()?.parse().ok()
}

fn probe_hardware() -> DisplayProbe {
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
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
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
    #[cfg(not(target_os = "android"))]
    {
        if let Some(v) = parse_xrandr(&run_cmd("xrandr", &[])) {
            return Some((v.0, v.1, v.2, v.3, "xrandr"));
        }
        if let Some(v) = parse_wlr_randr(&run_cmd("wlr-randr", &[])) {
            return Some((v.0, v.1, v.2, v.3, "wlr-randr"));
        }
        if let Some(v) = probe_drm_sysfs() {
            return Some((v.0, v.1, v.2, v.3, "drm"));
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
    let h = h
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
        let modes = std::fs::read_to_string(path.join("modes")).unwrap_or_default();
        let (hz, w, h) = modes
            .lines()
            .find_map(|line| {
                let hz = parse_drm_mode_hz(line).unwrap_or(DEFAULT_HZ);
                let (w, h) = parse_mode_wh(line.split('@').next().unwrap_or(line));
                if w > 0 {
                    Some((hz, w, h))
                } else {
                    None
                }
            })
            .unwrap_or((DEFAULT_HZ, 0, 0));
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
    #[cfg(not(target_os = "android"))]
    {
        let out = run_cmd("lspci", &["-nn"]);
        let mut devices = parse_lspci_gpus(&out);
        attach_drm_render_nodes(&mut devices);
        if devices.is_empty() {
            return GpuTopology::single("GPU", GpuTier::Unknown);
        }
        topology_from_devices(devices)
    }
}

#[allow(dead_code)]
fn probe_gpu() -> (String, GpuTier) {
    let t = probe_gpu_topology();
    (t.display.name.clone(), t.display_tier())
}

/// All VGA/3D controllers from lspci (multi-GPU).
#[cfg(any(test, not(target_os = "android")))]
pub fn parse_lspci_gpus(out: &str) -> Vec<GpuDevice> {
    let mut devices = Vec::new();
    for line in out.lines() {
        let lower = line.to_ascii_lowercase();
        if !(lower.contains("vga compatible")
            || lower.contains("3d controller")
            || lower.contains("display controller"))
        {
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

/// Best-effort: map PCI GPUs to `/dev/dri/renderD*` via sorted node list.
#[cfg(not(target_os = "android"))]
fn attach_drm_render_nodes(devices: &mut [GpuDevice]) {
    let Ok(entries) = std::fs::read_dir("/dev/dri") else {
        return;
    };
    let mut nodes: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().into_string().ok()?;
            if n.starts_with("renderD") {
                Some(format!("/dev/dri/{n}"))
            } else {
                None
            }
        })
        .collect();
    nodes.sort();
    for (dev, node) in devices.iter_mut().zip(nodes.into_iter()) {
        dev.render_node = Some(node);
    }
}

#[cfg(any(test, not(target_os = "android")))]
fn classify_gpu(lower: &str) -> (GpuTier, u8) {
    let nvidia = lower.contains("nvidia");
    let amd = lower.contains("amd") || lower.contains("ati");
    let intel = lower.contains("intel");
    if nvidia {
        return (GpuTier::Discrete, 0);
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
    if s.len() > 64 {
        s.truncate(61);
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
        assert_eq!(topo.decode.tier, GpuTier::Discrete);
        assert!(topo.requires_copy_path());
    }

    #[test]
    fn period_ms_round() {
        assert_eq!(period_ms(60), 17);
        assert_eq!(period_ms(120), 8);
        assert_eq!(period_ms(30), 33);
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
}
