//! Native playback backends — **libmpv** / **libav* FFmpeg** in-process (embedded RGBA),
//! plus optional CLI mpv/ffplay fallback.
//! Inspired by IPTVnator embedded MPV and Kodi's FFmpeg pipeline.

use std::net::Shutdown;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
#[cfg(not(target_os = "android"))] // IPC_SEQ (CLI player sockets)
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(not(target_os = "android"))] // ipc_socket_path
use std::time::{SystemTime, UNIX_EPOCH};

use fluxplay_core::models::PlayerBackendPref;
use fluxplay_core::Stopwatch;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, trace, warn};

use crate::{url_endpoint, PlayerError, Result};

#[cfg(not(target_os = "android"))] // CLI/IPC player path — desktop only
static IPC_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendId {
    Mpv,
    Ffmpeg,
    External,
    ExoPlayer,
    AvPlayer,
}

impl BackendId {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mpv => "libmpv (natif)",
            Self::Ffmpeg => "FFmpeg (natif)",
            Self::External => "Lecteur système",
            Self::ExoPlayer => "ExoPlayer (Android)",
            Self::AvPlayer => "AVPlayer (iOS)",
        }
    }
}

/// What the active backend can actually honor (UI must gate on this).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendCaps {
    pub pause: bool,
    pub volume_live: bool,
    pub mute: bool,
    pub seek_rel: bool,
    pub seek_abs: bool,
    pub times: bool,
    pub speed_loop: bool,
    pub screenshot: bool,
    pub tracks_filters: bool,
    pub owned: bool,
}

impl BackendCaps {
    pub const NONE: Self = Self {
        pause: false,
        volume_live: false,
        mute: false,
        seek_rel: false,
        seek_abs: false,
        times: false,
        speed_loop: false,
        screenshot: false,
        tracks_filters: false,
        owned: false,
    };

    fn mpv_full() -> Self {
        Self {
            pause: true,
            volume_live: true,
            mute: true,
            seek_rel: true,
            seek_abs: true,
            times: true,
            speed_loop: true,
            screenshot: true,
            tracks_filters: true,
            owned: true,
        }
    }

    fn libffmpeg() -> Self {
        Self {
            pause: true,
            volume_live: true,
            mute: true,
            seek_rel: true,
            seek_abs: true,
            times: true,
            speed_loop: false,
            screenshot: true,
            tracks_filters: false,
            owned: true,
        }
    }

    fn ffplay_cli() -> Self {
        Self {
            pause: true,
            volume_live: false,
            mute: true,
            seek_rel: true,
            seek_abs: false,
            times: false,
            speed_loop: false,
            screenshot: false,
            tracks_filters: false,
            owned: true,
        }
    }
}

fn preferred_hwdec() -> &'static str {
    #[cfg(target_os = "android")]
    {
        // Soft RGBA (vo=libmpv) needs CPU-readable frames. `mediacodec-copy` is the
        // vendor-agnostic path: Qualcomm/MediaTek/Exynos/Tensor/Unisoc all expose
        // MediaCodec; mpv downloads NV12/YUV into SW for soft present.
        // Plain `mediacodec` (zero-copy Surface) cannot feed vo=libmpv SW.
        // Env override for labs: FLUXPLAY_HWDEC=mediacodec-copy|auto-safe|no
        if let Ok(v) = std::env::var("FLUXPLAY_HWDEC") {
            match v.to_ascii_lowercase().as_str() {
                "no" | "none" | "software" => return "no",
                "auto" | "auto-safe" => return "auto-safe",
                "auto-copy" => return "auto-copy",
                "mediacodec" => return "mediacodec-copy", // force copy for soft VO
                other if !other.is_empty() => {
                    // Leak a static for rare overrides (mediacodec-copy, …).
                    return Box::leak(other.to_string().into_boxed_str());
                }
                _ => {}
            }
        }
        "mediacodec-copy"
    }
    #[cfg(not(target_os = "android"))]
    {
        // Soft embed / hybrid: always copy path. Prefer auto-copy.
        if let Ok(v) = std::env::var("FLUXPLAY_HWDEC") {
            let lower = v.to_ascii_lowercase();
            if matches!(
                lower.as_str(),
                "no" | "none" | "software" | "auto" | "auto-safe" | "auto-copy"
                    | "vaapi" | "vaapi-copy" | "cuda" | "nvdec" | "vulkan" | "d3d11va"
                    | "dxva2" | "videotoolbox"
            ) {
                return Box::leak(lower.into_boxed_str());
            }
        }
        "auto-copy"
    }
}

/// Android soft-present scale — prefer budget from live caps when provided.
#[cfg(target_os = "android")]
fn android_soft_vf_from_budget(budget: &str) -> String {
    budget.to_string()
}

#[cfg(target_os = "android")]
fn android_soft_vf_fallback() -> String {
    // Last resort when caps were never probed (should be rare).
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4) as u32;
    let caps = crate::AndroidDeviceCaps {
        cores,
        mediacodec_video: true,
        refresh_hz: 60,
        ..Default::default()
    };
    caps.soft_budget().vf_scale()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendInfo {
    pub id: BackendId,
    pub available: bool,
    pub path: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayOptions {
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    pub extra_headers: Vec<(String, String)>,
    pub hwdec: bool,
    /// Network cache in milliseconds (IPTV smoothness).
    pub cache_ms: u32,
    /// Demuxer readahead / buffer for unstable IPTV links.
    pub demux_secs: f32,
    pub volume: f32,
    pub low_latency: bool,
    pub preferred: PlayerBackendPref,
    /// App-scoped proxy (e.g. `socks5h://127.0.0.1:PORT` from WireGuard userspace).
    #[serde(default)]
    pub http_proxy: Option<String>,
    /// Android present path (Surface embed vs soft RGBA). Ignored on desktop.
    #[serde(default)]
    pub android_present: crate::AndroidPresentMode,
    /// Dynamic soft vf chain from [`crate::AndroidDeviceCaps::soft_budget`].
    #[serde(default)]
    pub android_soft_vf: Option<String>,
    /// Android `wid` = GlobalRef Surface jobject pointer (Phase A).
    #[serde(default)]
    pub android_surface_wid: Option<i64>,
    /// Android `android-surface-size` WxH when known.
    #[serde(default)]
    pub android_surface_wh: Option<(u32, u32)>,
    /// Linux DRM render node for VA-API when hybrid (`/dev/dri/renderD129`).
    #[serde(default)]
    pub vaapi_device: Option<String>,
    /// When true, prefer `*-copy` hwdec (cross-GPU or soft present).
    #[serde(default)]
    pub hwdec_force_copy: bool,
    /// Soft-path HDR→SDR tone-mapping (mpv tone-mapping=hable).
    #[serde(default = "default_tonemap_on")]
    pub tonemap_hdr: bool,
    /// User quality ceiling (360p–4K). Soft path only; Surface keeps bitstream.
    #[serde(default)]
    pub video_max_wh: Option<(u32, u32)>,
    /// HDR / gamut preference.
    #[serde(default)]
    pub hdr_mode: fluxplay_core::models::HdrPref,
    /// LED vs AMOLED tone.
    #[serde(default)]
    pub display_panel: fluxplay_core::models::DisplayPanelPref,
}

fn default_tonemap_on() -> bool {
    true
}

/// Screen-space rectangle for the mpv video surface.
///
/// `anchored = true`  → overlay: absolute `WxH+X+Y`, borderless, stays over iced stage.
/// `anchored = false` → detached (typical Wayland): size-only `WxH`, normal window
///                      managed by the compositor (no fake absolute coords).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub anchored: bool,
}

impl VideoRect {
    pub fn overlay(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self {
            x,
            y,
            w,
            h,
            anchored: true,
        }
    }

    pub fn detached(w: u32, h: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            w,
            h,
            anchored: false,
        }
    }

    pub fn to_geometry(self) -> String {
        if self.anchored {
            format!("{}x{}{:+}{:+}", self.w, self.h, self.x, self.y)
        } else {
            format!("{}x{}", self.w, self.h)
        }
    }

    pub fn is_usable(self) -> bool {
        self.w >= 64 && self.h >= 64
    }
}

impl Default for PlayOptions {
    fn default() -> Self {
        Self {
            // Same as IPTV Smarters Pro / Expert
            user_agent: Some("IPTVSmartersPlayer".into()),
            referer: None,
            extra_headers: Vec::new(),
            hwdec: true,
            cache_ms: 4000,
            demux_secs: 8.0,
            volume: 0.85,
            low_latency: false,
            preferred: PlayerBackendPref::Auto,
            http_proxy: None,
            android_present: crate::AndroidPresentMode::default(),
            android_soft_vf: None,
            android_surface_wid: None,
            android_surface_wh: None,
            vaapi_device: None,
            hwdec_force_copy: false,
            tonemap_hdr: true,
            video_max_wh: None,
            hdr_mode: Default::default(),
            display_panel: Default::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum PlayerEvent {
    Started { backend: BackendId },
    Stopped,
    Error(String),
}

/// Detect available desktop backends (linked libmpv and/or PATH tools).
pub fn detect_backends() -> Vec<BackendInfo> {
    let mut out = Vec::new();

    #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
    {
        out.push(BackendInfo {
            id: BackendId::Mpv,
            available: true,
            path: Some("libmpv".into()),
            detail: if cfg!(feature = "static-link") {
                "libmpv lié statiquement — HLS/DASH/RTSP/HW".into()
            } else {
                "libmpv natif (FFI) — HLS/DASH/RTSP/RTMP/SRT, HW accel".into()
            },
        });
    }
    #[cfg(not(all(feature = "native-mpv", fluxplay_has_libmpv)))]
    {
        let mpv = which("mpv");
        out.push(BackendInfo {
            id: BackendId::Mpv,
            available: mpv.is_some(),
            path: mpv.clone(),
            detail: if mpv.is_some() {
                "mpv CLI (fallback)".into()
            } else {
                "Installez libmpv (dev) pour le FFI natif, ou mpv en CLI".into()
            },
        });
    }

    #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
    {
        out.push(BackendInfo {
            id: BackendId::Ffmpeg,
            available: true,
            path: Some("libav*".into()),
            detail: "FFmpeg natif (libav*) — RGBA embarqué dans iced".into(),
        });
    }
    #[cfg(all(
        not(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg)),
        target_os = "android",
        feature = "native-mpv",
        fluxplay_has_libmpv
    ))]
    {
        // Do not advertise a separate FFmpeg backend on Android — Pref::Ffmpeg
        // remaps to libmpv. Listing both confuses settings.
    }
    #[cfg(all(
        not(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg)),
        not(all(
            target_os = "android",
            feature = "native-mpv",
            fluxplay_has_libmpv
        ))
    ))]
    {
        let ffplay = which("ffplay");
        let ffmpeg_only = ffplay.is_none() && which("ffmpeg").is_some();
        out.push(BackendInfo {
            id: BackendId::Ffmpeg,
            // CLI path needs ffplay; bare ffmpeg cannot play without native embed.
            available: ffplay.is_some(),
            path: ffplay.clone().or_else(|| which("ffmpeg")),
            detail: if ffplay.is_some() {
                "ffplay CLI — fenêtre OS (pas d’embed; recompilez avec native-ffmpeg)".into()
            } else if ffmpeg_only {
                "ffmpeg sans ffplay — installez ffplay ou ffmpeg-devel pour l’embed".into()
            } else {
                "FFmpeg optionnel (libmpv suffit)".into()
            },
        });
    }

    out.push(BackendInfo {
        id: BackendId::External,
        available: true,
        path: None,
        detail: if cfg!(target_os = "android") {
            "ACTION_VIEW Intent".into()
        } else {
            "OS default handler / VLC / IINA".into()
        },
    });

    #[cfg(target_os = "ios")]
    out.push(BackendInfo {
        id: BackendId::AvPlayer,
        available: true,
        path: None,
        detail: "AVPlayer via Swift shell".into(),
    });

    debug!(
        count = out.len(),
        available = out.iter().filter(|b| b.available).count(),
        "detect_backends"
    );
    for b in &out {
        trace!(?b.id, available = b.available, path = ?b.path, "backend candidate");
    }
    out
}

fn looks_like_mpeg_ts(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.contains(".ts") && !lower.contains(".m3u8"))
        || lower.contains("mpegts")
        || lower.contains("format=ts")
}

