//! FluxPlay player — protocol routing + native backends (mpv / FFmpeg / platform).
//!
//! Desktop (Win/macOS/Linux): prefers **mpv** (embeds FFmpeg, HW accel) then **ffplay**.
//! Mobile: core logic + FFI; decode via ExoPlayer (Android) / AVPlayer (iOS).

mod backend;
mod platform;
mod session;

use fluxplay_core::protocol::{DeliveryKind, StreamScheme, StreamUrl};
use fluxplay_core::Channel;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use backend::{
    detect_backends, BackendId, BackendInfo, NativePlayer, PlayOptions, PlayerEvent,
};
pub use platform::{target_profile, Platform, TargetProfile};
pub use session::{PlaybackState, StreamSession};

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
    let decode_note = if native {
        "native decode via mpv/FFmpeg when available"
    } else {
        "install mpv or ffmpeg/ffplay for native decode"
    };
    vec![
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
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutedStream {
    pub url: StreamUrl,
    pub support: ProtocolSupport,
    pub open_externally: bool,
}

pub fn route(raw_url: &str) -> Result<RoutedStream> {
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

    if !url.scheme.is_playback_ready() && url.scheme != StreamScheme::Unknown {
        return Err(PlayerError::Unsupported(url.scheme.label().into()));
    }

    Ok(RoutedStream {
        open_externally: !support.decode,
        url,
        support,
    })
}

pub fn route_channel(channel: &Channel) -> Result<RoutedStream> {
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
    let parsed = StreamUrl::parse(url)?;
    if parsed.delivery != DeliveryKind::Hls && parsed.delivery != DeliveryKind::LlHls {
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
    Ok(HlsProbe {
        is_master,
        variant_count: variants,
        media_sequence: media_seq,
        bytes: text.len(),
    })
}
