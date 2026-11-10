//! FluxPlay player — protocol routing + native backends (libmpv / FFmpeg / platform).
//!
//! Desktop: **libmpv** and **libav* FFmpeg** in-process (RGBA into iced), optional CLI fallback.
//! Android iced (`fluxplay-android`): vendored **libmpv** RGBA embed + `ACTION_VIEW` Intent.
//! iOS / legacy FFI: AVPlayer (experimental).

mod android_quality;
mod backend;
mod chapter_classify;
mod external;
#[cfg(test)]
mod prop_route;
#[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
mod ffmpeg_ffi;
#[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
mod ffmpeg_pump;
#[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
mod mpv_ffi;
#[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
mod soft_pump;
mod native_log;
mod platform;
mod session;
mod video_frame;

use fluxplay_core::codes::{classify_io, classify_message, FpCode, HasFpCode};
use fluxplay_core::redact_urls;
use fluxplay_core::protocol::{DeliveryKind, StreamScheme, StreamUrl};
use fluxplay_core::Channel;
use fluxplay_core::Stopwatch;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, info, trace, warn};

pub use android_quality::{AndroidDeviceCaps, AndroidPresentMode, CompatTier, SoftBudget};
pub use backend::{
    detect_backends, BackendCaps, BackendId, BackendInfo, MediaTrack, NativePlayer, PlayOptions,
    PlayerEvent, VideoRect,
};
pub use chapter_classify::{
    classify_chapter_title, credits_from_chapters as credits_start_from_chapters,
    opening_from_chapters as opening_span_from_chapters, set_chapter_keyword_extras, ChapterClass,
};
pub use external::{
    detect_external_players, launch_external, pick_external_player, ExternalLaunch,
    ExternalPlayer,
};
pub use native_log::{
    ffmpeg_av_log_level, log_native_verbosity_banner, mpv_msg_level, mpv_verbose_log_path,
    verbose_master,
};
pub use platform::{target_profile, Platform, TargetProfile};
pub use video_frame::{
    install_video_stage, video_stage_ready, yuv_to_rgb_coeffs, ColorMatrix, FrameData, GpuFrame,
    GpuStage, PixelLayout, VideoFrame,
};
#[cfg(unix)]
pub use video_frame::ExportedSlots;
pub use session::{
    AspectMode, AudioChannelMode, Bookmark, DeinterlaceMode, EqPreset, PlaybackState,
    StreamSession, UpscaleMode,
};