fn looks_like_vod_container(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("/movie/")
        || lower.contains("/series/")
        || lower.contains(".mp4")
        || lower.contains(".mkv")
        || lower.contains(".avi")
        || lower.contains(".m4v")
        || lower.contains(".mov")
}

#[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
fn soft_set(mpv: &crate::mpv_ffi::LibMpv, name: &str, value: &str) -> bool {
    match mpv.set_option(name, value) {
        Ok(()) => true,
        Err(e) => {
            debug!(%name, error = %e, "libmpv soft option skipped");
            false
        }
    }
}

/// Strip CR/LF so referer / header values cannot inject lavf or mpv HTTP headers.
fn sanitize_http_field(s: &str) -> String {
    s.chars().filter(|c| *c != '\r' && *c != '\n').collect()
}

#[cfg(not(target_os = "android"))] // CLI player log — desktop only
fn tail_player_log() -> String {
    let path = std::env::temp_dir().join("fluxplay-mpv.log");
    std::fs::read_to_string(&path)
        .ok()
        .map(|s| {
            s.lines()
                .rev()
                .take(6)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" · ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
}

#[allow(dead_code)]
/// Legacy helper — prefer letting mpv/ffplay follow redirects (avoids burning CDN tokens).
fn resolve_playback_url(url: &str, ua: &str) -> Option<String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }
    let heavy_ts = looks_like_mpeg_ts(url);
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(if heavy_ts { 8 } else { 12 }))
        .user_agent(ua)
        .build()
        .ok()?;

    let mut current = url.to_string();
    for _ in 0..10 {
        let mut req = client.get(&current);
        if heavy_ts || looks_like_mpeg_ts(&current) {
            req = req.header(reqwest::header::RANGE, "bytes=0-0");
        }
        let resp = match req.send() {
            Ok(r) => r,
            Err(_) => return if current == url { None } else { Some(current) },
        };
        let status = resp.status();
        if status.is_redirection() {
            let loc = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            drop(resp);
            let Some(loc) = loc else {
                break;
            };
            current = resolve_relative_url(&current, &loc);
            continue;
        }
        if status.is_success() || status.as_u16() == 206 {
            drop(resp);
            return Some(current);
        }
        drop(resp);
        break;
    }
    if current == url {
        None
    } else {
        Some(current)
    }
}

fn resolve_relative_url(base: &str, loc: &str) -> String {
    if loc.starts_with("http://") || loc.starts_with("https://") {
        return loc.to_string();
    }
    if let Ok(base_u) = url::Url::parse(base) {
        if let Ok(joined) = base_u.join(loc) {
            return joined.to_string();
        }
    }
    loc.to_string()
}

fn which(bin: &str) -> Option<String> {
    let from_cmd = Command::new("which")
        .arg(bin)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        })
        .or_else(|| {
            Command::new("where")
                .arg(bin)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .next()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                })
        });
    if from_cmd.is_some() {
        return from_cmd;
    }
    // GUI launches often miss Homebrew / ~/.local in PATH.
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            candidates.push(dir.join(bin).display().to_string());
        }
    }
    if bin == "mpv" {
        candidates.extend([
            "/home/linuxbrew/.linuxbrew/bin/mpv".into(),
            "/usr/local/bin/mpv".into(),
            "/usr/bin/mpv".into(),
            "/opt/homebrew/bin/mpv".into(),
        ]);
    }
    if bin == "ffplay" || bin == "ffmpeg" {
        candidates.extend([
            format!("/home/linuxbrew/.linuxbrew/bin/{bin}"),
            format!("/usr/local/bin/{bin}"),
            format!("/usr/bin/{bin}"),
            format!("/opt/homebrew/bin/{bin}"),
        ]);
    }
    candidates.into_iter().find(|p| Path::new(p).is_file())
}

fn pick_backend(pref: PlayerBackendPref) -> Result<BackendId> {
    let available: Vec<_> = detect_backends()
        .into_iter()
        .filter(|b| b.available)
        .map(|b| b.id)
        .collect();

    let choose = |id: BackendId| available.contains(&id);

    let result = match pref {
        PlayerBackendPref::Auto => {
            if choose(BackendId::Mpv) {
                Ok(BackendId::Mpv)
            } else if choose(BackendId::Ffmpeg) {
                Ok(BackendId::Ffmpeg)
            } else {
                Ok(BackendId::External)
            }
        }
        PlayerBackendPref::Mpv => {
            if choose(BackendId::Mpv) {
                Ok(BackendId::Mpv)
            } else {
                Err(PlayerError::Backend(
                    "libmpv indisponible — recompilez avec native-mpv ou installez mpv".into(),
                ))
            }
        }
        PlayerBackendPref::Ffmpeg => {
            // Android: no separate libav* — libmpv (media-kit) embeds lavc.
            #[cfg(all(target_os = "android", feature = "native-mpv", fluxplay_has_libmpv))]
            {
                if choose(BackendId::Mpv) {
                    return Ok(BackendId::Mpv);
                }
            }
            if choose(BackendId::Ffmpeg) {
                Ok(BackendId::Ffmpeg)
            } else {
                Err(PlayerError::Backend("ffmpeg/ffplay introuvable".into()))
            }
        }
        PlayerBackendPref::External => Ok(BackendId::External),
    };
    match &result {
        Ok(id) => debug!(?pref, ?id, "pick_backend"),
        Err(e) => warn!(?pref, error = %e, "pick_backend failed"),
    }
    result
}

/// Controls native playback (in-process libmpv / FFmpeg, optional CLI child).
pub struct NativePlayer {
    backend: Option<BackendId>,
    #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
    libmpv: Option<std::sync::Arc<std::sync::Mutex<crate::mpv_ffi::LibMpv>>>,
    #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
    soft_pump: Option<crate::soft_pump::SoftPump>,
    #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
    libffmpeg: Option<crate::ffmpeg_ffi::LibFfmpeg>,
    child: Option<Child>,
    ipc_path: Option<PathBuf>,
    opts: PlayOptions,
    /// Borderless mpv window locked to the iced player stage.
    video_rect: Option<VideoRect>,
    /// Last mute state sent to ffplay CLI (`m` is a toggle — keep edge-only).
    ffplay_muted: bool,
    /// Last pause state for ffplay CLI (`space` is a toggle).
    ffplay_paused: bool,
    /// Last soft RGBA frame (libmpv / libffmpeg) for screenshot fallback.
    last_soft_rgba: Option<(u32, u32, Arc<[u8]>)>,
    soft_shot_tick: u32,
    /// Active Android present path (soft vs Surface).
    android_present: crate::AndroidPresentMode,
}

impl Default for NativePlayer {
    fn default() -> Self {
        Self::new(PlayOptions::default())
    }
}

impl NativePlayer {
    pub fn new(opts: PlayOptions) -> Self {
        debug!(
            preferred = ?opts.preferred,
            hwdec = opts.hwdec,
            cache_ms = opts.cache_ms,
            volume = opts.volume,
            "NativePlayer::new"
        );
        Self {
            backend: None,
            #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
            libmpv: None,
            #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
            soft_pump: None,
            #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
            libffmpeg: None,
            child: None,
            ipc_path: None,
            opts,
            video_rect: None,
            ffplay_muted: false,
            ffplay_paused: false,
            last_soft_rgba: None,
            soft_shot_tick: 0,
            android_present: crate::AndroidPresentMode::default(),
        }
    }


    #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
    fn lock_mpv(&self) -> Option<std::sync::MutexGuard<'_, crate::mpv_ffi::LibMpv>> {
        self.libmpv.as_ref()?.lock().ok()
    }

    /// Non-blocking variant for per-tick UI polls — never stall the UI behind a
    /// software render holding the mpv lock (returns None; callers keep last value).
    #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
    fn try_lock_mpv(&self) -> Option<std::sync::MutexGuard<'_, crate::mpv_ffi::LibMpv>> {
        self.libmpv.as_ref()?.try_lock().ok()
    }

    #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
    fn stop_soft_pump(&mut self) {
        if let Some(pump) = self.soft_pump.take() {
            pump.stop();
        }
    }

    #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
    fn ensure_soft_pump(&mut self) {
        if self.android_present.uses_surface() {
            self.stop_soft_pump();
            return;
        }
        if self.soft_pump.is_some() {
            return;
        }
        if let Some(arc) = self.libmpv.clone() {
            self.soft_pump = Some(crate::soft_pump::SoftPump::start(arc));
            tracing::info!("soft-pump started (off-UI mpv render)");
        }
    }

    /// Capability mask for UI gating (honest vs optimistic session mirrors).
    pub fn caps(&self) -> BackendCaps {
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if self.libmpv.is_some() {
            return BackendCaps::mpv_full();
        }
        if self.ipc_path.is_some() {
            return BackendCaps::mpv_full();
        }
        if self.using_libffmpeg() {
            return BackendCaps::libffmpeg();
        }
        if self.using_ffplay_cli() {
            return BackendCaps::ffplay_cli();
        }
        if self.backend == Some(BackendId::External) {
            return BackendCaps::NONE;
        }
        BackendCaps::NONE
    }

    /// Honest status label (embed vs CLI).
    pub fn display_label(&self) -> &'static str {
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if self.libmpv.is_some() {
            return "libmpv (embed)";
        }
        if self.ipc_path.is_some() {
            return "mpv (CLI)";
        }
        if self.using_libffmpeg() {
            return "FFmpeg (embed)";
        }
        if self.using_ffplay_cli() {
            return "ffplay (CLI)";
        }
        self.backend.map(|b| b.label()).unwrap_or("—")
    }

    /// Retained screenshot fallback, capped at ~1080p: a full 4K RGBA frame is ~33 MB
/// held for the whole session for an occasional PNG capture — nearest-neighbor
/// downscale keeps the fallback cheap (quality is irrelevant for a fallback).
fn snapshot_scaled(w: u32, h: u32, pixels: &[u8]) -> (u32, u32, Arc<[u8]>) {
    const MAX_EDGE: u32 = 1920;
    if w <= MAX_EDGE && h <= MAX_EDGE {
        return (w, h, Arc::from(pixels));
    }
    let scale = MAX_EDGE as f32 / w.max(h) as f32;
    let dw = (((w as f32 * scale) as u32).max(2) & !1).min(w);
    let dh = (((h as f32 * scale) as u32).max(2) & !1).min(h);
    let mut out = vec![0u8; (dw as usize) * (dh as usize) * 4];
    for y in 0..dh {
        let sy = ((y as u64 * h as u64) / dh as u64).min(h as u64 - 1) as u32;
        let srow = &pixels[(sy as usize) * (w as usize) * 4..][..(w as usize) * 4];
        let drow = &mut out[(y as usize) * (dw as usize) * 4..][..(dw as usize) * 4];
        for x in 0..dw {
            let sx = ((x as u64 * w as u64) / dw as u64).min(w as u64 - 1) as u32;
            drow[(x as usize) * 4..][..4].copy_from_slice(&srow[(sx as usize) * 4..][..4]);
        }
    }
    (dw, dh, Arc::from(out.as_slice()))
}

