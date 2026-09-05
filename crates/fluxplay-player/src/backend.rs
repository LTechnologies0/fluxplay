//! Native playback backends — **libmpv in-process** (preferred) and optional CLI mpv/ffplay.
//! Inspired by IPTVnator embedded MPV and Kodi's FFmpeg pipeline.

use std::io::Write;
use std::net::Shutdown;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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
            Self::Ffmpeg => "FFmpeg / ffplay",
            Self::External => "Lecteur système",
            Self::ExoPlayer => "ExoPlayer (Android)",
            Self::AvPlayer => "AVPlayer (iOS)",
        }
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
}

/// Screen-space rectangle for the borderless mpv video surface (overlay on iced stage).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl VideoRect {
    pub fn to_geometry(self) -> String {
        format!("{}x{}{:+}{:+}", self.w, self.h, self.x, self.y)
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

    let ffplay = which("ffplay").or_else(|| which("ffmpeg"));
    out.push(BackendInfo {
        id: BackendId::Ffmpeg,
        available: ffplay.is_some(),
        path: ffplay.clone(),
        detail: if which("ffplay").is_some() {
            "ffplay — FFmpeg fenêtre (optionnel)".into()
        } else if which("ffmpeg").is_some() {
            "ffmpeg présent (ffplay recommandé pour GUI)".into()
        } else {
            "ffplay optionnel (libmpv suffit)".into()
        },
    });

    out.push(BackendInfo {
        id: BackendId::External,
        available: true,
        path: None,
        detail: "OS default handler / VLC / IINA".into(),
    });

    #[cfg(target_os = "android")]
    out.push(BackendInfo {
        id: BackendId::ExoPlayer,
        available: true,
        path: None,
        detail: "Media3 ExoPlayer via JNI shell".into(),
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

/// Controls native playback (in-process libmpv, optional CLI child).
pub struct NativePlayer {
    backend: Option<BackendId>,
    #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
    libmpv: Option<crate::mpv_ffi::LibMpv>,
    child: Option<Child>,
    ipc_path: Option<PathBuf>,
    opts: PlayOptions,
    /// Borderless mpv window locked to the iced player stage.
    video_rect: Option<VideoRect>,
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
            child: None,
            ipc_path: None,
            opts,
            video_rect: None,
        }
    }

    pub fn options_mut(&mut self) -> &mut PlayOptions {
        &mut self.opts
    }

    /// Pin the video surface over the iced lecteur stage (screen coordinates).
    pub fn set_video_rect(&mut self, rect: VideoRect) {
        if !rect.is_usable() {
            return;
        }
        if self.video_rect == Some(rect) {
            return;
        }
        debug!(?rect, "NativePlayer::set_video_rect");
        self.video_rect = Some(rect);
        self.apply_video_geometry();
    }

    pub fn video_rect(&self) -> Option<VideoRect> {
        self.video_rect
    }

    fn apply_video_geometry(&mut self) {
        let Some(rect) = self.video_rect.filter(|r| r.is_usable()) else {
            return;
        };
        let geo = rect.to_geometry();
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = &self.libmpv {
            let _ = mpv.set_property("geometry", &geo);
            let _ = mpv.set_property("border", "no");
            let _ = mpv.set_property("ontop", "no");
            return;
        }
        if let Some(path) = &self.ipc_path {
            let _ = mpv_cmd(path, &["set_property", "geometry", &geo]);
            let _ = mpv_cmd(path, &["set_property", "border", "no"]);
        }
    }

    pub fn active_backend(&self) -> Option<BackendId> {
        self.backend
    }

    pub fn is_running(&mut self) -> bool {
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        {
            if self.libmpv.is_some() {
                return true;
            }
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
        std::thread::sleep(std::time::Duration::from_millis(650));

        let backend = pick_backend(self.opts.preferred)?;
        let attempt = |this: &mut Self, backend: BackendId, url: &str| -> Result<()> {
            match backend {
                BackendId::Mpv => this.start_mpv(url),
                BackendId::Ffmpeg => this.start_ffplay(url),
                BackendId::External => {
                    open::that(url).map_err(|e| PlayerError::Backend(e.to_string()))
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
                        return Err(PlayerError::Backend(format!(
                            "lecteur fermé aussitôt (code {status}). Stoppez les autres clients IPTV (1 connexion max) ou vérifiez le flux."
                        )));
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
                debug!("NativePlayer::stop libmpv quit");
                let _ = mpv.command(&["quit"]);
                drop(mpv);
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
    }

    pub fn pause(&mut self, paused: bool) -> Result<()> {
        debug!(paused, backend = ?self.backend, "NativePlayer::pause");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = &self.libmpv {
            return mpv.set_property("pause", if paused { "yes" } else { "no" });
        }
        if let Some(path) = &self.ipc_path {
            mpv_cmd(path, &["set_property", "pause", if paused { "true" } else { "false" }])?;
            return Ok(());
        }
        if self.backend == Some(BackendId::Ffmpeg) {
            ffplay_send_key("space");
        }
        Ok(())
    }

    pub fn set_volume(&mut self, vol: f32) -> Result<()> {
        self.opts.volume = vol.clamp(0.0, 1.0);
        let v = (self.opts.volume * 100.0).clamp(0.0, 100.0);
        debug!(volume = self.opts.volume, "NativePlayer::set_volume");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = &self.libmpv {
            return mpv.set_property("volume", &format!("{v:.0}"));
        }
        if let Some(path) = &self.ipc_path {
            mpv_cmd(path, &["set_property", "volume", &format!("{v:.0}")])?;
            return Ok(());
        }
        if self.backend == Some(BackendId::Ffmpeg) {
            ffplay_send_key("m");
            let _ = vol;
        }
        Ok(())
    }

    pub fn set_mute(&mut self, muted: bool) -> Result<()> {
        debug!(muted, "NativePlayer::set_mute");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = &self.libmpv {
            return mpv.set_property("mute", if muted { "yes" } else { "no" });
        }
        if let Some(path) = &self.ipc_path {
            mpv_cmd(path, &["set_property", "mute", if muted { "yes" } else { "no" }])?;
            return Ok(());
        }
        if self.backend == Some(BackendId::Ffmpeg) {
            ffplay_send_key("m");
            let _ = muted;
        }
        Ok(())
    }

    /// Relative seek in seconds (negative = rewind). Best with mpv / VOD.
    pub fn seek_relative(&mut self, secs: f64) -> Result<()> {
        debug!(secs, "NativePlayer::seek_relative");
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = &self.libmpv {
            return mpv.command(&["seek", &format!("{secs}"), "relative"]);
        }
        if let Some(path) = &self.ipc_path {
            return mpv_cmd(path, &["seek", &format!("{secs}"), "relative"]);
        }
        if self.backend == Some(BackendId::Ffmpeg) {
            let key = if secs <= -25.0 {
                "Down"
            } else if secs < 0.0 {
                "Left"
            } else if secs >= 25.0 {
                "Up"
            } else {
                "Right"
            };
            ffplay_send_key(key);
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
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = &self.libmpv {
            return mpv.command(&["cycle", "fullscreen"]);
        }
        if let Some(path) = &self.ipc_path {
            return mpv_cmd(path, &["cycle", "fullscreen"]);
        }
        if self.backend == Some(BackendId::Ffmpeg) {
            ffplay_send_key("f");
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
        if self.backend == Some(BackendId::Ffmpeg) {
            ffplay_send_key("a");
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
        if self.backend == Some(BackendId::Ffmpeg) {
            ffplay_send_key("t");
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
        if self.backend == Some(BackendId::Ffmpeg) {
            ffplay_send_key("s");
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
        let path = self.ipc_path.as_ref()?;
        let pos = mpv_get_number(path, "time-pos")?;
        let dur = mpv_get_number(path, "duration").unwrap_or(0.0);
        trace!(pos, dur, "playback_times ipc");
        Some((pos, dur))
    }

    fn set_prop(&self, name: &str, value: &str) -> Result<()> {
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = &self.libmpv {
            return mpv.set_property(name, value);
        }
        if let Some(path) = &self.ipc_path {
            return mpv_cmd(path, &["set_property", name, value]);
        }
        Ok(())
    }

    fn run_cmd(&self, args: &[&str]) -> Result<()> {
        #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
        if let Some(mpv) = &self.libmpv {
            return mpv.command(args);
        }
        if let Some(path) = &self.ipc_path {
            return mpv_cmd(path, args);
        }
        Ok(())
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
        self.run_cmd(&["screenshot-to-file", path, "video"])
    }

    pub fn seek_absolute(&mut self, secs: f64) -> Result<()> {
        let secs = secs.max(0.0);
        debug!(secs, "NativePlayer::seek_absolute");
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
                    warn!(error = %e, "libmpv start failed — trying CLI fallback");
                    #[cfg(not(feature = "cli-player"))]
                    {
                        return Err(e);
                    }
                }
            }
        }
        #[cfg(feature = "cli-player")]
        {
            return self.start_mpv_cli(url);
        }
        #[cfg(not(feature = "cli-player"))]
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
        let cache_secs = if vod {
            self.opts.demux_secs.max(20.0)
        } else if mpeg_ts {
            self.opts.demux_secs.max(8.0)
        } else {
            self.opts.demux_secs.max(4.0)
        };
        debug!(%endpoint, mpeg_ts, vod, cache_secs, "start_libmpv");

        let mpv = crate::mpv_ffi::LibMpv::create()?;
        // Options that must be set before initialize().
        mpv.set_option("config", "no")?;
        mpv.set_option("terminal", "no")?;
        mpv.set_option("idle", "yes")?;
        mpv.set_option("force-window", "yes")?;
        mpv.set_option("keep-open", "yes")?;
        // Borderless surface snapped onto the iced player stage (not a 3rd chrome window).
        mpv.set_option("border", "no")?;
        mpv.set_option("osc", "no")?;
        mpv.set_option("osd-level", "0")?;
        mpv.set_option("input-default-bindings", "no")?;
        mpv.set_option("input-vo-keyboard", "no")?;
        mpv.set_option("window-dragging", "no")?;
        mpv.set_option("focus-on-open", "no")?;
        mpv.set_option("keepaspect-window", "no")?;
        mpv.set_option("title", "FluxPlay")?;
        mpv.set_option("ytdl", "no")?;
        if let Some(rect) = self.video_rect.filter(|r| r.is_usable()) {
            mpv.set_option("geometry", &rect.to_geometry())?;
        } else {
            mpv.set_option("geometry", "960x540")?;
        }
        mpv.set_option("volume", &format!("{}", (self.opts.volume * 100.0) as u32))?;
        mpv.set_option("cache-secs", &format!("{cache_secs}"))?;
        mpv.set_option(
            "demuxer-readahead-secs",
            &format!("{}", if vod { 12.0 } else if mpeg_ts { 4.0 } else { 2.0 }),
        )?;
        mpv.set_option("cache", "yes")?;
        mpv.set_option(
            "stream-lavf-o",
            "reconnect_streamed=1,reconnect_delay_max=5,reconnect_on_network_error=1",
        )?;
        mpv.set_option("demuxer-lavf-o", "reconnect_streamed=1")?;

        if mpeg_ts || vod {
            mpv.set_option("demuxer-lavf-probesize", "10000000")?;
            mpv.set_option("demuxer-lavf-analyzeduration", "5")?;
            mpv.set_option("cache-pause-initial", "yes")?;
        }

        if self.opts.hwdec {
            mpv.set_option("hwdec", "auto-safe")?;
        } else {
            mpv.set_option("hwdec", "no")?;
        }

        if self.opts.low_latency && !mpeg_ts && !vod {
            mpv.set_option("profile", "low-latency")?;
            mpv.set_option("cache", "no")?;
            mpv.set_option("untimed", "yes")?;
        }

        if let Some(ua) = &self.opts.user_agent {
            mpv.set_option("user-agent", ua)?;
        }
        if let Some(ref_r) = &self.opts.referer {
            mpv.set_option("referrer", ref_r)?;
        }
        if !self.opts.extra_headers.is_empty() {
            let joined = self
                .opts
                .extra_headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join("\r\n");
            mpv.set_option("http-header-fields", &joined)?;
        }

        mpv.initialize()?;
        mpv.command(&["loadfile", url, "replace"])?;
        self.libmpv = Some(mpv);
        self.apply_video_geometry();
        info!(%endpoint, "libmpv loadfile ok");
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
        let cache_secs = if vod {
            self.opts.demux_secs.max(20.0)
        } else if mpeg_ts {
            self.opts.demux_secs.max(8.0)
        } else {
            self.opts.demux_secs.max(4.0)
        };
        let mut args = vec![
            "--force-window=yes".into(),
            "--keep-open=yes".into(),
            "--idle=no".into(),
            "--border=no".into(),
            "--osc=no".into(),
            "--osd-level=0".into(),
            "--input-default-bindings=no".into(),
            "--input-vo-keyboard=no".into(),
            "--window-dragging=no".into(),
            "--focus-on-open=no".into(),
            "--keepaspect-window=no".into(),
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
            "--hwdec=auto".into(),
            "--stream-lavf-o=reconnect_streamed=1,reconnect_delay_max=5,reconnect_on_network_error=1".into(),
            "--demuxer-lavf-o=reconnect_streamed=1".into(),
        ];
        if let Some(rect) = self.video_rect.filter(|r| r.is_usable()) {
            args.push(format!("--geometry={}", rect.to_geometry()));
        } else {
            args.push("--geometry=960x540".into());
        }

        if mpeg_ts || vod {
            args.extend([
                "--demuxer-lavf-probesize=10000000".into(),
                "--demuxer-lavf-analyzeduration=5".into(),
                "--cache-pause-initial=yes".into(),
            ]);
        }

        if self.opts.hwdec {
            args.push("--hwdec=auto-safe".into());
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

        if let Some(ua) = &self.opts.user_agent {
            args.push(format!("--user-agent={ua}"));
        }
        if let Some(ref_r) = &self.opts.referer {
            args.push(format!("--referrer={ref_r}"));
        }
        if !self.opts.extra_headers.is_empty() {
            let joined = self
                .opts
                .extra_headers
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join("\r\n");
            args.push(format!("--http-header-fields={joined}"));
        }

        args.push(url.into());

        let child = Command::new(&mpv_bin)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| PlayerError::Backend(format!("mpv spawn ({mpv_bin}): {e}")))?;

        self.child = Some(child);
        self.ipc_path = Some(ipc);
        self.apply_video_geometry();
        info!(%endpoint, %mpv_bin, "mpv CLI spawned");
        Ok(())
    }

    fn start_ffplay(&mut self, url: &str) -> Result<()> {
        let _prof = Stopwatch::start("start_ffplay");
        let endpoint = url_endpoint(url);
        // Prefer ffplay window; fall back to ffplay-less environments with mpv-less ffmpeg tip.
        let bin = which("ffplay").ok_or_else(|| {
            PlayerError::Backend(
                "ffplay required for FFmpeg GUI playback (package ffmpeg)".into(),
            )
        })?;
        debug!(%endpoint, %bin, "start_ffplay");

        let mut cmd = Command::new(&bin);
        cmd.arg("-hide_banner")
            .arg("-loglevel")
            .arg("warning")
            .arg("-alwaysontop")
            .arg("-window_title")
            .arg(FFPLAY_WINDOW_TITLE)
            .arg("-infbuf")
            // IPTV reconnect — same idea as Smarters resilient players
            .arg("-reconnect")
            .arg("1")
            .arg("-reconnect_streamed")
            .arg("1")
            .arg("-reconnect_delay_max")
            .arg("5");

        if let Some(ua) = &self.opts.user_agent {
            cmd.arg("-user_agent").arg(ua);
        } else {
            cmd.arg("-user_agent").arg("IPTVSmartersPlayer");
        }
        if let Some(ref_r) = &self.opts.referer {
            cmd.arg("-headers").arg(format!("Referer: {ref_r}\r\n"));
        }
        if self.opts.hwdec {
            // Best-effort; ignored if unsupported.
            cmd.arg("-hwaccel").arg("auto");
        }
        let mpeg_ts = looks_like_mpeg_ts(url);
        let is_hls = url.to_ascii_lowercase().contains(".m3u8");
        // Always give lavf enough probe room for HEVC IPTV / VOD containers.
        cmd.arg("-probesize").arg("5M");
        cmd.arg("-analyzeduration").arg("3000000");
        if self.opts.low_latency && !mpeg_ts && !is_hls {
            cmd.arg("-fflags").arg("nobuffer");
            cmd.arg("-flags").arg("low_delay");
            cmd.arg("-framedrop");
        } else {
            cmd.arg("-fflags").arg("+genpts+discardcorrupt");
            cmd.arg("-sync").arg("ext");
        }

        cmd.arg("-i").arg(url);
        cmd.arg("-x").arg("1280").arg("-y").arg("720");

        // Keep a small log for diagnosis (tail when spawn fails).
        let log = std::env::temp_dir().join("fluxplay-ffplay.log");
        let err_file = std::fs::File::create(&log).ok();

        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(err_file.map(Stdio::from).unwrap_or_else(Stdio::null))
            .spawn()
            .map_err(|e| PlayerError::Backend(format!("ffplay spawn: {e}")))?;

        self.child = Some(child);
        info!(%endpoint, %bin, "ffplay spawned");
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
fn ffplay_send_key(key: &str) {
    let _ = Command::new("xdotool")
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
        .status();
}

fn mpv_get_number(ipc: &Path, prop: &str) -> Option<f64> {
    let cmd = serde_json::json!({ "command": ["get_property", prop] });
    let line = format!("{cmd}\n");
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
                if let Some(n) = v.get("data").and_then(|d| d.as_f64()) {
                    return Some(n);
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
        use std::os::unix::net::UnixStream;
        // Brief retry — mpv may still be starting.
        let mut last = None;
        for _ in 0..20 {
            match UnixStream::connect(ipc) {
                Ok(mut stream) => {
                    stream
                        .write_all(line.as_bytes())
                        .map_err(|e| PlayerError::Backend(e.to_string()))?;
                    let _ = stream.shutdown(Shutdown::Both);
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
        // Named pipe write on Windows — best effort via std::fs after create.
        let _ = (ipc, line);
        Ok(())
    }
}
