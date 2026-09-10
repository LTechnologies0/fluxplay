//! FluxPlay player — protocol routing + native backends (libmpv / FFmpeg / platform).
//!
//! Desktop: **libmpv** and **libav* FFmpeg** in-process (RGBA into iced), optional CLI fallback.
//! Android iced (`fluxplay-android`): vendored **libmpv** RGBA embed + `ACTION_VIEW` Intent.
//! iOS / legacy FFI: AVPlayer (experimental).

mod android_quality;
mod backend;
#[cfg(all(feature = "native-ffmpeg", fluxplay_has_ffmpeg))]
mod ffmpeg_ffi;
#[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
mod mpv_ffi;
#[cfg(all(feature = "native-mpv", fluxplay_has_libmpv))]
mod soft_pump;
mod native_log;
mod platform;
mod session;

use fluxplay_core::protocol::{DeliveryKind, StreamScheme, StreamUrl};
use fluxplay_core::Channel;
use fluxplay_core::Stopwatch;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, info, trace, warn};

pub use android_quality::{AndroidDeviceCaps, AndroidPresentMode, CompatTier, SoftBudget};
pub use backend::{
    detect_backends, BackendCaps, BackendId, BackendInfo, NativePlayer, PlayOptions, PlayerEvent,
    VideoRect,
};
pub use native_log::{
    ffmpeg_av_log_level, log_native_verbosity_banner, mpv_msg_level, mpv_verbose_log_path,
    verbose_master,
};
pub use platform::{target_profile, Platform, TargetProfile};
pub use session::{
    AspectMode, AudioChannelMode, Bookmark, DeinterlaceMode, EqPreset, PlaybackState,
    StreamSession, UpscaleMode,
};

#[derive(Debug, Error)]
pub enum PlayerError {
    #[error("unsupported protocol: {0}")]
    Unsupported(String),
    #[error("invalid url: {0}")]
    InvalidUrl(String),
    #[error("backend: {0}")]
    Backend(String),
    #[error("core: {0}")]
    Core(#[from] fluxplay_core::Error),
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, PlayerError>;

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
        raw.split(['?', '#']).next().unwrap_or(raw).to_string()
    }
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
        if !allow {
            let path = raw_url
                .strip_prefix("file://")
                .or_else(|| raw_url.strip_prefix("file:"))
                .unwrap_or(raw_url);
            let p = std::path::Path::new(path);
            if !p.is_file() {
                return Err(PlayerError::Unsupported(
                    "file:// refusé (fichier local introuvable; FLUXPLAY_ALLOW_FILE=1 pour forcer)"
                        .into(),
                ));
            }
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
    let is_master = text.contains("#EXT-X-STREAM-INF");
    let variants = text
        .lines()
        .filter(|l| l.starts_with("#EXT-X-STREAM-INF"))
        .count();
    let media_seq = text
        .lines()
        .find_map(|l| l.strip_prefix("#EXT-X-MEDIA-SEQUENCE:"))
        .and_then(|s| s.trim().parse().ok());
    let probe = HlsProbe {
        is_master,
        variant_count: variants,
        media_sequence: media_seq,
        bytes: text.len(),
    };
    info!(
        %endpoint,
        is_master = probe.is_master,
        variants = probe.variant_count,
        bytes = probe.bytes,
        "probe_hls_manifest ok"
    );
    Ok(probe)
}
