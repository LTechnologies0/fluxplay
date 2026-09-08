//! Native playback backends — **libmpv** / **libav* FFmpeg** in-process (embedded RGBA),
//! plus optional CLI mpv/ffplay fallback.
//! Inspired by IPTVnator embedded MPV and Kodi's FFmpeg pipeline.

use std::io::Write;
use std::net::Shutdown;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use fluxplay_core::models::PlayerBackendPref;
use fluxplay_core::Stopwatch;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, trace, warn};

use crate::{url_endpoint, PlayerError, Result};

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
        "mediacodec-copy"
    }
    #[cfg(not(target_os = "android"))]
    {
        // Same policy for embed + CLI (auto-copy / auto-safe are close; prefer auto-copy).
        "auto-copy"
    }
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
        for dir in path.split(':') {
            candidates.push(format!("{dir}/{bin}"));
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
    libmpv: Option<crate::mpv_ffi::LibMpv>,
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

    /// mpv `paused-for-cache` — drives Buffering state.
    pub fn paused_for_cache(&self) -> bool {
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = &self.libmpv {
            return matches!(
                mpv.get_property_string("paused-for-cache").as_deref(),
                Some("yes") | Some("true")
            );
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
            return self.libffmpeg.is_none();
        }
        #[cfg(not(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg)))]
        {
            true
        }
    }

    fn using_libffmpeg(&self) -> bool {
        #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
        {
            return self.libffmpeg.is_some();
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
    pub fn pull_video_frame(&mut self, w: u32, h: u32) -> Option<(u32, u32, Vec<u8>)> {
        let rw = (w.clamp(2, 3840) & !1).max(2);
        let rh = (h.clamp(2, 2160) & !1).max(2);
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.libmpv.as_mut() {
            let pixels = mpv.render_sw_rgba(rw, rh)?;
            self.soft_shot_tick = self.soft_shot_tick.wrapping_add(1);
            // Snapshot for screenshot ~1×/s at 60 Hz — avoid Arc copy hitch every frame.
            if self.last_soft_rgba.is_none() || self.soft_shot_tick % 60 == 0 {
                self.last_soft_rgba = Some((rw, rh, Arc::from(pixels.as_slice())));
            }
            return Some((rw, rh, pixels));
        }
        #[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
        if let Some(ff) = self.libffmpeg.as_ref() {
            let (rw, rh) = crate::ffmpeg_ffi::LibFfmpeg::soft_present_dims(w, h);
            let pixels = ff.pull_rgba(rw, rh)?;
            self.soft_shot_tick = self.soft_shot_tick.wrapping_add(1);
            if self.last_soft_rgba.is_none() || self.soft_shot_tick % 60 == 0 {
                self.last_soft_rgba = Some((rw, rh, Arc::from(pixels.as_slice())));
            }
            return Some((rw, rh, pixels));
        }
        let _ = (rw, rh);
        None
    }

    /// Return a discarded soft RGBA buffer to the embed pool (mpv capacity recycle).
    pub fn recycle_soft_rgba(&mut self, buf: Vec<u8>) {
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = self.libmpv.as_mut() {
            mpv.recycle_sw_rgba(buf);
            return;
        }
        let _ = buf;
    }

    /// True when embedded backend has a newer frame than the last successful pull.
    pub fn frame_needs_redraw(&self) -> bool {
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = &self.libmpv {
            return mpv.frame_needs_redraw();
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
            if let Some(mpv) = &self.libmpv {
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
        self.stop();
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
            if let Some(mpv) = self.libmpv.take() {
                debug!("NativePlayer::stop libmpv shutdown");
                mpv.shutdown();
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
        self.last_soft_rgba = None;
        self.soft_shot_tick = 0;
        self.ffplay_muted = false;
        self.ffplay_paused = false;
    }

    pub fn pause(&mut self, paused: bool) -> Result<()> {
        debug!(paused, backend = ?self.backend, "NativePlayer::pause");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = &self.libmpv {
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
        if let Some(mpv) = &self.libmpv {
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
        if let Some(mpv) = &self.libmpv {
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
        if let Some(mpv) = &self.libmpv {
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
        if let Some(mpv) = &self.libmpv {
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
        if let Some(mpv) = &self.libmpv {
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
        if let Some(mpv) = &self.libmpv {
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
        if let Some(mpv) = &self.libmpv {
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
        if let Some(mpv) = &self.libmpv {
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
        if let Some(mpv) = &self.libmpv {
            let pos = mpv.get_property_f64("time-pos")?;
            let dur = mpv.get_property_f64("duration").unwrap_or(0.0);
            trace!(pos, dur, "playback_times libmpv");
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
        if let Some(mpv) = &self.libmpv {
            let fps = mpv
                .get_property_f64("container-fps")
                .filter(|f| *f >= 20.0 && f.is_finite())
                .or_else(|| {
                    mpv.get_property_f64("estimated-vf-fps")
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

    fn set_prop(&self, name: &str, value: &str) -> Result<()> {
        debug!(%name, %value, "NativePlayer::set_prop");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = &self.libmpv {
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
        if let Some(mpv) = &self.libmpv {
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
        if delta >= 0 {
            self.run_cmd(&["add", "chapter", &format!("{delta}")])
        } else {
            self.run_cmd(&["add", "chapter", &format!("{delta}")])
        }
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
        #[cfg(target_os = "android")]
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
            soft_set(&mpv, "msg-level", level);
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
        // vo=libmpv is required for mpv_render SW → iced RGBA. Fail clearly if missing.
        mpv.set_option("vo", "libmpv").map_err(|e| {
            PlayerError::Backend(format!(
                "{e} — libmpv Android sans vo=libmpv (media-kit incomplet?)"
            ))
        })?;
        soft_set(&mpv, "force-window", "no");
        soft_set(&mpv, "keep-open", "yes");
        soft_set(&mpv, "ytdl", "no");
        soft_set(&mpv, "title", "FluxPlay");
        soft_set(&mpv, "osc", "no");
        soft_set(&mpv, "osd-level", "0");
        soft_set(&mpv, "input-default-bindings", "no");
        soft_set(&mpv, "input-vo-keyboard", "no");
        soft_set(&mpv, "video-timing-offset", "0");
        // Android NativeActivity: OpenSLES audio; soft RGBA present (vo=libmpv).
        #[cfg(target_os = "android")]
        {
            // Hard-require OpenSLES — soft_set hid silent audio death on media-kit builds.
            mpv.set_option("ao", "opensles").map_err(|e| {
                PlayerError::Backend(format!("{e} — ao=opensles requis sur Android"))
            })?;
            soft_set(&mpv, "audio-device", "auto");
        }
        soft_set(
            &mpv,
            "volume",
            &format!("{}", (self.opts.volume * 100.0) as u32),
        );
        soft_set(&mpv, "cache-secs", &format!("{cache_secs}"));
        soft_set(
            &mpv,
            "demuxer-readahead-secs",
            &format!("{}", if vod { 12.0 } else if mpeg_ts { 4.0 } else { 2.0 }),
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
            soft_set(&mpv, "hwdec", preferred_hwdec());
            soft_set(&mpv, "hwdec-codecs", "all");
        } else {
            soft_set(&mpv, "hwdec", "no");
        }
        // Fast filters for CPU soft-render path (stage already matches window size).
        soft_set(&mpv, "scale", "bilinear");
        soft_set(&mpv, "cscale", "bilinear");
        soft_set(&mpv, "dscale", "bilinear");
        soft_set(&mpv, "correct-downscaling", "no");
        soft_set(&mpv, "sigmoid-upscaling", "no");
        soft_set(&mpv, "interpolation", "no");
        soft_set(&mpv, "video-sync", "audio");
        soft_set(&mpv, "framedrop", "vo");
        soft_set(&mpv, "vd-lavc-threads", "0");

        if self.opts.low_latency && !mpeg_ts && !vod {
            soft_set(&mpv, "profile", "low-latency");
            soft_set(&mpv, "cache", "no");
            soft_set(&mpv, "untimed", "yes");
        }

        if let Some(ua) = &self.opts.user_agent {
            soft_set(&mpv, "user-agent", ua);
        } else {
            soft_set(&mpv, "user-agent", "IPTVSmartersPlayer");
        }
        if let Some(proxy) = &self.opts.http_proxy {
            // FFmpeg lavf accepts socks5h:// for HTTP(S) streams when built with it.
            soft_set(&mpv, "http-proxy", proxy);
            soft_set(&mpv, "ytdl-raw-options", &format!("proxy={proxy}"));
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
        mpv.init_sw_render()?;
        mpv.command(&["loadfile", url, "replace"])?;
        // Desktop only: brief settle so hwdec-current is meaningful. Never sleep on
        // Android UI thread (ANR / frozen NativeActivity).
        #[cfg(not(target_os = "android"))]
        std::thread::sleep(std::time::Duration::from_millis(200));
        let hw = mpv
            .get_property_string("hwdec-current")
            .unwrap_or_else(|| "none".into());
        let vo = mpv
            .get_property_string("current-vo")
            .unwrap_or_else(|| "?".into());
        self.libmpv = Some(mpv);
        info!(%endpoint, %hw, %vo, "libmpv embedded loadfile ok");
        Ok(())
    }

    #[cfg(feature = "cli-player")]
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
            return self.start_ffplay(url);
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

fn ipc_socket_path() -> PathBuf {
    let n = IPC_SEQ.fetch_add(1, Ordering::Relaxed);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dir = std::env::temp_dir();
    #[cfg(windows)]
    {
        // mpv on Windows uses named pipes: \\.\pipe\name
        return PathBuf::from(format!(r"\\.\pipe\fluxplay-mpv-{ts}-{n}"));
    }
    #[cfg(not(windows))]
    {
        dir.join(format!("fluxplay-mpv-{ts}-{n}.sock"))
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
    #[cfg(unix)]
    {
        use std::io::Read;
        use std::os::unix::net::UnixStream;
        let mut stream = UnixStream::connect(ipc).ok()?;
        stream.write_all(line.as_bytes()).ok()?;
        let _ = stream.shutdown(Shutdown::Write);
        let mut buf = String::new();
        let _ = stream.read_to_string(&mut buf);
        for raw in buf.lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
                if v.get("event").is_some() {
                    continue;
                }
                if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
                    if err != "success" {
                        continue;
                    }
                }
                if let Some(data) = v.get("data") {
                    return Some(data.clone());
                }
            }
        }
        None
    }
    #[cfg(not(unix))]
    {
        let _ = (ipc, line);
        None
    }
}

fn mpv_cmd(ipc: &Path, parts: &[&str]) -> Result<()> {
    // JSON IPC: { "command": ["loadfile", "url"] }
    let cmd = serde_json::json!({ "command": parts });
    let line = format!("{cmd}\n");

    #[cfg(unix)]
    {
        use std::io::Read;
        use std::os::unix::net::UnixStream;
        // Brief retry — mpv may still be starting.
        let mut last = None;
        for _ in 0..20 {
            match UnixStream::connect(ipc) {
                Ok(mut stream) => {
                    stream
                        .write_all(line.as_bytes())
                        .map_err(|e| PlayerError::Backend(e.to_string()))?;
                    let _ = stream.shutdown(Shutdown::Write);
                    let mut buf = String::new();
                    let _ = stream.read_to_string(&mut buf);
                    for raw in buf.lines() {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
                            if v.get("event").is_some() {
                                continue;
                            }
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
                    // No reply parsed — treat as soft success (older mpv).
                    return Ok(());
                }
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
        warn!(?last, "mpv ipc connect failed");
        Err(PlayerError::Backend(format!(
            "mpv IPC: {}",
            last.map(|e| e.to_string()).unwrap_or_default()
        )))
    }
    #[cfg(not(unix))]
    {
        let _ = (ipc, line);
        Err(PlayerError::Backend(
            "mpv IPC non supporté sur cette plateforme".into(),
        ))
    }
}
