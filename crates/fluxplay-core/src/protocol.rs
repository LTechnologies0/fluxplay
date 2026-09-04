use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::{Error, Result};

/// URL schemes commonly used for IPTV / contribution / broadcast IP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamScheme {
    Http,
    Https,
    Rtsp,
    Rtsps,
    Rtmp,
    Rtmps,
    Udp,
    Rtp,
    Srt,
    WebRtc,
    Whip,
    Ndi,
    S2110,
    S2022,
    Rist,
    Quic,
    File,
    Unknown,
}

impl StreamScheme {
    pub fn parse(raw: &str) -> Self {
        let scheme = raw.split(':').next().unwrap_or("").to_ascii_lowercase();
        match scheme.as_str() {
            "http" => Self::Http,
            "https" => Self::Https,
            "rtsp" => Self::Rtsp,
            "rtsps" => Self::Rtsps,
            "rtmp" => Self::Rtmp,
            "rtmps" => Self::Rtmps,
            "udp" => Self::Udp,
            "rtp" => Self::Rtp,
            "srt" => Self::Srt,
            "webrtc" => Self::WebRtc,
            "whip" | "whep" => Self::Whip,
            "ndi" => Self::Ndi,
            "s2110" => Self::S2110,
            "s2022" => Self::S2022,
            "rist" => Self::Rist,
            "quic" | "http3" => Self::Quic,
            "file" => Self::File,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Http => "HTTP",
            Self::Https => "HTTPS",
            Self::Rtsp => "RTSP",
            Self::Rtsps => "RTSPS",
            Self::Rtmp => "RTMP",
            Self::Rtmps => "RTMPS",
            Self::Udp => "UDP",
            Self::Rtp => "RTP",
            Self::Srt => "SRT",
            Self::WebRtc => "WebRTC",
            Self::Whip => "WHIP",
            Self::Ndi => "NDI",
            Self::S2110 => "ST 2110",
            Self::S2022 => "ST 2022",
            Self::Rist => "RIST",
            Self::Quic => "QUIC / HTTP3",
            Self::File => "Fichier",
            Self::Unknown => "Inconnu",
        }
    }

    pub fn default_port(self) -> Option<u16> {
        match self {
            Self::Http => Some(80),
            Self::Https | Self::Quic => Some(443),
            Self::Rtsp | Self::Rtsps => Some(554),
            Self::Rtmp | Self::Rtmps => Some(1935),
            Self::Srt => Some(4200),
            _ => None,
        }
    }

    pub fn is_playback_ready(self) -> bool {
        matches!(
            self,
            Self::Http
                | Self::Https
                | Self::Rtsp
                | Self::Rtsps
                | Self::Rtmp
                | Self::Rtmps
                | Self::Udp
                | Self::Rtp
                | Self::Srt
                | Self::WebRtc
                | Self::Quic
                | Self::File
        )
    }
}

/// Delivery protocols (viewer-facing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKind {
    Hls,
    LlHls,
    Dash,
    Cmaf,
    WebRtc,
    ProgressiveHttp,
    Unknown,
}

impl DeliveryKind {
    pub fn detect(url: &str) -> Self {
        let lower = url.to_ascii_lowercase();
        if lower.contains(".mpd") || lower.contains("dash") {
            Self::Dash
        } else if lower.contains("llhls") || lower.contains("lowlatency") {
            Self::LlHls
        } else if lower.contains(".m3u8") || lower.contains("hls") {
            Self::Hls
        } else if lower.contains("cmaf") {
            Self::Cmaf
        } else if lower.starts_with("webrtc://") || lower.contains("webrtc") {
            Self::WebRtc
        } else if lower.starts_with("http://") || lower.starts_with("https://") {
            Self::ProgressiveHttp
        } else {
            Self::Unknown
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Hls => "HLS",
            Self::LlHls => "LL-HLS",
            Self::Dash => "MPEG-DASH",
            Self::Cmaf => "CMAF",
            Self::WebRtc => "WebRTC",
            Self::ProgressiveHttp => "HTTP",
            Self::Unknown => "—",
        }
    }

    pub fn typical_latency_hint(self) -> &'static str {
        match self {
            Self::Hls => "15–30 s",
            Self::LlHls => "2–5 s",
            Self::Dash => "10–25 s",
            Self::Cmaf => "3–5 s",
            Self::WebRtc => "< 500 ms",
            Self::ProgressiveHttp => "variable",
            Self::Unknown => "—",
        }
    }
}

/// Ingest / contribution protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestKind {
    Rtmp,
    Srt,
    Rtsp,
    Whip,
    RtpUdp,
    Unknown,
}

/// Parsed stream locator with scheme + delivery hints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamUrl {
    pub raw: String,
    pub scheme: StreamScheme,
    pub delivery: DeliveryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

impl StreamUrl {
    pub fn parse(raw: &str) -> Result<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(Error::Validation("empty stream URL".into()));
        }

        let scheme = StreamScheme::parse(trimmed);
        let delivery = DeliveryKind::detect(trimmed);

        // url::Url does not know all IPTV schemes; normalize a few for host/port.
        let (host, port) = match scheme {
            StreamScheme::Http
            | StreamScheme::Https
            | StreamScheme::Rtsp
            | StreamScheme::Rtsps
            | StreamScheme::Rtmp
            | StreamScheme::Rtmps
            | StreamScheme::File => {
                let parsed = Url::parse(trimmed)?;
                (
                    parsed.host_str().map(str::to_string),
                    parsed.port().or_else(|| scheme.default_port()),
                )
            }
            StreamScheme::Udp | StreamScheme::Rtp | StreamScheme::Srt => {
                parse_host_port_loose(trimmed)
            }
            _ => (None, scheme.default_port()),
        };

        Ok(Self {
            raw: trimmed.to_string(),
            scheme,
            delivery,
            host,
            port,
        })
    }
}

fn parse_host_port_loose(raw: &str) -> (Option<String>, Option<u16>) {
    // udp://239.0.0.1:1234 or srt://host:4200?...
    let without_scheme = raw.split("://").nth(1).unwrap_or(raw);
    let authority = without_scheme.split(['/', '?']).next().unwrap_or("");
    if let Some((h, p)) = authority.rsplit_once(':') {
        let port = p.parse().ok();
        (Some(h.to_string()), port)
    } else if authority.is_empty() {
        (None, None)
    } else {
        (Some(authority.to_string()), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hls() {
        let u = StreamUrl::parse("https://cdn.example.com/live/ch/master.m3u8").unwrap();
        assert_eq!(u.scheme, StreamScheme::Https);
        assert_eq!(u.delivery, DeliveryKind::Hls);
    }

    #[test]
    fn parses_udp_multicast() {
        let u = StreamUrl::parse("udp://239.0.0.1:1234").unwrap();
        assert_eq!(u.scheme, StreamScheme::Udp);
        assert_eq!(u.host.as_deref(), Some("239.0.0.1"));
        assert_eq!(u.port, Some(1234));
    }

    #[test]
    fn parses_srt() {
        let u = StreamUrl::parse("srt://192.168.1.100:4200?mode=listener").unwrap();
        assert_eq!(u.scheme, StreamScheme::Srt);
        assert_eq!(u.port, Some(4200));
    }
}