/// mpv `paused-for-cache` — drives Buffering state.
    pub fn paused_for_cache(&self) -> bool {
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.try_lock_mpv() {
            return mpv.get_property_flag("paused-for-cache").unwrap_or(false);
        }
        if let Some(path) = &self.ipc_path {
            return mpv_get_bool(path, "paused-for-cache") == Some(true);
        }
        false
    }

    pub fn last_soft_rgba(&self) -> Option<(u32, u32, &[u8])> {
        self.last_soft_rgba
            .as_ref()
            .map(|(w, h, b)| (*w, *h, b.as_ref()))
    }
    fn using_ffplay_cli(&self) -> bool {
        if self.backend != Some(BackendId::Ffmpeg) || self.child.is_none() {
            return false;
        }
        #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
        {
            self.libffmpeg.is_none()
        }
        #[cfg(not(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg)))]
        {
            true
        }
    }

    fn using_libffmpeg(&self) -> bool {
        #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
        {
            self.libffmpeg.is_some()
        }
        #[cfg(not(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg)))]
        {
            false
        }
    }

    pub fn options_mut(&mut self) -> &mut PlayOptions {
        &mut self.opts
    }

    /// Pin the video surface size for embedded software rendering (logical stage).
    pub fn set_video_rect(&mut self, rect: VideoRect) {
        if !rect.is_usable() {
            return;
        }
        if self.video_rect == Some(rect) {
            return;
        }
        debug!(?rect, "NativePlayer::set_video_rect");
        self.video_rect = Some(rect);
        if !self.has_embedded_video() {
            self.apply_video_geometry();
        }
    }

    pub fn video_rect(&self) -> Option<VideoRect> {
        self.video_rect
    }

    /// Pull an RGBA frame for the iced stage (embedded libmpv / FFmpeg software render).
    /// Returns `None` when there is no new frame — keep the previous ImageHandle.
    /// Android Surface present modes never produce soft frames.
    pub fn pull_video_frame(&mut self, w: u32, h: u32) -> Option<(u32, u32, Vec<u8>)> {
        if self.android_present.uses_surface() {
            return None;
        }
        let rw = (w.clamp(2, 3840) & !1).max(2);
        let rh = (h.clamp(2, 2160) & !1).max(2);
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if self.libmpv.is_some() {
            if let Some(pump) = self.soft_pump.as_ref() {
                pump.set_target(rw, rh);
                if let Some((fw, fh, pixels)) = pump.take_frame() {
                    self.soft_shot_tick = self.soft_shot_tick.wrapping_add(1);
                    if self.last_soft_rgba.is_none() || self.soft_shot_tick.is_multiple_of(180) {
                        self.last_soft_rgba = Some(Self::snapshot_scaled(fw, fh, &pixels));
                    }
                    return Some((fw, fh, pixels));
                }
                return None;
            }
            let pixels = {
                let mut mpv = self.lock_mpv()?;
                mpv.render_sw_rgba(rw, rh)?
            };
            self.soft_shot_tick = self.soft_shot_tick.wrapping_add(1);
            if self.last_soft_rgba.is_none() || self.soft_shot_tick.is_multiple_of(180) {
                self.last_soft_rgba = Some(Self::snapshot_scaled(rw, rh, &pixels));
            }
            return Some((rw, rh, pixels));
        }
        // Software fallback path — reachable when the FFmpeg backend is selected
        // at runtime (libmpv compiled but not the active backend).
        #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
        if let Some(ff) = self.libffmpeg.as_ref() {
            let (rw, rh) = crate::ffmpeg_ffi::LibFfmpeg::soft_present_dims(w, h);
            let pixels = ff.pull_rgba(rw, rh)?;
            self.soft_shot_tick = self.soft_shot_tick.wrapping_add(1);
            if self.last_soft_rgba.is_none() || self.soft_shot_tick.is_multiple_of(180) {
                self.last_soft_rgba = Some(Self::snapshot_scaled(rw, rh, &pixels));
            }
            return Some((rw, rh, pixels));
        }
        let _ = (rw, rh);
        None
    }

    /// Return a discarded soft RGBA buffer to the embed pool (mpv capacity recycle).
    pub fn recycle_soft_rgba(&mut self, buf: Vec<u8>) {
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        {
            if let Some(pump) = self.soft_pump.as_ref() {
                pump.offer_recycle(buf);
                return;
            }
            if let Some(mut mpv) = self.lock_mpv() {
                mpv.recycle_sw_rgba(buf);
                return;
            }
        }
        let _ = buf;
    }

    /// True when embedded backend has a newer frame than the last successful pull.
    pub fn frame_needs_redraw(&self) -> bool {
        if self.android_present.uses_surface() {
            return false;
        }
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        {
            if let Some(pump) = self.soft_pump.as_ref() {
                return pump.needs_redraw();
            }
            if let Some(mpv) = self.lock_mpv() {
                return mpv.frame_needs_redraw();
            }
        }
        #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
        if let Some(ff) = &self.libffmpeg {
            return ff.has_frame();
        }
        false
    }

    pub fn has_embedded_video(&self) -> bool {
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if self.libmpv.is_some() {
            return true;
        }
        #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
        if self.libffmpeg.is_some() {
            return true;
        }
        false
    }

    /// Android Surface / gpu-egl present (video not in iced RGBA).
    pub fn android_surface_present(&self) -> bool {
        self.android_present.uses_surface() && self.has_embedded_video()
    }

    pub fn android_present_mode(&self) -> crate::AndroidPresentMode {
        self.android_present
    }

    /// Rebind MediaCodec Surface after rotate / Surface recreate (gen bump).
    pub fn rebind_android_surface(&mut self, wid: i64, wh: Option<(u32, u32)>) -> bool {
        if !self.android_present.uses_surface() {
            return false;
        }
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        {
            let Some(mpv) = self.lock_mpv() else {
                return false;
            };
            // mpv-android: ensure VO is active when surface returns.
            if self.android_present == crate::AndroidPresentMode::SurfaceEmbed {
                let _ = mpv.set_property("vo", "mediacodec_embed");
            } else if self.android_present == crate::AndroidPresentMode::GpuEgl {
                let _ = mpv.set_property("vo", "gpu");
            }
            let _ = mpv.set_property("force-window", "yes");
            let ok = mpv.set_property_i64("wid", wid).is_ok();
            if !ok {
                warn!(wid, "android Surface wid rebind failed");
                return false;
            }
            if let Some((w, h)) = wh {
                let size = format!("{w}x{h}");
                let _ = mpv.set_property("android-surface-size", &size);
            }
            drop(mpv);
            if let Some((w, h)) = wh {
                self.opts.android_surface_wh = Some((w, h));
            }
            self.opts.android_surface_wid = Some(wid);
            info!(wid, ?wh, "android Surface wid rebound");
            true
        }
        #[cfg(not(all(feature = "native-mpv", fluxplay_has_libmpv)))]
        {
            let _ = (wid, wh);
            false
        }
    }

    /// Update Surface size property only — keep the same wid (no MediaCodec tear-down).
    pub fn rebind_android_surface_size(&mut self, wh: (u32, u32)) -> bool {
        if !self.android_present.uses_surface() {
            return false;
        }
        let (w, h) = wh;
        if w < 64 || h < 64 {
            return false;
        }
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        {
            let Some(mpv) = self.lock_mpv() else {
                return false;
            };
            let size = format!("{w}x{h}");
            let ok = mpv.set_property("android-surface-size", &size).is_ok();
            drop(mpv);
            if ok {
                self.opts.android_surface_wh = Some((w, h));
                debug!(?wh, "android Surface size updated (wid kept)");
            }
            ok
        }
        #[cfg(not(all(feature = "native-mpv", fluxplay_has_libmpv)))]
        {
            self.opts.android_surface_wh = Some((w, h));
            true
        }
    }

    /// mpv-android detach order: `vo=null` → `force-window=no` → `wid=0` before
    /// dropping the Surface GlobalRef (avoids UAF into a released jobject).
    pub fn detach_android_surface(&mut self) {
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        {
            if let Some(mpv) = self.lock_mpv() {
                let _ = mpv.set_property("vo", "null");
                let _ = mpv.set_property("force-window", "no");
                let _ = mpv.set_property_i64("wid", 0);
                info!("android Surface detached (vo=null, wid=0)");
            }
        }
        self.opts.android_surface_wid = None;
    }

    fn apply_video_geometry(&mut self) {
        let Some(rect) = self.video_rect.filter(|r| r.is_usable()) else {
            return;
        };
        // Only applies to CLI mpv fallback (separate window).
        if self.has_embedded_video() {
            return;
        }
        let geo = rect.to_geometry();
        let border = if rect.anchored { "no" } else { "yes" };
        let ontop = if rect.anchored { "yes" } else { "no" };
        if let Some(path) = &self.ipc_path {
            let _ = mpv_cmd(path, &["set_property", "geometry", &geo]);
            let _ = mpv_cmd(path, &["set_property", "border", border]);
            let _ = mpv_cmd(path, &["set_property", "ontop", ontop]);
        }
    }

    pub fn active_backend(&self) -> Option<BackendId> {
        self.backend
    }

    pub fn is_running(&mut self) -> bool {
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        {
            if let Some(mpv) = self.lock_mpv() {
                // Avoid idle-active alone (flaps → black stage). Combine EOF + empty path.
                if let Some(eof) = mpv.get_property_string("eof-reached") {
                    if eof == "yes" || eof == "true" {
                        return false;
                    }
                }
                let path = mpv.get_property_string("path").unwrap_or_default();
                if path.is_empty() {
                    if let Some(idle) = mpv.get_property_string("idle-active") {
                        if idle == "yes" || idle == "true" {
                            // Failed/stalled loadfile: idle with nothing loaded.
                            return false;
                        }
                    }
                }
                return true;
            }
        }
        #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
        {
            if let Some(ff) = &self.libffmpeg {
                return ff.is_alive();
            }
        }
        // CLI mpv: keep-open leaves the process alive after EOF — check IPC.
        if let Some(path) = &self.ipc_path {
            let child_alive = match &mut self.child {
                Some(c) => matches!(c.try_wait(), Ok(None)),
                None => false,
            };
            if !child_alive {
                return false;
            }
            if mpv_get_bool(path, "eof-reached") == Some(true) {
                return false;
            }
            if mpv_get_string(path, "path").as_deref().unwrap_or("").is_empty()
                && mpv_get_bool(path, "idle-active") == Some(true)
            {
                return false;
            }
            return true;
        }
        match &mut self.child {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }

    pub fn play(&mut self, url: &str) -> Result<BackendId> {
        let _prof = Stopwatch::start("native_play");
        let endpoint = url_endpoint(url);
        debug!(%endpoint, preferred = ?self.opts.preferred, "NativePlayer::play");
        // stop() detaches mpv wid — preserve the Surface bind prepared by the app
        // so start_libmpv still sees mediacodec_embed + wid (otherwise soft black overlay).
        #[cfg(target_os = "android")]
        let preserved_surface = (
            self.opts.android_present,
            self.opts.android_surface_wid,
            self.opts.android_surface_wh,
        );
        self.stop();
        #[cfg(target_os = "android")]
        {
            self.opts.android_present = preserved_surface.0;
            self.opts.android_surface_wid = preserved_surface.1;
            self.opts.android_surface_wh = preserved_surface.2;
        }
        // Panels with max_connections=1 need a beat to release the CDN slot.
        // Never sleep on Android UI/NativeActivity thread (ANR).
        #[cfg(not(target_os = "android"))]
        {
            std::thread::sleep(std::time::Duration::from_millis(650));
        }

        let backend = pick_backend(self.opts.preferred)?;
        let attempt = |this: &mut Self, backend: BackendId, url: &str| -> Result<()> {
            match backend {
                BackendId::Mpv => this.start_mpv(url),
                BackendId::Ffmpeg => this.start_ffmpeg(url),
                BackendId::External => {
                    // Android: app layer must use JNI ACTION_VIEW (android_intent).
                    // fluxplay-player must not depend on the iced app crate.
                    #[cfg(target_os = "android")]
                    {
                        let _ = url;
                        Err(PlayerError::Backend("EXTERNAL_NEEDS_INTENT".into()))
                    }
                    #[cfg(not(target_os = "android"))]
                    {
                        open::that(url).map_err(|e| PlayerError::Backend(e.to_string()))
                    }
                }
                BackendId::ExoPlayer | BackendId::AvPlayer => Err(PlayerError::Backend(
                    "Mobile decode is handled by the native shell (ExoPlayer/AVPlayer)".into(),
                )),
            }
        };

        if let Err(e) = attempt(self, backend, url) {
            error!(?backend, %endpoint, error = %e, "native play attempt failed");
            return Err(e);
        }

        // Reap immediate crash so UI can surface the error; one retry after another pause.
        // CLI child only (desktop); libmpv embed has no child to poll.
        #[cfg(not(target_os = "android"))]
        if let Some(child) = &mut self.child {
            let wait_ms = if looks_like_vod_container(url) { 900 } else { 450 };
            std::thread::sleep(std::time::Duration::from_millis(wait_ms));
            if let Ok(Some(status)) = child.try_wait() {
                self.child = None;
                self.backend = None;
                self.ipc_path = None;
                warn!(%status, %url, "player exited immediately — retry once");
                std::thread::sleep(std::time::Duration::from_millis(800));
                if let Err(e) = attempt(self, backend, url) {
                    error!(?backend, %endpoint, error = %e, "native play retry failed");
                    return Err(e);
                }
                if let Some(child) = &mut self.child {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    if let Ok(Some(status)) = child.try_wait() {
                        self.child = None;
                        self.backend = None;
                        error!(%status, %endpoint, "player exited after retry");
                        let hint = tail_player_log();
                        return Err(PlayerError::Backend(if hint.is_empty() {
                            format!(
                                "lecteur fermé aussitôt (code {status}). Stoppez les autres clients IPTV (1 connexion max) ou vérifiez le flux."
                            )
                        } else {
                            format!(
                                "lecteur fermé aussitôt (code {status}). {hint}"
                            )
                        }));
                    }
                }
            }
        }
        self.backend = Some(backend);
        info!(?backend, %url, "native playback started");
        Ok(backend)
    }

    pub fn stop(&mut self) {
        let had = self.backend;
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        {
            self.detach_android_surface();
            self.stop_soft_pump();
            if let Some(arc) = self.libmpv.take() {
                debug!("NativePlayer::stop libmpv shutdown");
                match std::sync::Arc::try_unwrap(arc) {
                    Ok(m) => {
                        let mpv = m.into_inner().unwrap_or_else(|e| e.into_inner());
                        mpv.shutdown();
                    }
                    Err(arc) => {
                        // Unexpected extra refs — drop Arc; LibMpv::Drop shuts down.
                        drop(arc);
                    }
                }
            }
        }
        #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
        {
            if let Some(ff) = self.libffmpeg.take() {
                debug!("NativePlayer::stop libffmpeg shutdown");
                ff.shutdown();
            }
        }
        if let Some(path) = self.ipc_path.take() {
            debug!(ipc = %path.display(), "NativePlayer::stop ipc quit");
            let _ = mpv_cmd(&path, &["quit"]);
            let _ = std::fs::remove_file(&path);
        }
        if let Some(mut child) = self.child.take() {
            debug!("NativePlayer::stop kill child");
            let _ = child.kill();
            let _ = child.wait();
        }
        if had.is_some() {
            info!(?had, "NativePlayer::stop");
        } else {
            trace!("NativePlayer::stop idle");
        }
        self.backend = None;
        self.android_present = crate::AndroidPresentMode::default();
        self.last_soft_rgba = None;
        self.soft_shot_tick = 0;
        self.ffplay_muted = false;
        self.ffplay_paused = false;
    }

    pub fn pause(&mut self, paused: bool) -> Result<()> {
        debug!(paused, backend = ?self.backend, "NativePlayer::pause");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.lock_mpv() {
            return mpv.set_property("pause", if paused { "yes" } else { "no" });
        }
        #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
        if let Some(ff) = &self.libffmpeg {
            ff.pause(paused);
            return Ok(());
        }
        if let Some(path) = &self.ipc_path {
            mpv_cmd(path, &["set_property", "pause", if paused { "yes" } else { "no" }])?;
            return Ok(());
        }
        if self.using_ffplay_cli() {
            // SDL `space` toggles — edge-trigger like mute.
            if self.ffplay_paused != paused {
                if !ffplay_send_key("space") {
                    return Err(PlayerError::Backend(
                        "ffplay: xdotool pause failed (window introuvable ?)".into(),
                    ));
                }
                self.ffplay_paused = paused;
            }
            return Ok(());
        }
        Err(PlayerError::Backend(
            "pause: aucun backend contrôlable (external / sans lecteur)".into(),
        ))
    }

    pub fn set_volume(&mut self, vol: f32) -> Result<()> {
        self.opts.volume = vol.clamp(0.0, 1.0);
        let v = (self.opts.volume * 100.0).clamp(0.0, 100.0);
        debug!(volume = self.opts.volume, "NativePlayer::set_volume");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.lock_mpv() {
            return mpv.set_property("volume", &format!("{v:.0}"));
        }
        #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
        if let Some(ff) = &self.libffmpeg {
            ff.set_volume(self.opts.volume);
            return Ok(());
        }
        if let Some(path) = &self.ipc_path {
            mpv_cmd(path, &["set_property", "volume", &format!("{v:.0}")])?;
            return Ok(());
        }
        if self.using_ffplay_cli() {
            // Volume is applied at spawn (`-volume`); live change needs restart.
            return Ok(());
        }
        Err(PlayerError::Backend(
            "set_volume: aucun backend contrôlable".into(),
        ))
    }

    pub fn set_mute(&mut self, muted: bool) -> Result<()> {
        debug!(muted, "NativePlayer::set_mute");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.lock_mpv() {
            return mpv.set_property("mute", if muted { "yes" } else { "no" });
        }
        #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
        if let Some(ff) = &self.libffmpeg {
            ff.set_volume(if muted { 0.0 } else { self.opts.volume });
            return Ok(());
        }
        if let Some(path) = &self.ipc_path {
            mpv_cmd(path, &["set_property", "mute", if muted { "yes" } else { "no" }])?;
            return Ok(());
        }
        if self.using_ffplay_cli() {
            if self.ffplay_muted != muted {
                if !ffplay_send_key("m") {
                    return Err(PlayerError::Backend(
                        "ffplay: xdotool mute failed (window introuvable ?)".into(),
                    ));
                }
                self.ffplay_muted = muted;
            }
            return Ok(());
        }
        Err(PlayerError::Backend(
            "set_mute: aucun backend contrôlable".into(),
        ))
    }

    /// Relative seek in seconds (negative = rewind). Best with mpv / VOD.
    pub fn seek_relative(&mut self, secs: f64) -> Result<()> {
        debug!(secs, "NativePlayer::seek_relative");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.lock_mpv() {
            return mpv.command(&["seek", &format!("{secs}"), "relative"]);
        }
        #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
        if let Some(ff) = &self.libffmpeg {
            let target = (ff.position_secs() + secs).max(0.0);
            ff.seek(target);
            return Ok(());
        }
        if let Some(path) = &self.ipc_path {
            return mpv_cmd(path, &["seek", &format!("{secs}"), "relative"]);
        }
        if self.using_libffmpeg() {
            return Ok(());
        }
        if self.using_ffplay_cli() {
            let key = if secs <= -25.0 {
                "Down"
            } else if secs < 0.0 {
                "Left"
            } else if secs >= 25.0 {
                "Up"
            } else {
                "Right"
            };
            if !ffplay_send_key(key) {
                return Err(PlayerError::Backend(
                    "ffplay: xdotool seek failed (window introuvable ?)".into(),
                ));
            }
        }
        Ok(())
    }

    pub fn seek_percent(&mut self, pct: f64) -> Result<()> {
        let pct = pct.clamp(0.0, 100.0);
        debug!(pct, "NativePlayer::seek_percent");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.lock_mpv() {
            return mpv.command(&["seek", &format!("{pct}"), "absolute-percent"]);
        }
        #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
        if let Some(ff) = &self.libffmpeg {
            let dur = ff.duration_secs();
            if dur > 0.0 {
                ff.seek(dur * pct / 100.0);
            }
            return Ok(());
        }
        if let Some(path) = &self.ipc_path {
            return mpv_cmd(
                path,
                &["seek", &format!("{pct}"), "absolute-percent"],
            );
        }
        Ok(())
    }

    pub fn toggle_fullscreen(&mut self) -> Result<()> {
        debug!("NativePlayer::toggle_fullscreen");
        // Soft embed: no OS video window — iced owns fullscreen.
        if self.has_embedded_video() {
            return Ok(());
        }
        if let Some(path) = &self.ipc_path {
            return mpv_cmd(path, &["cycle", "fullscreen"]);
        }
        if self.using_ffplay_cli() {
            if !ffplay_send_key("f") {
                return Err(PlayerError::Backend(
                    "ffplay: xdotool fullscreen failed".into(),
                ));
            }
            return Ok(());
        }
        Ok(())
    }

    pub fn cycle_audio(&mut self) -> Result<()> {
        debug!("NativePlayer::cycle_audio");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.lock_mpv() {
            return mpv.command(&["cycle", "audio"]);
        }
        if let Some(path) = &self.ipc_path {
            return mpv_cmd(path, &["cycle", "audio"]);
        }
        if self.using_libffmpeg() {
            return Ok(());
        }
        if self.using_ffplay_cli() {
            if !ffplay_send_key("a") {
                return Err(PlayerError::Backend(
                    "ffplay: xdotool audio-cycle failed".into(),
                ));
            }
            return Ok(());
        }
        Ok(())
    }

    pub fn cycle_subtitles(&mut self) -> Result<()> {
        debug!("NativePlayer::cycle_subtitles");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.lock_mpv() {
            return mpv.command(&["cycle", "sub"]);
        }
        if let Some(path) = &self.ipc_path {
            return mpv_cmd(path, &["cycle", "sub"]);
        }
        if self.using_libffmpeg() {
            return Ok(());
        }
        if self.using_ffplay_cli() {
            if !ffplay_send_key("t") {
                return Err(PlayerError::Backend(
                    "ffplay: xdotool subtitle-cycle failed".into(),
                ));
            }
            return Ok(());
        }
        Ok(())
    }

    /// Re-load the same URL (reconnect after stall / token refresh).
    pub fn restart(&mut self, url: &str) -> Result<()> {
        let _prof = Stopwatch::start("native_restart");
        let endpoint = url_endpoint(url);
        info!(%endpoint, "NativePlayer::restart");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.lock_mpv() {
            mpv.command(&["loadfile", url, "replace"])?;
            return Ok(());
        }
        if let Some(path) = &self.ipc_path {
            mpv_cmd(path, &["loadfile", url, "replace"])?;
            return Ok(());
        }
        self.play(url)?;
        Ok(())
    }

    pub fn frame_step(&mut self) -> Result<()> {
        trace!("NativePlayer::frame_step");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.lock_mpv() {
            return mpv.command(&["frame-step"]);
        }
        if let Some(path) = &self.ipc_path {
            return mpv_cmd(path, &["frame-step"]);
        }
        if self.using_libffmpeg() {
            return Ok(());
        }
        if self.using_ffplay_cli() {
            if !ffplay_send_key("s") {
                return Err(PlayerError::Backend(
                    "ffplay: xdotool frame-step failed".into(),
                ));
            }
            return Ok(());
        }
        Ok(())
    }

    /// Query playback position / duration (seconds).
    pub fn playback_times(&self) -> Option<(f64, f64)> {
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.try_lock_mpv() {
            let pos = mpv.get_property_double("time-pos")?;
            let dur = mpv.get_property_double("duration").unwrap_or(0.0);
            return Some((pos, dur));
        }
        #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
        if let Some(ff) = &self.libffmpeg {
            let pos = ff.position_secs();
            let dur = ff.duration_secs();
            trace!(pos, dur, "playback_times libffmpeg");
            return Some((pos, dur));
        }
        let path = self.ipc_path.as_ref()?;
        let pos = mpv_get_number(path, "time-pos")?;
        let dur = mpv_get_number(path, "duration").unwrap_or(0.0);
        trace!(pos, dur, "playback_times ipc");
        Some((pos, dur))
    }

    /// Estimated content frame rate (VOD/HLS). Used to avoid over-presenting soft frames.
    pub fn content_fps(&self) -> Option<f64> {
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.try_lock_mpv() {
            let fps = mpv
                .get_property_double("container-fps")
                .filter(|f| *f >= 20.0 && f.is_finite())
                .or_else(|| {
                    mpv.get_property_double("estimated-vf-fps")
                        .filter(|f| *f >= 20.0 && f.is_finite())
                });
            return fps;
        }
        #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
        if self.libffmpeg.is_some() {
            // Soft FFmpeg path presents decoded frames as available — no container clock here.
            return None;
        }
        let path = self.ipc_path.as_ref()?;
        mpv_get_number(path, "container-fps")
            .filter(|f| *f >= 20.0 && f.is_finite())
            .or_else(|| {
                mpv_get_number(path, "estimated-vf-fps").filter(|f| *f >= 20.0 && f.is_finite())
            })
    }

    /// Post-content-fps A/V offset for the Android Surface (zero-copy) path.
    ///
    /// A fixed pre-init `video-timing-offset` vs the mpv default left audio ~50 ms
    /// ahead on 4K `mediacodec_embed`, which reads as judder on 24/25 fps streams.
    /// Scale the offset by content rate once the demuxer knows it.
    pub fn sync_android_video_timing(&self) {
        if self.android_present.uses_soft_rgba() {
            return;
        }
        let Some(fps) = self.content_fps() else {
            return;
        };
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.lock_mpv() {
            // ≈1 frame at content rate, clamped — covers MediaCodec queue depth without
            // pushing VO behind audio on low-fps film.
            let offset = (1.0 / fps).clamp(0.005, 0.042);
            let v = format!("{offset:.4}");
            if mpv.set_option("video-timing-offset", &v).is_err() {
                soft_set(&mpv, "video-timing-offset", &v);
            }
        }
    }

    /// Display video size (SAR/DAR-corrected), e.g. (2386, 1080) for anamorphic scope.
    /// `dw`/`dh` is what mpv would render — the SurfaceView is fit to this ratio.
    pub fn video_wh(&self) -> Option<(u32, u32)> {
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.try_lock_mpv() {
            let (w, h) = mpv
                .get_property_double("video-out-params/dw")
                .zip(mpv.get_property_double("video-out-params/dh"))
                .or_else(|| {
                    mpv.get_property_double("video-params/dw")
                        .zip(mpv.get_property_double("video-params/dh"))
                })?;
            if w >= 16.0 && h >= 16.0 {
                return Some((w as u32, h as u32));
            }
        }
        let _ = &self;
        None
    }

    /// Decoded buffer size (no SAR/DAR correction) — matches the actual
    /// MediaCodec/decoder frame dims fed to the Surface buffer.
    pub fn video_buffer_wh(&self) -> Option<(u32, u32)> {
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.try_lock_mpv() {
            let (w, h) = mpv
                .get_property_double("video-out-params/w")
                .zip(mpv.get_property_double("video-out-params/h"))
                .or_else(|| {
                    mpv.get_property_double("video-params/w")
                        .zip(mpv.get_property_double("video-params/h"))
                })?;
            if w >= 16.0 && h >= 16.0 {
                return Some((w as u32, h as u32));
            }
        }
        let _ = &self;
        None
    }

    fn set_prop(&self, name: &str, value: &str) -> Result<()> {
        debug!(%name, %value, "NativePlayer::set_prop");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.lock_mpv() {
            return match mpv.set_property(name, value) {
                Ok(()) => {
                    info!(%name, %value, "set_prop ok (libmpv)");
                    Ok(())
                }
                Err(e) => {
                    warn!(%name, %value, error = %e, "set_prop FAILED (libmpv)");
                    Err(e)
                }
            };
        }
        if let Some(path) = &self.ipc_path {
            return match mpv_cmd(path, &["set_property", name, value]) {
                Ok(()) => {
                    info!(%name, %value, "set_prop ok (ipc)");
                    Ok(())
                }
                Err(e) => {
                    warn!(%name, %value, error = %e, "set_prop FAILED (ipc)");
                    Err(e)
                }
            };
        }
        warn!(%name, %value, "set_prop skipped — no active mpv");
        Err(PlayerError::Backend(format!(
            "set_prop {name}: pas de libmpv/IPC (backend non supporté)"
        )))
    }

    fn run_cmd(&self, args: &[&str]) -> Result<()> {
        let cmd = args.first().copied().unwrap_or("");
        debug!(%cmd, args = ?args, "NativePlayer::run_cmd");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.lock_mpv() {
            return match mpv.command(args) {
                Ok(()) => {
                    info!(%cmd, "run_cmd ok (libmpv)");
                    Ok(())
                }
                Err(e) => {
                    warn!(%cmd, error = %e, "run_cmd FAILED (libmpv)");
                    Err(e)
                }
            };
        }
        if let Some(path) = &self.ipc_path {
            return match mpv_cmd(path, args) {
                Ok(()) => {
                    info!(%cmd, "run_cmd ok (ipc)");
                    Ok(())
                }
                Err(e) => {
                    warn!(%cmd, error = %e, "run_cmd FAILED (ipc)");
                    Err(e)
                }
            };
        }
        warn!(%cmd, "run_cmd skipped — no active mpv");
        Err(PlayerError::Backend(format!(
            "run_cmd {cmd}: pas de libmpv/IPC (backend non supporté)"
        )))
    }

    pub fn set_speed(&mut self, speed: f64) -> Result<()> {
        let speed = speed.clamp(0.25, 3.0);
        debug!(speed, "NativePlayer::set_speed");
        self.set_prop("speed", &format!("{speed:.2}"))
    }

    pub fn set_loop_file(&mut self, on: bool) -> Result<()> {
        debug!(on, "NativePlayer::set_loop_file");
        self.set_prop("loop-file", if on { "inf" } else { "no" })
    }

    pub fn set_ab_loop(&mut self, a: Option<f64>, b: Option<f64>) -> Result<()> {
        debug!(?a, ?b, "NativePlayer::set_ab_loop");
        match a {
            Some(t) => self.set_prop("ab-loop-a", &format!("{t:.3}"))?,
            None => self.set_prop("ab-loop-a", "no")?,
        }
        match b {
            Some(t) => self.set_prop("ab-loop-b", &format!("{t:.3}"))?,
            None => self.set_prop("ab-loop-b", "no")?,
        }
        Ok(())
    }

    pub fn screenshot_to(&mut self, path: &str) -> Result<()> {
        info!(%path, "NativePlayer::screenshot");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if self.libmpv.is_some() {
            return self.run_cmd(&["screenshot-to-file", path, "video"]);
        }
        if self.ipc_path.is_some() {
            return self.run_cmd(&["screenshot-to-file", path, "video"]);
        }
        // Soft embed: app writes PNG from last_soft_rgba when this error is seen.
        if self.last_soft_rgba.is_some() {
            return Err(PlayerError::Backend("SOFT_RGBA".into()));
        }
        Err(PlayerError::Backend(
            "screenshot: pas de libmpv/IPC ni frame soft".into(),
        ))
    }

    pub fn seek_absolute(&mut self, secs: f64) -> Result<()> {
        let secs = secs.max(0.0);
        debug!(secs, "NativePlayer::seek_absolute");
        #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
        if let Some(ff) = &self.libffmpeg {
            ff.seek(secs);
            return Ok(());
        }
        self.run_cmd(&["seek", &format!("{secs}"), "absolute"])
    }

    pub fn chapter_step(&mut self, delta: i32) -> Result<()> {
        debug!(delta, "NativePlayer::chapter_step");
        self.run_cmd(&["add", "chapter", &format!("{delta}")])
    }

    pub fn set_sub_delay(&mut self, secs: f64) -> Result<()> {
        debug!(secs, "NativePlayer::set_sub_delay");
        self.set_prop("sub-delay", &format!("{secs:.2}"))
    }

    pub fn set_audio_delay(&mut self, secs: f64) -> Result<()> {
        debug!(secs, "NativePlayer::set_audio_delay");
        self.set_prop("audio-delay", &format!("{secs:.2}"))
    }

    pub fn set_audio_channels(&mut self, mode: &str) -> Result<()> {
        // mono | auto | 1 | 2 | …
        debug!(mode, "NativePlayer::set_audio_channels");
        self.set_prop("audio-channels", mode)
    }

    pub fn set_af(&mut self, filter: &str) -> Result<()> {
        debug!(filter, "NativePlayer::set_af");
        self.set_prop("af", filter)
    }

    pub fn set_deinterlace(&mut self, on: bool) -> Result<()> {
        debug!(on, "NativePlayer::set_deinterlace");
        self.set_prop("deinterlace", if on { "yes" } else { "no" })
    }

    pub fn set_deinterlace_mode(&mut self, mode: &str) -> Result<()> {
        debug!(mode, "NativePlayer::set_deinterlace_mode");
        self.set_prop("deinterlace", mode)
    }

    pub fn set_scale(&mut self, scale: &str) -> Result<()> {
        debug!(scale, "NativePlayer::set_scale");
        self.set_prop("scale", scale)?;
        self.set_prop("cscale", scale)?;
        self.set_prop("dscale", scale)
    }

    pub fn set_video_rotate(&mut self, deg: u32) -> Result<()> {
        let deg = match deg {
            90 | 180 | 270 => deg,
            _ => 0,
        };
        debug!(deg, "NativePlayer::set_video_rotate");
        self.set_prop("video-rotate", &format!("{deg}"))
    }

    pub fn set_video_zoom(&mut self, zoom: f64) -> Result<()> {
        let zoom = zoom.clamp(-2.0, 2.0);
        debug!(zoom, "NativePlayer::set_video_zoom");
        self.set_prop("video-zoom", &format!("{zoom:.2}"))
    }

    pub fn set_aspect(&mut self, aspect: &str) -> Result<()> {
        // "-1" = auto, "16:9", "4:3", "2.35", …
        debug!(aspect, "NativePlayer::set_aspect");
        self.set_prop("video-aspect-override", aspect)
    }

    pub fn set_ontop(&mut self, on: bool) -> Result<()> {
        debug!(on, "NativePlayer::set_ontop");
        self.set_prop("ontop", if on { "yes" } else { "no" })
    }

    /// Bring the mpv video window forward (Wayland/X11 managed window).
    pub fn raise_video_window(&mut self) -> Result<()> {
        debug!("NativePlayer::raise_video_window");
        let _ = self.set_prop("ontop", "yes");
        self.apply_video_geometry();
        Ok(())
    }

    pub fn set_vf(&mut self, filter: &str) -> Result<()> {
        debug!(filter, "NativePlayer::set_vf");
        self.set_prop("vf", filter)
    }

    /// Color wheel safe on MediaCodec Surface (no vf / no black frames).
    pub fn set_color_adjust(
        &mut self,
        brightness: i32,
        contrast: i32,
        saturation: i32,
        gamma: i32,
    ) -> Result<()> {
        debug!(brightness, contrast, saturation, gamma, "NativePlayer::set_color_adjust");
        self.set_prop("brightness", &brightness.to_string())?;
        self.set_prop("contrast", &contrast.to_string())?;
        self.set_prop("saturation", &saturation.to_string())?;
        self.set_prop("gamma", &gamma.to_string())
    }

    /// Force Soft RGBA present flag (after Surface demotion). Does not recreate mpv.
    pub fn force_android_soft_present(&mut self) {
        self.detach_android_surface();
        self.android_present = crate::AndroidPresentMode::SoftRgba;
        self.opts.android_present = crate::AndroidPresentMode::SoftRgba;
        self.opts.android_surface_wid = None;
        self.opts.android_surface_wh = None;
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        self.ensure_soft_pump();
    }

    pub fn toggle_sub_visibility(&mut self) -> Result<()> {
        debug!("NativePlayer::toggle_sub_visibility");
        self.run_cmd(&["cycle", "sub-visibility"])
    }

    fn start_mpv(&mut self, url: &str) -> Result<()> {
        let endpoint = url_endpoint(url);
        debug!(%endpoint, "start_mpv");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        {
            match self.start_libmpv(url) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    #[cfg(target_os = "android")]
                    {
                        warn!(error = %e, "libmpv start failed (Android — no CLI fallback)");
                        return Err(e);
                    }
                    #[cfg(not(target_os = "android"))]
                    {
                        warn!(error = %e, "libmpv start failed — trying CLI fallback");
                        #[cfg(not(feature = "cli-player"))]
                        {
                            return Err(e);
                        }
                    }
                }
            }
        }
        #[cfg(all(
            target_os = "android",
            not(all(feature = "native-mpv", fluxplay_has_libmpv))
        ))]
        {
            Err(PlayerError::Backend(
                "libmpv indisponible — vérifiez vendor/android-native libmpv.so".into(),
            ))
        }
        #[cfg(all(not(target_os = "android"), feature = "cli-player"))]
        {
            self.start_mpv_cli(url)
        }
        #[cfg(all(not(target_os = "android"), not(feature = "cli-player")))]
        {
            Err(PlayerError::Backend(
                "Aucun backend mpv (activez native-mpv ou cli-player)".into(),
            ))
        }
    }

    #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
    fn start_libmpv(&mut self, url: &str) -> Result<()> {
        let _prof = Stopwatch::start("start_libmpv");
        let endpoint = url_endpoint(url);
        let mpeg_ts = looks_like_mpeg_ts(url);
        let vod = looks_like_vod_container(url);
        let demux_cache = if vod {
            self.opts.demux_secs.max(20.0)
        } else if mpeg_ts {
            self.opts.demux_secs.max(8.0)
        } else {
            self.opts.demux_secs.max(4.0)
        };
        // Prefer the larger of demux readahead and settings cache_ms.
        let cache_secs = demux_cache.max(self.opts.cache_ms as f32 / 1000.0);
        debug!(%endpoint, mpeg_ts, vod, cache_secs, cache_ms = self.opts.cache_ms, "start_libmpv embedded");

        let mut mpv = crate::mpv_ffi::LibMpv::create()?;
        // Embedded path: vo=libmpv + software render → frames drawn into iced (no OS video window).
        // media-kit Android builds omit some desktop options — soft_set those, hard-require vo.
        soft_set(&mpv, "config", "no");
        if let Some(level) = crate::native_log::mpv_msg_level() {
            let log_path = crate::native_log::mpv_verbose_log_path();
            soft_set(&mpv, "terminal", "yes");
            // Hard-set: soft_set can silently drop msg-level on media-kit.
            if mpv.set_option("msg-level", level).is_err() {
                soft_set(&mpv, "msg-level", level);
            }
            soft_set(&mpv, "log-file", &log_path.to_string_lossy());
            info!(
                msg_level = level,
                log = %log_path.display(),
                "libmpv verbose logging enabled"
            );
        } else {
            soft_set(&mpv, "terminal", "no");
        }
        soft_set(&mpv, "idle", "yes");
        #[cfg(target_os = "android")]
        let present = {
            let mut mode = self.opts.android_present;
            // Surface path needs a live wid; otherwise fall back soft (no black video).
            if mode.uses_surface() && self.opts.android_surface_wid.is_none() {
                warn!(
                    mode = mode.label(),
                    "android surface wid missing — soft RGBA fallback"
                );
                mode = crate::AndroidPresentMode::SoftRgba;
            }
            mode
        };
        #[cfg(not(target_os = "android"))]
        let present = crate::AndroidPresentMode::SoftRgba;
        self.android_present = present;

        #[cfg(target_os = "android")]
        {
            match present {
                crate::AndroidPresentMode::SurfaceEmbed => {
                    // Zero-copy MediaCodec → SurfaceView (HDR/4K capable).
                    let wid = self.opts.android_surface_wid.unwrap();
                    mpv.set_option_i64("wid", wid).map_err(|e| {
                        PlayerError::Backend(format!("{e} — wid Surface requis pour mediacodec_embed"))
                    })?;
                    mpv.set_option("vo", "mediacodec_embed").map_err(|e| {
                        PlayerError::Backend(format!("{e} — vo=mediacodec_embed indisponible"))
                    })?;
                    if let Some((w, h)) = self.opts.android_surface_wh {
                        if mpv
                            .set_option("android-surface-size", &format!("{w}x{h}"))
                            .is_err()
                        {
                            soft_set(&mpv, "android-surface-size", &format!("{w}x{h}"));
                        }
                    }
                    // Hard-set: soft_set can silently skip → SW decode / black video.
                    if mpv.set_option("hwdec", "mediacodec").is_err() {
                        soft_set(&mpv, "hwdec", "mediacodec");
                        warn!("android Surface hwdec=mediacodec soft-set fallback");
                    }
                    soft_set(&mpv, "hwdec-codecs", "all");
                    // mpv-android: force-window yes while Surface is attached.
                    soft_set(&mpv, "force-window", "yes");
                    // No vf scale — keep full bitstream quality on Surface.
                    // Pre-init neutral; play_session re-tunes per content fps (sync_android_video_timing).
                    if mpv.set_option("video-timing-offset", "0.020").is_err() {
                        soft_set(&mpv, "video-timing-offset", "0.020");
                    }
                    info!(%endpoint, wid, "android present=mediacodec_embed");
                }
                crate::AndroidPresentMode::GpuEgl => {
                    // Phase D without Vulkan rebuild: vo=gpu + egl-android on Surface wid.
                    let wid = self.opts.android_surface_wid.unwrap();
                    mpv.set_option_i64("wid", wid).map_err(|e| {
                        PlayerError::Backend(format!("{e} — wid Surface requis pour vo=gpu"))
                    })?;
                    if mpv.set_option("vo", "gpu").is_err() {
                        warn!("vo=gpu failed — falling back to mediacodec_embed");
                        mpv.set_option("vo", "mediacodec_embed").map_err(|e| {
                            PlayerError::Backend(format!("{e} — vo gpu/embed indisponible"))
                        })?;
                        soft_set(&mpv, "hwdec", "mediacodec");
                    } else {
                        soft_set(&mpv, "gpu-context", "android");
                        soft_set(&mpv, "hwdec", "mediacodec-copy");
                    }
                    if let Some((w, h)) = self.opts.android_surface_wh {
                        soft_set(&mpv, "android-surface-size", &format!("{w}x{h}"));
                    }
                    soft_set(&mpv, "force-window", "yes");
                    info!(%endpoint, wid, "android present=gpu-egl");
                }
                crate::AndroidPresentMode::SoftRgba => {
                    mpv.set_option("vo", "libmpv").map_err(|e| {
                        PlayerError::Backend(format!(
                            "{e} — libmpv Android sans vo=libmpv (media-kit incomplet?)"
                        ))
                    })?;
                    soft_set(&mpv, "force-window", "no");
                    // SW render into portrait stage must letterbox — never stretch.
                    soft_set(&mpv, "keepaspect", "yes");
                    soft_set(&mpv, "panscan", "0");
                    info!(%endpoint, "android present=soft-rgba");
                }
            }
        }
        #[cfg(not(target_os = "android"))]
        {
            // vo=libmpv is required for mpv_render SW → iced RGBA. Fail clearly if missing.
            mpv.set_option("vo", "libmpv").map_err(|e| {
                PlayerError::Backend(format!("{e} — vo=libmpv requis pour soft present"))
            })?;
            soft_set(&mpv, "force-window", "no");
            if let Some((w, h)) = self.opts.video_max_wh {
                let w = w.max(2) & !1;
                let h = h.max(2) & !1;
                let vf = if w >= h {
                    format!("scale={w}:-2:flags=fast_bilinear,format=yuv420p")
                } else {
                    format!("scale=-2:{h}:flags=fast_bilinear,format=yuv420p")
                };
                if mpv.set_option("vf", &vf).is_err() {
                    soft_set(&mpv, "vf", &vf);
                }
            }
        }

        // HDR/gamut: iced RGBA is SDR — tonemap on soft. Surface keeps bitstream + window color mode.
        if present.uses_soft_rgba() {
            let hints = self.opts.hdr_mode.mpv_color_hints(self.opts.tonemap_hdr);
            soft_set(
                &mpv,
                "target-colorspace-hint",
                if hints.target_colorspace_hint { "yes" } else { "no" },
            );
            if let Some(tm) = hints.tone_mapping {
                soft_set(&mpv, "tone-mapping", tm);
            }
            if let Some(p) = hints.target_prim {
                soft_set(&mpv, "target-prim", p);
            }
            if let Some(t) = hints.target_trc {
                soft_set(&mpv, "target-trc", t);
            }
            if hints.hdr_compute_peak {
                soft_set(&mpv, "hdr-compute-peak", "yes");
            }
        }

        soft_set(&mpv, "keep-open", "yes");
        // media-kit Android omits ytdl/osc — skip to avoid soft_set DEBUG noise.
        #[cfg(not(target_os = "android"))]
        {
            soft_set(&mpv, "ytdl", "no");
            soft_set(&mpv, "osc", "no");
        }
        soft_set(&mpv, "title", "FluxPlay");
        soft_set(&mpv, "osd-level", "0");
        soft_set(&mpv, "input-default-bindings", "no");
        soft_set(&mpv, "input-vo-keyboard", "no");
        // Soft present uploads async via iced — a zero offset races audio ahead of GPU.
        // Hard-set: soft_set can silently skip on media-kit → A/V desync.
        if present.uses_soft_rgba() {
            // ~2 frames @48Hz soft — covers iced GPU allocate without starving VO.
            if mpv.set_option("video-timing-offset", "0.040").is_err() {
                soft_set(&mpv, "video-timing-offset", "0.040");
            }
        }
        // Android NativeActivity audio + soft-only filters.
        #[cfg(target_os = "android")]
        {
            // Vendored media-kit has OpenSLES (not AAudio) — avoid option warnings.
            if mpv.set_option("ao", "opensles").is_err() {
                soft_set(&mpv, "ao", "opensles");
                if mpv.set_option("ao", "auto").is_err() {
                    soft_set(&mpv, "ao", "auto");
                }
            }
            soft_set(&mpv, "audio-device", "auto");
            soft_set(&mpv, "audio-buffer", "0.2");
            if present.uses_soft_rgba() {
                let vf_owned = self
                    .opts
                    .android_soft_vf
                    .clone()
                    .unwrap_or_else(android_soft_vf_fallback);
                let vf = android_soft_vf_from_budget(&vf_owned);
                // media-kit often rejects bare `vf=scale=…` — try lavfi wrapper then property.
                let vf_ok = mpv.set_option("vf", &vf).is_ok()
                    || mpv.set_option("vf", &format!("lavfi=[{vf}]")).is_ok()
                    || {
                        soft_set(&mpv, "vf", &vf);
                        soft_set(&mpv, "vf", &format!("lavfi=[{vf}]"));
                        false
                    };
                if !vf_ok {
                    // Last resort: force SW decode budget so SoftPump isn't fed 4K copies.
                    soft_set(&mpv, "hwdec", "no");
                    warn!(%vf, "android soft vf rejected — hwdec=no fallback");
                }
                // HQ soft (≈720p+ budget): never skip loop filters / non-ref frames —
                // those options were the main "saccade / soft quality loss" source on Tensor.
                let hq_soft = self
                    .opts
                    .android_soft_vf
                    .as_deref()
                    .map(|s| {
                        s.contains("1920")
                            || s.contains("1280")
                            || s.contains("1080")
                            || s.contains("720")
                    })
                    .unwrap_or(true);
                if hq_soft {
                    soft_set(&mpv, "vd-lavc-skiploopfilter", "none");
                    soft_set(&mpv, "vd-lavc-skipframe", "none");
                    soft_set(&mpv, "vd-lavc-fast", "no");
                    // Keep VO framedrop only — decoder+vo can starve soft present on media-kit.
                    if mpv.set_option("framedrop", "vo").is_err() {
                        soft_set(&mpv, "framedrop", "vo");
                    }
                    soft_set(&mpv, "hwdec-extra-frames", "8");
                } else {
                    soft_set(&mpv, "vd-lavc-skiploopfilter", "nonkey");
                    soft_set(&mpv, "vd-lavc-skipframe", "nonref");
                    soft_set(&mpv, "vd-lavc-fast", "yes");
                    if mpv.set_option("framedrop", "vo").is_err() {
                        soft_set(&mpv, "framedrop", "vo");
                    }
                }
            } else {
                // Surface path: drop VO only if compositor stalls; keep decode.
                soft_set(&mpv, "framedrop", "no");
            }
            // Cap demux RAM on Android — 192+64 MiB was a large share of Unknown PSS.
            soft_set(&mpv, "demuxer-max-bytes", "64MiB");
            soft_set(&mpv, "demuxer-max-back-bytes", "16MiB");
        }
        soft_set(
            &mpv,
            "volume",
            &format!("{}", (self.opts.volume * 100.0) as u32),
        );
        #[cfg(target_os = "android")]
        let cache_secs = if vod {
            cache_secs.min(8.0)
        } else if mpeg_ts {
            cache_secs.min(3.0)
        } else {
            cache_secs.min(2.0)
        };
        soft_set(&mpv, "cache-secs", &format!("{cache_secs}"));
        soft_set(
            &mpv,
            "demuxer-readahead-secs",
            &format!(
                "{}",
                if vod {
                    #[cfg(target_os = "android")]
                    {
                        6.0
                    }
                    #[cfg(not(target_os = "android"))]
                    {
                        12.0
                    }
                } else if mpeg_ts {
                    3.0
                } else {
                    2.0
                }
            ),
        );
        soft_set(&mpv, "cache", "yes");
        soft_set(
            &mpv,
            "stream-lavf-o",
            "reconnect_streamed=1,reconnect_delay_max=5,reconnect_on_network_error=1",
        );
        soft_set(&mpv, "demuxer-lavf-o", "reconnect_streamed=1");

        if mpeg_ts || vod {
            soft_set(&mpv, "demuxer-lavf-probesize", "10000000");
            soft_set(&mpv, "demuxer-lavf-analyzeduration", "5");
            soft_set(&mpv, "cache-pause-initial", "yes");
        }

        if self.opts.hwdec {
            #[cfg(target_os = "android")]
            let skip_hwdec = present.uses_surface();
            #[cfg(not(target_os = "android"))]
            let skip_hwdec = false;
            if !skip_hwdec {
                let preferred = if self.opts.hwdec_force_copy {
                    // Cross-GPU: never request zero-copy interop on the display GPU.
                    match preferred_hwdec() {
                        "vaapi" => "vaapi-copy",
                        "cuda" | "nvdec" => "cuda-copy",
                        "vulkan" => "vulkan-copy",
                        other => other,
                    }
                } else {
                    preferred_hwdec()
                };
                // Hard-set: soft_set hid silent mediacodec-copy failure → black video + audio.
                if mpv.set_option("hwdec", preferred).is_err() {
                    warn!(%preferred, "hwdec preferred failed — falling back to software");
                    let _ = mpv.set_option("hwdec", "no");
                } else if preferred != "no" {
                    soft_set(&mpv, "hwdec-codecs", "all");
                }
                if let Some(dev) = &self.opts.vaapi_device {
                    soft_set(&mpv, "vaapi-device", dev);
                    info!(%dev, "vaapi-device (hybrid decode GPU)");
                }
            }
        } else if !present.uses_surface() {
            let _ = mpv.set_option("hwdec", "no");
        }
        // Fast filters for CPU soft-render path (stage already matches window size).
        soft_set(&mpv, "scale", "bilinear");
        soft_set(&mpv, "cscale", "bilinear");
        soft_set(&mpv, "dscale", "bilinear");
        soft_set(&mpv, "correct-downscaling", "no");
        soft_set(&mpv, "sigmoid-upscaling", "no");
        soft_set(&mpv, "interpolation", "no");
        if mpv.set_option("video-sync", "audio").is_err() {
            soft_set(&mpv, "video-sync", "audio");
        }
        #[cfg(not(target_os = "android"))]
        if mpv.set_option("framedrop", "vo").is_err() {
            soft_set(&mpv, "framedrop", "vo");
        }
        soft_set(&mpv, "vd-lavc-threads", "0");

        if self.opts.low_latency && !mpeg_ts && !vod {
            soft_set(&mpv, "profile", "low-latency");
            soft_set(&mpv, "cache", "no");
            // Do not set untimed=yes with soft/embedded VO — iced upload is async A/V.
        }

        if let Some(ua) = &self.opts.user_agent {
            soft_set(&mpv, "user-agent", &sanitize_http_field(ua));
        } else {
            soft_set(&mpv, "user-agent", "IPTVSmartersPlayer");
        }
        if let Some(proxy) = &self.opts.http_proxy {
            // Tunnel / SOCKS must apply — soft_set can silently skip on some builds.
            let safe = sanitize_http_field(proxy);
            mpv.set_option("http-proxy", &safe).map_err(|e| {
                PlayerError::Backend(format!("http-proxy: {e} — tunnel would leak to clearnet"))
            })?;
            let _ = soft_set(&mpv, "ytdl-raw-options", &format!("proxy={safe}"));
            // protocol_whitelist rejected on media-kit Android (code -7).
            #[cfg(not(target_os = "android"))]
            let _ = soft_set(
                &mpv,
                "stream-lavf-o",
                "protocol_whitelist=http,https,tcp,tls,rtmp,rtmps,rtsp,rtsps,rtp,udp,srt",
            );
        } else {
            #[cfg(not(target_os = "android"))]
            let _ = soft_set(
                &mpv,
                "stream-lavf-o",
                "protocol_whitelist=http,https,tcp,tls,rtmp,rtmps,rtsp,rtsps,rtp,udp,srt",
            );
        }
        if let Some(ref_r) = &self.opts.referer {
            let safe = sanitize_http_field(ref_r);
            if !safe.is_empty() {
                soft_set(&mpv, "referrer", &safe);
            }
        }
        if !self.opts.extra_headers.is_empty() {
            let joined = self
                .opts
                .extra_headers
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{}: {}",
                        sanitize_http_field(k),
                        sanitize_http_field(v)
                    )
                })
                .filter(|line| line.len() > 2)
                .collect::<Vec<_>>()
                .join("\r\n");
            if !joined.is_empty() {
                soft_set(&mpv, "http-header-fields", &joined);
            }
        }

        mpv.initialize()?;
        if present.uses_soft_rgba() {
            mpv.init_sw_render()?;
        }
        mpv.command(&["loadfile", url, "replace"])?;
        // Ensure AO audible after loadfile reconfig (OpenSL often start→stop→start).
        let _ = mpv.set_property("mute", "no");
        let _ = mpv.set_property(
            "volume",
            &format!("{}", (self.opts.volume * 100.0).clamp(0.0, 100.0) as u32),
        );
        // Desktop only: brief settle so hwdec-current is meaningful. Never sleep on
        // Android UI thread (ANR / frozen NativeActivity).
        #[cfg(not(target_os = "android"))]
        {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let hw = mpv
                .get_property_string("hwdec-current")
                .unwrap_or_else(|| "none".into());
            // After settle: if preferred never engaged, force software (avoid black soft VO).
            if self.opts.hwdec
                && (hw.is_empty()
                    || hw.eq_ignore_ascii_case("no")
                    || hw.eq_ignore_ascii_case("none"))
            {
                let _ = mpv.set_property("hwdec", "no");
                warn!(%endpoint, "hwdec-current idle — forced software decode");
            }
        }
        #[cfg(target_os = "android")]
        {
            // Do NOT force hwdec=no here — MediaCodec often attaches after the first
            // packets. Premature SW fallback was decoding 4K on CPU (~10 fps).
            let hw = mpv
                .get_property_string("hwdec-current")
                .unwrap_or_else(|| "none".into());
            info!(%endpoint, %hw, "android hwdec after loadfile (may still be attaching)");
        }
        let hw = mpv
            .get_property_string("hwdec-current")
            .unwrap_or_else(|| "none".into());
        let vo = mpv
            .get_property_string("current-vo")
            .unwrap_or_else(|| "?".into());
        let ao = mpv
            .get_property_string("current-ao")
            .unwrap_or_else(|| "?".into());
        let vol = mpv
            .get_property_string("volume")
            .unwrap_or_else(|| "?".into());
        let arc = std::sync::Arc::new(std::sync::Mutex::new(mpv));
        self.libmpv = Some(std::sync::Arc::clone(&arc));
        if present.uses_soft_rgba() {
            self.soft_pump = Some(crate::soft_pump::SoftPump::start(arc));
            info!("soft-pump started after loadfile");
        } else {
            self.stop_soft_pump();
        }
        info!(%endpoint, %hw, %vo, %ao, %vol, "libmpv embedded loadfile ok");
        Ok(())
    }

    #[cfg(feature = "cli-player")]
    #[cfg(not(target_os = "android"))] // CLI fallback — desktop only
    fn start_mpv_cli(&mut self, url: &str) -> Result<()> {
        let _prof = Stopwatch::start("start_mpv_cli");
        let endpoint = url_endpoint(url);
        let mpv_bin = which("mpv").ok_or_else(|| {
            PlayerError::Backend(
                "mpv CLI introuvable — libmpv natif préféré; installez mpv pour le fallback".into(),
            )
        })?;
        debug!(%endpoint, %mpv_bin, "start_mpv_cli");
        let ipc = ipc_socket_path();
        let mpeg_ts = looks_like_mpeg_ts(url);
        let vod = looks_like_vod_container(url);
        let demux_cache = if vod {
            self.opts.demux_secs.max(20.0)
        } else if mpeg_ts {
            self.opts.demux_secs.max(8.0)
        } else {
            self.opts.demux_secs.max(4.0)
        };
        let cache_secs = demux_cache.max(self.opts.cache_ms as f32 / 1000.0);
        let mut args = vec![
            "--force-window=yes".into(),
            "--keep-open=yes".into(),
            // Must stay yes: idle=no + failed open → immediate exit (status 1).
            "--idle=yes".into(),
            "--osc=no".into(),
            "--osd-level=0".into(),
            "--input-default-bindings=no".into(),
            "--input-vo-keyboard=no".into(),
            // focus-on-open was removed; focus-on=never is the replacement.
            "--focus-on=never".into(),
            "--title=FluxPlay".into(),
            "--ytdl=no".into(),
            format!("--input-ipc-server={}", ipc.display()),
            format!("--volume={}", (self.opts.volume * 100.0) as u32),
            format!("--cache-secs={cache_secs}"),
            format!(
                "--demuxer-readahead-secs={}",
                if vod { 12.0 } else if mpeg_ts { 4.0 } else { 2.0 }
            ),
            "--cache=yes".into(),
            "--stream-lavf-o=reconnect_streamed=1,reconnect_delay_max=5,reconnect_on_network_error=1".into(),
            "--demuxer-lavf-o=reconnect_streamed=1".into(),
            format!(
                "--user-agent={}",
                self.opts
                    .user_agent
                    .as_deref()
                    .unwrap_or("IPTVSmartersPlayer")
            ),
        ];
        if let Some(proxy) = &self.opts.http_proxy {
            args.push(format!("--http-proxy={proxy}"));
        }
        if let Some(rect) = self.video_rect.filter(|r| r.is_usable()) {
            args.push(format!("--geometry={}", rect.to_geometry()));
            if rect.anchored {
                args.extend([
                    "--border=no".into(),
                    "--window-dragging=no".into(),
                    "--keepaspect-window=no".into(),
                    "--ontop=yes".into(),
                ]);
            } else {
                args.extend([
                    "--border=yes".into(),
                    "--window-dragging=yes".into(),
                    "--keepaspect-window=yes".into(),
                ]);
            }
        } else {
            args.push("--geometry=960x540".into());
            args.extend([
                "--border=yes".into(),
                "--window-dragging=yes".into(),
                "--keepaspect-window=yes".into(),
            ]);
        }

        if mpeg_ts || vod {
            args.extend([
                "--demuxer-lavf-probesize=10000000".into(),
                "--demuxer-lavf-analyzeduration=5".into(),
                "--cache-pause-initial=yes".into(),
            ]);
        }

        if self.opts.hwdec {
            args.push(format!("--hwdec={}", preferred_hwdec()));
        } else {
            args.push("--hwdec=no".into());
        }

        if self.opts.low_latency && !mpeg_ts && !vod {
            args.extend([
                "--profile=low-latency".into(),
                "--cache=no".into(),
                "--untimed=yes".into(),
            ]);
        }

        if let Some(ref_r) = &self.opts.referer {
            let safe = sanitize_http_field(ref_r);
            if !safe.is_empty() {
                args.push(format!("--referrer={safe}"));
            }
        }
        if !self.opts.extra_headers.is_empty() {
            let joined = self
                .opts
                .extra_headers
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{}: {}",
                        sanitize_http_field(k),
                        sanitize_http_field(v)
                    )
                })
                .filter(|line| line.len() > 2)
                .collect::<Vec<_>>()
                .join("\r\n");
            if !joined.is_empty() {
                args.push(format!("--http-header-fields={joined}"));
            }
        }

        args.push(url.into());

        if let Some(level) = crate::native_log::mpv_msg_level() {
            args.push(format!("--msg-level={level}"));
            // `-v` stacks; for debug/trace add a second bump.
            if level.contains("debug") || level.contains("trace") {
                args.push("-v".into());
                args.push("-v".into());
            } else {
                args.push("-v".into());
            }
            let vlog = crate::native_log::mpv_verbose_log_path();
            args.push(format!("--log-file={}", vlog.display()));
            info!(msg_level = level, log = %vlog.display(), "mpv CLI verbose logging enabled");
        }

        let log = std::env::temp_dir().join("fluxplay-mpv.log");
        let err_file = std::fs::File::create(&log).ok();

        let child = Command::new(&mpv_bin)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(err_file.map(Stdio::from).unwrap_or_else(Stdio::null))
            .spawn()
            .map_err(|e| PlayerError::Backend(format!("mpv spawn ({mpv_bin}): {e}")))?;

        self.child = Some(child);
        self.ipc_path = Some(ipc);
        self.apply_video_geometry();
        info!(%endpoint, %mpv_bin, log = %log.display(), "mpv CLI spawned");
        Ok(())
    }

    fn start_ffmpeg(&mut self, url: &str) -> Result<()> {
        let endpoint = url_endpoint(url);
        debug!(%endpoint, "start_ffmpeg");
        #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
        {
            match self.start_libffmpeg(url) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    warn!(error = %e, "libffmpeg start failed — trying ffplay CLI fallback");
                    #[cfg(not(feature = "cli-player"))]
                    {
                        return Err(e);
                    }
                }
            }
        }
        #[cfg(feature = "cli-player")]
        {
            self.start_ffplay(url)
        }
        #[cfg(not(feature = "cli-player"))]
        {
            Err(PlayerError::Backend(
                "Aucun backend FFmpeg (activez native-ffmpeg ou cli-player)".into(),
            ))
        }
    }

    #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
    fn start_libffmpeg(&mut self, url: &str) -> Result<()> {
        let _prof = Stopwatch::start("start_libffmpeg");
        let endpoint = url_endpoint(url);
        debug!(%endpoint, "start_libffmpeg embedded");
        let ff = crate::ffmpeg_ffi::LibFfmpeg::open(
            url,
            self.opts.user_agent.as_deref(),
            self.opts.referer.as_deref(),
            self.opts.http_proxy.as_deref(),
            self.opts.low_latency,
            self.opts.hwdec,
        )?;
        ff.set_volume(self.opts.volume);
        if let Some(rect) = self.video_rect.filter(|r| r.is_usable()) {
            // Same soft cap as iced pull — uncapped window size caused mismatch discards.
            let (rw, rh) = crate::ffmpeg_ffi::LibFfmpeg::soft_present_dims(rect.w, rect.h);
            ff.set_output_size(rw, rh);
        }
        self.libffmpeg = Some(ff);
        info!(%endpoint, "libffmpeg embedded open ok");
        Ok(())
    }

    #[cfg(feature = "cli-player")]
    fn start_ffplay(&mut self, url: &str) -> Result<()> {
        let _prof = Stopwatch::start("start_ffplay");
        let endpoint = url_endpoint(url);
        let bin = which("ffplay").ok_or_else(|| {
            if which("ffmpeg").is_some() {
                PlayerError::Backend(
                    "ffmpeg trouvé mais pas ffplay — installez le paquet ffplay (souvent `ffmpeg`) pour la lecture GUI".into(),
                )
            } else {
                PlayerError::Backend(
                    "ffplay introuvable — installez ffmpeg/ffplay, ou choisissez libmpv".into(),
                )
            }
        })?;
        info!(%endpoint, %bin, "start_ffplay");

        let log = std::env::temp_dir().join("fluxplay-ffplay.log");
        let ff_loglevel = match crate::native_log::ffmpeg_av_log_level() {
            Some(48..=i32::MAX) => "debug", // AV_LOG_DEBUG+
            Some(40..) => "verbose",
            Some(_) => "info",
            None => "info",
        };
        let try_spawn = |extra_compat: bool| -> Result<std::process::Child> {
            let mut cmd = Command::new(&bin);
            cmd.arg("-hide_banner")
                .arg("-loglevel")
                .arg(ff_loglevel)
                .arg("-window_title")
                .arg(FFPLAY_WINDOW_TITLE)
                .arg("-autoexit")
                .arg("-volume")
                .arg(format!("{}", (self.opts.volume.clamp(0.0, 1.0) * 100.0) as u32))
                .arg("-x")
                .arg("1280")
                .arg("-y")
                .arg("720");

            if let Some(ua) = &self.opts.user_agent {
                cmd.arg("-user_agent").arg(ua);
            } else {
                cmd.arg("-user_agent").arg("IPTVSmartersPlayer");
            }
            if let Some(ref_r) = &self.opts.referer {
                let safe = sanitize_http_field(ref_r);
                if !safe.is_empty() {
                    cmd.arg("-headers").arg(format!("Referer: {safe}\r\n"));
                }
            }
            if let Some(proxy) = &self.opts.http_proxy {
                cmd.arg("-http_proxy").arg(proxy);
            }

            // Probe room for HEVC IPTV / VOD (before -i).
            cmd.arg("-probesize").arg("8M");
            cmd.arg("-analyzeduration").arg("5000000");
            cmd.arg("-fflags").arg("+genpts+discardcorrupt");

            if !extra_compat {
                // Optional resilience — some older ffplay builds reject these.
                cmd.arg("-infbuf");
                if self.opts.hwdec {
                    cmd.arg("-hwaccel").arg("auto");
                }
                let mpeg_ts = looks_like_mpeg_ts(url);
                let is_hls = url.to_ascii_lowercase().contains(".m3u8");
                if self.opts.low_latency && !mpeg_ts && !is_hls {
                    cmd.arg("-fflags").arg("+genpts+discardcorrupt+nobuffer");
                    cmd.arg("-flags").arg("low_delay");
                    cmd.arg("-framedrop");
                } else {
                    // Sync video to audio (not external clock) so sound stays audible/locked.
                    cmd.arg("-fflags").arg("+genpts+discardcorrupt");
                    cmd.arg("-sync").arg("audio");
                }
                // HTTP reconnect (input options; ignored if unsupported).
                cmd.arg("-reconnect").arg("1");
                cmd.arg("-reconnect_streamed").arg("1");
                cmd.arg("-reconnect_delay_max").arg("5");
            }

            cmd.arg("-i").arg(url);

            let err_file = std::fs::File::create(&log).ok();
            debug!(%endpoint, compat = extra_compat, log = %log.display(), "ffplay spawn");
            cmd.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(err_file.map(Stdio::from).unwrap_or_else(Stdio::null))
                .spawn()
                .map_err(|e| PlayerError::Backend(format!("ffplay spawn: {e}")))
        };

        let mut child = try_spawn(false)?;
        // Detect instant death from unsupported flags → retry minimal args.
        std::thread::sleep(std::time::Duration::from_millis(350));
        if let Ok(Some(status)) = child.try_wait() {
            warn!(%status, %endpoint, "ffplay exited with full args — retrying compatible mode");
            let hint = std::fs::read_to_string(&log).unwrap_or_default();
            if !hint.trim().is_empty() {
                warn!(ffplay_log = %hint.chars().take(800).collect::<String>(), "ffplay stderr");
            }
            child = try_spawn(true)?;
            std::thread::sleep(std::time::Duration::from_millis(400));
            if let Ok(Some(status)) = child.try_wait() {
                let hint = std::fs::read_to_string(&log).unwrap_or_default();
                error!(%status, %endpoint, "ffplay failed in compatible mode");
                return Err(PlayerError::Backend(format!(
                    "ffplay fermé aussitôt (code {status}). {}",
                    if hint.trim().is_empty() {
                        "Voir /tmp/fluxplay-ffplay.log".into()
                    } else {
                        hint.chars().take(400).collect::<String>()
                    }
                )));
            }
        }

        self.child = Some(child);
        self.ffplay_muted = false;
        self.ffplay_paused = false;
        info!(
            %endpoint,
            %bin,
            log = %log.display(),
            "ffplay spawned (external window — not embedded in iced)"
        );
        Ok(())
    }
}