#[derive(Debug, Error)]
pub enum PlayerError {
    #[error("{}", display_unsupported(.0))]
    Unsupported(String),
    #[error("{}", display_invalid_url(.0))]
    InvalidUrl(String),
    #[error("{}", display_backend(.0))]
    Backend(String),
    #[error("{}", display_core(.0))]
    Core(#[from] fluxplay_core::Error),
    #[error("{}", display_http(.0))]
    Http(#[from] reqwest::Error),
    #[error("{}", display_io(.0))]
    Io(#[from] std::io::Error),
    #[error("{}", display_message(.0))]
    Message(String),
}

pub type Result<T> = std::result::Result<T, PlayerError>;

fn backend_code(msg: &str) -> FpCode {
    classify_message(msg)
}

fn display_unsupported(msg: &str) -> String {
    FpCode::AvUnsupportedScheme.ensure_coded(format!("unsupported protocol: {msg}"))
}

fn display_invalid_url(msg: &str) -> String {
    FpCode::AvInvalidUrl.ensure_coded(format!("invalid url: {msg}"))
}

fn display_backend(msg: &str) -> String {
    backend_code(msg).ensure_coded(format!("backend: {msg}"))
}

fn display_core(e: &fluxplay_core::Error) -> String {
    format!("core: {e}")
}

fn display_http(e: &reqwest::Error) -> String {
    // Prefer provider classifier when available; fall back to message heuristics.
    // reqwest Display often embeds the full request URL (Xtream creds in query).
    let code = http_code(e);
    code.ensure_coded(format!("http: {}", redact_urls(&e.to_string())))
}

fn http_code(e: &reqwest::Error) -> FpCode {
    let s = e.to_string().to_ascii_lowercase();
    if e.is_timeout() {
        FpCode::NetHttpTimeout
    } else if e.is_connect() {
        if s.contains("dns") || s.contains("resolve") {
            FpCode::NetDnsServfail
        } else if s.contains("certificate") || s.contains("cert") {
            FpCode::NetHttpsCert
        } else if s.contains("tls") || s.contains("ssl") {
            FpCode::NetHttpsTls
        } else {
            FpCode::NetTcpConnect
        }
    } else if s.contains("certificate") {
        FpCode::NetHttpsCert
    } else if s.contains("tls") || s.contains("ssl") {
        FpCode::NetHttpsTls
    } else {
        classify_message(&s)
    }
}

fn display_io(e: &std::io::Error) -> String {
    classify_io(e).ensure_coded(format!("io: {e}"))
}

fn display_message(msg: &str) -> String {
    classify_message(msg).ensure_coded(msg)
}

impl HasFpCode for PlayerError {
    fn fp_code(&self) -> FpCode {
        match self {
            Self::Unsupported(_) => FpCode::AvUnsupportedScheme,
            Self::InvalidUrl(_) => FpCode::AvInvalidUrl,
            Self::Backend(m) => backend_code(m),
            Self::Core(e) => e.fp_code(),
            Self::Http(e) => http_code(e),
            Self::Io(e) => classify_io(e),
            Self::Message(m) => classify_message(m),
        }
    }
}

impl PlayerError {
    /// Display already includes `[FP-…]`.
    pub fn coded(&self) -> String {
        self.to_string()
    }
}

#[cfg(test)]
mod code_tests {
    use super::*;

    #[test]
    fn backend_intent_code() {
        let e = PlayerError::Backend("EXTERNAL_NEEDS_INTENT".into());
        assert_eq!(e.fp_code(), FpCode::AvIntentRefuse);
        assert!(e.to_string().contains("[FP-AV-INTENT-REFUSE]"));
    }

    #[test]
    fn soft_rgba_code() {
        let e = PlayerError::Backend("SOFT_RGBA".into());
        assert_eq!(e.fp_code(), FpCode::AvSoftRgba);
    }

    #[test]
    fn unsupported_scheme_code() {
        let e = PlayerError::Unsupported("rtsp".into());
        assert_eq!(e.fp_code(), FpCode::AvUnsupportedScheme);
        assert!(e.to_string().starts_with("[FP-AV-SCHEME]"));
    }

    #[test]
    fn redact_urls_scrubs_password_query() {
        let raw = "error sending request for url (https://x.example/get.php?username=u&password=s3cret)";
        let out = redact_urls(raw);
        assert!(out.contains("<url>"), "{out}");
        assert!(!out.contains("s3cret"), "{out}");
    }
}

/// Network read-ahead of the embedded player (seconds of video already downloaded).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BufferState {
    /// Playback holds because the queue ran dry (mpv `paused-for-cache`).
    pub rebuffering: bool,
    pub buffered_secs: f64,
    /// Buffered seconds needed to resume.
    pub goal_secs: f64,
    /// Measured download rate (0 until known).
    pub net_mbps: f64,
    /// Average stream bitrate from the container (0 if unknown).
    pub media_mbps: f64,
}

impl BufferState {
    pub fn fill_percent(&self) -> u8 {
        if self.goal_secs <= 0.0 {
            return 0;
        }
        ((self.buffered_secs / self.goal_secs) * 100.0).clamp(0.0, 99.0) as u8
    }

    /// The link cannot sustain this stream: pauses will come back.
    pub fn link_too_slow(&self) -> bool {
        self.net_mbps > 0.0 && self.media_mbps > 0.0 && self.net_mbps < self.media_mbps * 0.97
    }
}

/// Register Android `JavaVM` with the FFmpeg inside libmpv (MediaCodec Surface).
/// Safe no-op when libmpv is not linked.
pub fn register_android_java_vm(vm: *mut std::ffi::c_void) -> bool {
    #[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
    {
        mpv_ffi::register_android_java_vm(vm)
    }
    #[cfg(not(all(feature = "native-mpv", fluxplay_has_libmpv)))]
    {
        let _ = vm;
        false
    }
}

/// Scheme + host only — avoids logging path/query credentials.
pub(crate) fn url_endpoint(raw: &str) -> String {
    if let Ok(u) = StreamUrl::parse(raw) {
        match (u.scheme.label(), u.host.as_deref()) {
            (scheme, Some(host)) => format!("{scheme}://{host}"),
            (scheme, None) => scheme.to_string(),
        }
    } else {
        // Unparsable: the scheme at most (the path may hold `/user/pass/`).
        raw.split_once("://")
            .map(|(scheme, _)| format!("{scheme}://…"))
            .unwrap_or_else(|| "<url>".into())
    }
}

/// A `file:` URL naming an existing file under `FLUXPLAY_DOWNLOAD_ROOT` (a download of
/// this app). Both sides are canonicalized, so `root/../../etc/…` does not qualify.
pub(crate) fn is_downloaded_file(url: &str) -> bool {
    let Some(path) = file_url_path(url) else {
        return false;
    };
    let Ok(root) = std::env::var("FLUXPLAY_DOWNLOAD_ROOT") else {
        return false;
    };
    if root.trim().is_empty() {
        return false;
    }
    match (std::fs::canonicalize(&path), std::fs::canonicalize(root.trim())) {
        (Ok(p), Ok(r)) => p.is_file() && p.starts_with(&r),
        _ => false,
    }
}

/// `file:///C:/Films/a.mkv` and `file://C:\Films\a.mkv` both become a path.
fn file_url_path(url: &str) -> Option<std::path::PathBuf> {
    let rest = url.strip_prefix("file://")?;
    if let Some(win) = rest.strip_prefix('/') {
        let bytes = win.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' {
            return Some(std::path::PathBuf::from(win.replace('/', "\\")));
        }
    }
    let bytes = rest.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        return Some(std::path::PathBuf::from(rest.replace('/', "\\")));
    }
    if rest.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(rest))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolSupport {
    pub scheme: StreamScheme,
    pub classify: bool,
    pub manifest: bool,
    pub decode: bool,
    pub notes: String,
}

