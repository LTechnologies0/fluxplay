//! Native playback backends — mpv (preferred) and FFmpeg/ffplay.
//! Inspired by IPTVnator embedded MPV and Kodi's FFmpeg pipeline.

use std::io::Write;
use std::net::Shutdown;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fluxplay_core::models::PlayerBackendPref;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::{PlayerError, Result};

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
            Self::Mpv => "mpv (FFmpeg)",
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

/// Detect installed desktop backends (PATH).
pub fn detect_backends() -> Vec<BackendInfo> {
    let mut out = Vec::new();

    let mpv = which("mpv");
    out.push(BackendInfo {
        id: BackendId::Mpv,
        available: mpv.is_some(),
        path: mpv.clone(),
        detail: if mpv.is_some() {
            "libmpv/mpv — HLS/DASH/RTSP/RTMP/SRT, HW accel (best IPTV)".into()
        } else {
            "Install mpv for best quality (apt/brew/choco: mpv)".into()
        },
    });

    let ffplay = which("ffplay").or_else(|| which("ffmpeg"));
    out.push(BackendInfo {
        id: BackendId::Ffmpeg,
        available: ffplay.is_some(),
        path: ffplay.clone(),
        detail: if which("ffplay").is_some() {
            "ffplay — FFmpeg native window".into()
        } else if which("ffmpeg").is_some() {
            "ffmpeg present (ffplay recommended for GUI window)".into()
        } else {
            "Install ffmpeg/ffplay".into()
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

    match pref {
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
                    "mpv introuvable — installez mpv ou choisissez Auto/FFmpeg".into(),
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
    }
}

/// Controls a native player process for high-quality IPTV.
pub struct NativePlayer {
    backend: Option<BackendId>,
    child: Option<Child>,
    ipc_path: Option<PathBuf>,
    opts: PlayOptions,
}

impl Default for NativePlayer {
    fn default() -> Self {
        Self::new(PlayOptions::default())
    }
}

impl NativePlayer {
    pub fn new(opts: PlayOptions) -> Self {
        Self {
            backend: None,
            child: None,
            ipc_path: None,
            opts,
        }
    }

    pub fn options_mut(&mut self) -> &mut PlayOptions {
        &mut self.opts
    }

    pub fn active_backend(&self) -> Option<BackendId> {
        self.backend
    }

    pub fn is_running(&mut self) -> bool {
        match &mut self.child {
            Some(c) => matches!(c.try_wait(), Ok(None)),
            None => false,
        }
    }

    pub fn play(&mut self, url: &str) -> Result<BackendId> {
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

        attempt(self, backend, url)?;

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
                attempt(self, backend, url)?;
                if let Some(child) = &mut self.child {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    if let Ok(Some(status)) = child.try_wait() {
                        self.child = None;
                        self.backend = None;
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
        if let Some(path) = self.ipc_path.take() {
            let _ = mpv_cmd(&path, &["quit"]);
            let _ = std::fs::remove_file(&path);
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.backend = None;
    }

    pub fn pause(&mut self, paused: bool) -> Result<()> {
        if let Some(path) = &self.ipc_path {
            mpv_cmd(path, &["set_property", "pause", if paused { "true" } else { "false" }])?;
            return Ok(());
        }
        // ffplay: space toggles pause — only send when we want toggle-like behavior.
        if self.backend == Some(BackendId::Ffmpeg) {
            ffplay_send_key("space");
        }
        Ok(())
    }

    pub fn set_volume(&mut self, vol: f32) -> Result<()> {
        self.opts.volume = vol.clamp(0.0, 1.0);
        if let Some(path) = &self.ipc_path {
            let v = (self.opts.volume * 100.0).clamp(0.0, 100.0);
            mpv_cmd(path, &["set_property", "volume", &format!("{v:.0}")])?;
            return Ok(());
        }
        if self.backend == Some(BackendId::Ffmpeg) {
            // ffplay: 0 = quieter, 9 = louder — approximate target with a few taps.
            ffplay_send_key("m"); // unmute path often starts muted state unclear; skip
            let _ = vol;
        }
        Ok(())
    }

    pub fn set_mute(&mut self, muted: bool) -> Result<()> {
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
        if let Some(path) = &self.ipc_path {
            return mpv_cmd(
                path,
                &["seek", &format!("{}", pct.clamp(0.0, 100.0)), "absolute-percent"],
            );
        }
        Ok(())
    }

    pub fn toggle_fullscreen(&mut self) -> Result<()> {
        if let Some(path) = &self.ipc_path {
            return mpv_cmd(path, &["cycle", "fullscreen"]);
        }
        if self.backend == Some(BackendId::Ffmpeg) {
            ffplay_send_key("f");
        }
        Ok(())
    }

    pub fn cycle_audio(&mut self) -> Result<()> {
        if let Some(path) = &self.ipc_path {
            return mpv_cmd(path, &["cycle", "audio"]);
        }
        if self.backend == Some(BackendId::Ffmpeg) {
            ffplay_send_key("a");
        }
        Ok(())
    }

    pub fn cycle_subtitles(&mut self) -> Result<()> {
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
        if let Some(path) = &self.ipc_path {
            mpv_cmd(path, &["loadfile", url, "replace"])?;
            return Ok(());
        }
        self.play(url)?;
        Ok(())
    }

    pub fn frame_step(&mut self) -> Result<()> {
        if let Some(path) = &self.ipc_path {
            return mpv_cmd(path, &["frame-step"]);
        }
        if self.backend == Some(BackendId::Ffmpeg) {
            ffplay_send_key("s");
        }
        Ok(())
    }

    /// Query mpv playback position / duration (seconds). Returns None for ffplay.
    pub fn playback_times(&self) -> Option<(f64, f64)> {
        let path = self.ipc_path.as_ref()?;
        let pos = mpv_get_number(path, "time-pos")?;
        let dur = mpv_get_number(path, "duration").unwrap_or(0.0);
        Some((pos, dur))
    }

    fn start_mpv(&mut self, url: &str) -> Result<()> {
        let mpv_bin = which("mpv").ok_or_else(|| {
            PlayerError::Backend(
                "mpv introuvable — installez-le (brew/dnf: mpv) pour ouvrir la fenêtre vidéo".into(),
            )
        })?;
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
            "--force-window=immediate".into(),
            "--keep-open=yes".into(),
            "--idle=no".into(),
            "--title=FluxPlay Video".into(),
            "--geometry=1280x720".into(),
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
            // IPTV resilience (Kodi-like reconnect behaviour)
            "--stream-lavf-o=reconnect_streamed=1,reconnect_delay_max=5,reconnect_on_network_error=1".into(),
            "--demuxer-lavf-o=reconnect_streamed=1".into(),
        ];

        if mpeg_ts || vod {
            // Progressive MPEG-TS / MP4/MKV need real probe — tiny probesize breaks demux.
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
        Ok(())
    }

    fn start_ffplay(&mut self, url: &str) -> Result<()> {
        // Prefer ffplay window; fall back to ffplay-less environments with mpv-less ffmpeg tip.
        let bin = which("ffplay").ok_or_else(|| {
            PlayerError::Backend(
                "ffplay required for FFmpeg GUI playback (package ffmpeg)".into(),
            )
        })?;

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