impl Drop for NativePlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(not(target_os = "android"))] // CLI/IPC player path — desktop only
fn ipc_socket_path() -> PathBuf {
    let n = IPC_SEQ.fetch_add(1, Ordering::Relaxed);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    #[cfg(windows)]
    {
        // mpv on Windows uses named pipes: \\.\pipe\name
        return PathBuf::from(format!(r"\\.\pipe\fluxplay-mpv-{ts}-{n}"));
    }
    #[cfg(not(windows))]
    {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        let path = dir.join(format!("fluxplay-mpv-{ts}-{n}.sock"));
        // Restrict to owner before mpv binds (best-effort; race is narrow).
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .and_then(|_| std::fs::remove_file(&path));
        }
        path
    }
}

const FFPLAY_WINDOW_TITLE: &str = "FluxPlay Video";

/// Best-effort remote keys into the ffplay SDL window (requires xdotool on PATH).
fn ffplay_send_key(key: &str) -> bool {
    Command::new("xdotool")
        .args([
            "search",
            "--name",
            FFPLAY_WINDOW_TITLE,
            "windowactivate",
            "--sync",
            "key",
            "--clearmodifiers",
            key,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn mpv_get_number(ipc: &Path, prop: &str) -> Option<f64> {
    let cmd = serde_json::json!({ "command": ["get_property", prop] });
    let line = format!("{cmd}\n");
    mpv_ipc_data(ipc, &line).and_then(|d| {
        d.as_f64()
            .or_else(|| d.as_i64().map(|i| i as f64))
            .or_else(|| d.as_bool().map(|b| if b { 1.0 } else { 0.0 }))
    })
}

fn mpv_get_bool(ipc: &Path, prop: &str) -> Option<bool> {
    let cmd = serde_json::json!({ "command": ["get_property", prop] });
    let line = format!("{cmd}\n");
    mpv_ipc_data(ipc, &line).and_then(|d| {
        d.as_bool()
            .or_else(|| d.as_f64().map(|n| n >= 0.5))
            .or_else(|| {
                d.as_str()
                    .map(|s| s == "yes" || s == "true" || s == "1")
            })
    })
}

fn mpv_get_string(ipc: &Path, prop: &str) -> Option<String> {
    let cmd = serde_json::json!({ "command": ["get_property", prop] });
    let line = format!("{cmd}\n");
    mpv_ipc_data(ipc, &line).and_then(|d| {
        d.as_str()
            .map(|s| s.to_string())
            .or_else(|| d.as_f64().map(|n| n.to_string()))
    })
}

fn mpv_ipc_data(ipc: &Path, line: &str) -> Option<serde_json::Value> {
    let mut stream = mpv_ipc_stream(ipc)?;
    stream.write_all(line.as_bytes()).ok()?;
    let _ = stream.shutdown_write();
    let raw = stream.read_line_timeout(std::time::Duration::from_millis(800))?;
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
        if v.get("event").is_some() {
            return None;
        }
        if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
            if err != "success" {
                return None;
            }
        }
        return v.get("data").cloned();
    }
    None
}

struct MpvIpcStream {
    #[cfg(unix)]
    inner: std::os::unix::net::UnixStream,
    #[cfg(windows)]
    inner: std::fs::File,
}

impl MpvIpcStream {
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        self.inner.write_all(buf)
    }

    fn shutdown_write(&mut self) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            self.inner.shutdown(Shutdown::Write)
        }
        #[cfg(windows)]
        {
            use std::io::Write;
            self.inner.flush()
        }
    }

    /// One JSON IPC line — never `read_to_string` (mpv keeps the pipe open → hang).
    fn read_line_timeout(&mut self, timeout: std::time::Duration) -> Option<String> {
        use std::io::Read;
        #[cfg(unix)]
        {
            let _ = self.inner.set_read_timeout(Some(timeout));
        }
        let deadline = std::time::Instant::now() + timeout;
        let mut acc = Vec::with_capacity(256);
        let mut tmp = [0u8; 128];
        while std::time::Instant::now() < deadline {
            match self.inner.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => {
                    acc.extend_from_slice(&tmp[..n]);
                    if acc.contains(&b'\n') {
                        break;
                    }
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
        if acc.is_empty() {
            return None;
        }
        String::from_utf8(acc).ok().map(|s| {
            s.lines().next().unwrap_or("").trim().to_string()
        })
    }
}

fn mpv_ipc_stream(ipc: &Path) -> Option<MpvIpcStream> {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        let inner = UnixStream::connect(ipc).ok()?;
        let _ = inner.set_read_timeout(Some(std::time::Duration::from_millis(800)));
        Some(MpvIpcStream { inner })
    }
    #[cfg(windows)]
    {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;
        const ACCESS: u32 = 0x8000_0000 | 0x4000_0000;
        OpenOptions::new()
            .read(true)
            .write(true)
            .access_mode(ACCESS)
            .open(ipc)
            .ok()
            .map(|inner| MpvIpcStream { inner })
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        let _ = ipc;
        None
    }
}

fn mpv_cmd(ipc: &Path, parts: &[&str]) -> Result<()> {
    let cmd = serde_json::json!({ "command": parts });
    let line = format!("{cmd}\n");

    let mut last = None;
    for _ in 0..20 {
        match mpv_ipc_stream(ipc) {
            Some(mut stream) => {
                stream
                    .write_all(line.as_bytes())
                    .map_err(|e| PlayerError::Backend(e.to_string()))?;
                let _ = stream.shutdown_write();
                if let Some(raw) =
                    stream.read_line_timeout(std::time::Duration::from_millis(800))
                {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                        if v.get("event").is_none() {
                            match v.get("error").and_then(|e| e.as_str()) {
                                Some("success") | None => return Ok(()),
                                Some(err) => {
                                    return Err(PlayerError::Backend(format!(
                                        "mpv IPC {parts:?}: {err}"
                                    )));
                                }
                            }
                        }
                    }
                }
                return Ok(());
            }
            None => {
                last = Some("connect failed");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
    warn!(?last, "mpv ipc connect failed");
    Err(PlayerError::Backend(format!(
        "mpv IPC: {}",
        last.unwrap_or("unavailable")
    )))
}