pub fn support_matrix() -> Vec<ProtocolSupport> {
    let native = !detect_backends().is_empty();
    let decode_note = if cfg!(all(feature = "native-mpv", fluxplay_has_libmpv)) {
        "libmpv natif lié dans le binaire"
    } else if cfg!(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg)) {
        "FFmpeg libav* natif lié dans le binaire"
    } else if native {
        "native decode via mpv/FFmpeg when available"
    } else {
        "install mpv or ffmpeg/ffplay for native decode"
    };
    let matrix = vec![
        ProtocolSupport {
            scheme: StreamScheme::Http,
            classify: true,
            manifest: true,
            decode: native,
            notes: format!("HLS/DASH/progressive — {decode_note}"),
        },
        ProtocolSupport {
            scheme: StreamScheme::Https,
            classify: true,
            manifest: true,
            decode: native,
            notes: format!("TLS via rustls + backend — {decode_note}"),
        },
        ProtocolSupport {
            scheme: StreamScheme::Quic,
            classify: true,
            manifest: false,
            decode: native,
            notes: "HTTP/3 / QUIC — mpv if built with it".into(),
        },
        ProtocolSupport {
            scheme: StreamScheme::Rtmp,
            classify: true,
            manifest: false,
            decode: native,
            notes: "RTMP via FFmpeg/mpv".into(),
        },
        ProtocolSupport {
            scheme: StreamScheme::Rtmps,
            classify: true,
            manifest: false,
            decode: native,
            notes: "RTMPS".into(),
        },
        ProtocolSupport {
            scheme: StreamScheme::Rtsp,
            classify: true,
            manifest: false,
            decode: native,
            notes: "RTSP cameras / legacy IPTV".into(),
        },
        ProtocolSupport {
            scheme: StreamScheme::WebRtc,
            classify: true,
            manifest: false,
            decode: false,
            notes: "WebRTC — mobile ExoPlayer/AVPlayer or browser".into(),
        },
        ProtocolSupport {
            scheme: StreamScheme::Srt,
            classify: true,
            manifest: false,
            decode: native,
            notes: "SRT via mpv/FFmpeg libsrt".into(),
        },
        ProtocolSupport {
            scheme: StreamScheme::Udp,
            classify: true,
            manifest: false,
            decode: native,
            notes: "UDP/RTP multicast — mpv/ffmpeg".into(),
        },
        ProtocolSupport {
            scheme: StreamScheme::Rtp,
            classify: true,
            manifest: false,
            decode: native,
            notes: "RTP transport".into(),
        },
        ProtocolSupport {
            scheme: StreamScheme::Whip,
            classify: true,
            manifest: false,
            decode: false,
            notes: "WHIP ingest".into(),
        },
        ProtocolSupport {
            scheme: StreamScheme::File,
            classify: true,
            manifest: false,
            decode: native,
            notes: "Local media".into(),
        },
    ];
    trace!(entries = matrix.len(), native_decode = native, "support_matrix");
    matrix
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutedStream {
    pub url: StreamUrl,
    pub support: ProtocolSupport,
    pub open_externally: bool,
}

