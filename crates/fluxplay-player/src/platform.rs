//! Platform / target profiles for FluxPlay (desktop + mobile).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Linux,
    MacOs,
    Windows,
    Android,
    Ios,
    Unknown,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "android") {
            Self::Android
        } else if cfg!(target_os = "ios") {
            Self::Ios
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Unknown
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Linux => "Linux",
            Self::MacOs => "macOS",
            Self::Windows => "Windows",
            Self::Android => "Android",
            Self::Ios => "iOS",
            Self::Unknown => "Unknown",
        }
    }

    pub fn is_mobile(self) -> bool {
        matches!(self, Self::Android | Self::Ios)
    }

    pub fn is_desktop(self) -> bool {
        matches!(self, Self::Linux | Self::MacOs | Self::Windows)
    }
}

/// Recommended decode stack per platform (Kodi / IPTVnator inspired).
#[derive(Debug, Clone)]
pub struct TargetProfile {
    pub platform: Platform,
    pub ui_shell: &'static str,
    pub preferred_backends: &'static [&'static str],
    pub hw_accel: &'static str,
    pub notes: &'static str,
}

pub fn target_profile() -> TargetProfile {
    match Platform::current() {
        Platform::Linux => TargetProfile {
            platform: Platform::Linux,
            ui_shell: "iced (desktop)",
            preferred_backends: &["mpv", "ffmpeg", "external"],
            hw_accel: "vaapi / vulkan / cuda",
            notes: "mpv embeds FFmpeg; VA-API for HW decode",
        },
        Platform::MacOs => TargetProfile {
            platform: Platform::MacOs,
            ui_shell: "iced (desktop)",
            preferred_backends: &["mpv", "ffmpeg", "external"],
            hw_accel: "videotoolbox",
            notes: "mpv + VideoToolbox; IINA/mpv as external fallback",
        },
        Platform::Windows => TargetProfile {
            platform: Platform::Windows,
            ui_shell: "iced (desktop)",
            preferred_backends: &["mpv", "ffmpeg", "external"],
            hw_accel: "d3d11va / dxva2",
            notes: "mpv + D3D11VA; bundle libmpv in installer",
        },
        Platform::Android => TargetProfile {
            platform: Platform::Android,
            ui_shell: "Kotlin/Jetpack + fluxplay-ffi",
            preferred_backends: &["exoplayer", "mediacodec"],
            hw_accel: "MediaCodec",
            notes: "ExoPlayer (Media3) for HLS/DASH; Rust core via UniFFI/JNI",
        },
        Platform::Ios => TargetProfile {
            platform: Platform::Ios,
            ui_shell: "SwiftUI + fluxplay-ffi",
            preferred_backends: &["avplayer"],
            hw_accel: "VideoToolbox",
            notes: "AVPlayer; Rust core via UniFFI",
        },
        Platform::Unknown => TargetProfile {
            platform: Platform::Unknown,
            ui_shell: "unknown",
            preferred_backends: &["external"],
            hw_accel: "none",
            notes: "fallback open URL",
        },
    }
}