pub fn route(raw_url: &str) -> Result<RoutedStream> {
    let _prof = Stopwatch::start("route");
    let endpoint = url_endpoint(raw_url);
    debug!(%endpoint, "route start");
    let url = StreamUrl::parse(raw_url)?;
    let support = support_matrix()
        .into_iter()
        .find(|s| s.scheme == url.scheme)
        .unwrap_or(ProtocolSupport {
            scheme: url.scheme,
            classify: true,
            manifest: false,
            decode: false,
            notes: "Unrecognized".into(),
        });

    if !url.scheme.is_playback_ready() {
        warn!(
            scheme = %url.scheme.label(),
            %endpoint,
            "route unsupported scheme"
        );
        return Err(PlayerError::Unsupported(url.scheme.label().into()));
    }
    // Remote playlists must not force local file:// opens (path traversal / SSRF-ish).
    if url.scheme == StreamScheme::File {
        let allow = std::env::var("FLUXPLAY_ALLOW_FILE")
            .map(|v| v == "1")
            .unwrap_or(false);
        if !allow && !is_downloaded_file(raw_url) {
            return Err(PlayerError::Unsupported(
                "file:// refusé (hors du dossier de téléchargements; FLUXPLAY_ALLOW_FILE=1 pour forcer)"
                    .into(),
            ));
        }
    }

    let routed = RoutedStream {
        open_externally: !support.decode,
        url,
        support,
    };
    info!(
        scheme = %routed.url.scheme.label(),
        delivery = %routed.url.delivery.label(),
        host = ?routed.url.host,
        decode = routed.support.decode,
        open_externally = routed.open_externally,
        "route ok"
    );
    Ok(routed)
}

pub fn route_channel(channel: &Channel) -> Result<RoutedStream> {
    debug!(channel_id = %channel.id, channel_name = %channel.name, "route_channel");
    route(&channel.stream_url)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HlsProbe {
    pub is_master: bool,
    pub variant_count: usize,
    pub media_sequence: Option<u64>,
    pub bytes: usize,
}

/// Pure HLS/m3u8 body probe — no network. Safe for fuzz / property tests.
pub fn parse_hls_manifest_body(text: &str) -> HlsProbe {
    // Cap work on pathological multi-megabyte fuzz inputs.
    let slice = if text.len() > 512 * 1024 {
        &text[..512 * 1024]
    } else {
        text
    };
    let is_master = slice.contains("#EXT-X-STREAM-INF");
    let variants = slice
        .lines()
        .filter(|l| l.starts_with("#EXT-X-STREAM-INF"))
        .count();
    let media_seq = slice
        .lines()
        .find_map(|l| l.strip_prefix("#EXT-X-MEDIA-SEQUENCE:"))
        .and_then(|s| s.trim().parse().ok());
    HlsProbe {
        is_master,
        variant_count: variants,
        media_sequence: media_seq,
        bytes: text.len(),
    }
}

pub async fn probe_hls_manifest(url: &str) -> Result<HlsProbe> {
    let _prof = Stopwatch::start("probe_hls_manifest");
    let endpoint = url_endpoint(url);
    debug!(%endpoint, "probe_hls_manifest start");
    let parsed = StreamUrl::parse(url)?;
    if parsed.delivery != DeliveryKind::Hls && parsed.delivery != DeliveryKind::LlHls {
        warn!(
            delivery = %parsed.delivery.label(),
            %endpoint,
            "probe_hls_manifest not HLS"
        );
        return Err(PlayerError::Message("not an HLS URL".into()));
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let text = client.get(url).send().await?.error_for_status()?.text().await?;
    let probe = parse_hls_manifest_body(&text);
    info!(
        %endpoint,
        is_master = probe.is_master,
        variants = probe.variant_count,
        bytes = probe.bytes,
        "probe_hls_manifest ok"
    );
    Ok(probe)
}

/// Stable surface for libFuzzer harnesses (`--features fuzzing`).
#[cfg(feature = "fuzzing")]
pub mod fuzz_api {
    pub use crate::android_quality::{
        AndroidDeviceCaps, AndroidPresentMode, CompatTier, SoftBudget,
    };
    pub use crate::chapter_classify::{
        classify_chapter_title, credits_from_chapters, opening_from_chapters, ChapterClass,
    };
    pub use crate::parse_hls_manifest_body;
    pub use crate::route;
    pub use crate::support_matrix;
    pub use crate::video_frame::{yuv_to_rgb_coeffs, ColorMatrix, PixelLayout};
    pub use crate::HlsProbe;
}
