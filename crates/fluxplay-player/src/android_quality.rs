//! Android present-path + **dynamic** quality matrix (Phases A–D).
//!
//! Single source of truth for Soft WH/Hz and present selection. Callers must not
//! re-hardcode core→720p buckets — use [`AndroidDeviceCaps::soft_budget`].

use serde::{Deserialize, Serialize};

/// How libmpv presents video on Android.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AndroidPresentMode {
    /// CPU-readable frames into iced (safe everywhere, dynamically capped).
    #[default]
    SoftRgba,
    /// MediaCodec → SurfaceView (preferred whenever HW decode exists).
    SurfaceEmbed,
    /// `vo=gpu` + egl-android on the same Surface (Phase D without Vulkan rebuild).
    GpuEgl,
}

impl AndroidPresentMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::SoftRgba => "soft-rgba",
            Self::SurfaceEmbed => "mediacodec-embed",
            Self::GpuEgl => "gpu-egl",
        }
    }

    pub fn uses_surface(self) -> bool {
        matches!(self, Self::SurfaceEmbed | Self::GpuEgl)
    }

    pub fn uses_soft_rgba(self) -> bool {
        matches!(self, Self::SoftRgba)
    }
}

/// Soft RGBA budget derived from live device caps (not fixed core buckets alone).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoftBudget {
    pub max_w: u32,
    pub max_h: u32,
    pub video_hz: u32,
    pub gui_hz: u32,
}

impl SoftBudget {
    /// Soft downscale that preserves aspect (never stretch into a fixed WxH box).
    ///
    /// Avoid `force_original_aspect_ratio` — media-kit libmpv on Android rejects it
    /// (`error setting option` / unsupported property). Prefer `W:-2` / `-2:H`.
    pub fn vf_scale(self) -> String {
        let w = self.max_w.max(2) & !1;
        let h = self.max_h.max(2) & !1;
        // media-kit rejects `flags=` / `format=` on Android — keep the filter minimal.
        if w >= h {
            format!("scale={w}:-2")
        } else {
            format!("scale=-2:{h}")
        }
    }
}

/// Compatibility tier 0=low … 3=ultra (derived, not a fixed SoC allowlist alone).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompatTier {
    Low = 0,
    Mid = 1,
    High = 2,
    Ultra = 3,
}

/// Display / SoC snapshot written by Java (`device_caps.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AndroidDeviceCaps {
    #[serde(default)]
    pub soc: String,
    #[serde(default)]
    pub board: String,
    #[serde(default)]
    pub hardware: String,
    #[serde(default)]
    pub manufacturer: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub gl_renderer: String,
    #[serde(default)]
    pub cores: u32,
    /// Peak panel refresh (Hz).
    #[serde(default)]
    pub refresh_hz: u32,
    /// Supported mode refresh rates from Display (dynamic snap list).
    #[serde(default)]
    pub refresh_modes: Vec<f32>,
    /// Supported HDR type ints from [`Display.HdrCapabilities`].
    #[serde(default)]
    pub hdr_types: Vec<i32>,
    #[serde(default)]
    pub hdr_capable: bool,
    #[serde(default)]
    pub mediacodec_4k: bool,
    #[serde(default)]
    pub mediacodec_hdr: bool,
    /// Any hardware video decoder present (H264/HEVC/VP9/AV1…).
    #[serde(default)]
    pub mediacodec_video: bool,
    /// Max MediaCodec decode width/height observed across codecs.
    #[serde(default)]
    pub mediacodec_max_w: u32,
    #[serde(default)]
    pub mediacodec_max_h: u32,
    /// `Build.VERSION.SDK_INT`.
    #[serde(default)]
    pub sdk_int: u32,
    /// Heuristic OLED/AMOLED panel (Pixel, Galaxy, OnePlus…).
    #[serde(default)]
    pub panel_oled: bool,
    /// Human labels for hdr_types (HDR10, HLG, HDR10+, Dolby Vision…).
    #[serde(default)]
    pub hdr_labels: Vec<String>,
    #[serde(default)]
    pub surface_ready: bool,
    #[serde(default)]
    pub surface_w: u32,
    #[serde(default)]
    pub surface_h: u32,
}

impl AndroidDeviceCaps {
    /// Name-based flagship hint (secondary to [`Self::compat_tier`]).
    pub fn is_flagship(&self) -> bool {
        self.compat_tier() >= CompatTier::High
    }

    pub fn compat_tier(&self) -> CompatTier {
        let blob = format!(
            "{} {} {} {} {}",
            self.soc, self.hardware, self.board, self.model, self.gl_renderer
        )
        .to_ascii_lowercase();
        let named_ultra = blob.contains("tensor")
            || blob.contains("sm85")
            || blob.contains("sm86")
            || blob.contains("sm87")
            || blob.contains("sm88")
            || blob.contains("sm89")
            || blob.contains("kalama")
            || blob.contains("pineapple")
            || blob.contains("lahaina")
            || blob.contains("taro")
            || blob.contains("waipio")
            || blob.contains("sm8")
            || blob.contains("dimensity 9")
            || blob.contains("mt69")
            || blob.contains("exynos2")
            || blob.contains("s5e99")
            || blob.contains("mali-g7")
            || blob.contains("adreno 7");
        let named_high = named_ultra
            || blob.contains("dimensity")
            || blob.contains("mt68")
            || blob.contains("tegra")
            || blob.contains("adreno 6");

        if self.mediacodec_4k && (self.cores >= 8 || named_ultra) {
            CompatTier::Ultra
        } else if self.mediacodec_4k || named_high || self.cores >= 8 {
            CompatTier::High
        } else if self.mediacodec_video || self.cores >= 6 {
            CompatTier::Mid
        } else {
            CompatTier::Low
        }
    }

    /// Soft RGBA max size — **fallback only** (Surface is HQ path). Cap ≤720p by
    /// default so demotion stays smooth; `FLUXPLAY_SOFT_UHD` raises to 1080p.
    pub fn max_soft_wh(&self) -> (u32, u32) {
        let uhd = std::env::var_os("FLUXPLAY_SOFT_UHD").is_some();
        let (mut w, mut h) = match self.compat_tier() {
            CompatTier::Ultra | CompatTier::High => {
                if uhd {
                    (1920, 1080)
                } else {
                    (1280, 720)
                }
            }
            CompatTier::Mid => (960, 540),
            CompatTier::Low => (854, 480),
        };
        // Never request soft present larger than HW can decode (when known).
        // Fit uniformly into the MediaCodec max box (don't squash aspect).
        if self.mediacodec_max_w >= 64 && self.mediacodec_max_h >= 64 {
            let sx = self.mediacodec_max_w as f32 / w as f32;
            let sy = self.mediacodec_max_h as f32 / h as f32;
            let s = sx.min(sy).min(1.0);
            w = ((w as f32 * s).round() as u32).max(2);
            h = ((h as f32 * s).round() as u32).max(2);
        }
        // Prefer even dims for SW paths.
        (w.max(2) & !1, h.max(2) & !1)
    }

    /// Soft budget with an explicit user quality rung (360p–4K). Soft upload caps at 1080p.
    /// Never exceeds the SoC auto soft budget (user quality is a ceiling, not a raise).
    pub fn soft_budget_with_quality(&self, quality: Option<(u32, u32)>) -> SoftBudget {
        let mut b = self.soft_budget();
        if let Some((qw, qh)) = quality {
            let mut w = qw.max(2) & !1;
            let mut h = qh.max(2) & !1;
            if self.mediacodec_max_w >= 64 && self.mediacodec_max_h >= 64 {
                let sx = self.mediacodec_max_w as f32 / w as f32;
                let sy = self.mediacodec_max_h as f32 / h as f32;
                let s = sx.min(sy).min(1.0);
                w = ((w as f32 * s).round() as u32).max(2);
                h = ((h as f32 * s).round() as u32).max(2);
            }
            // Soft RGBA into iced — fit into tier budget ∩ soft upload box, keep aspect.
            let upload_cap_w = if std::env::var_os("FLUXPLAY_SOFT_UHD").is_some() {
                1920u32
            } else {
                1280u32
            };
            let upload_cap_h = if std::env::var_os("FLUXPLAY_SOFT_UHD").is_some() {
                1080u32
            } else {
                720u32
            };
            let box_w = upload_cap_w.min(b.max_w).max(2);
            let box_h = upload_cap_h.min(b.max_h).max(2);
            let sx = box_w as f32 / w as f32;
            let sy = box_h as f32 / h as f32;
            let s = sx.min(sy).min(1.0);
            w = ((w as f32 * s).round() as u32).max(2) & !1;
            h = ((h as f32 * s).round() as u32).max(2) & !1;
            b.max_w = w;
            b.max_h = h;
        }
        b
    }

    pub fn has_hdr10_plus(&self) -> bool {
        self.hdr_types.contains(&4) // Display.HdrCapabilities.HDR_TYPE_HDR10_PLUS
            || self
                .hdr_labels
                .iter()
                .any(|l| l.to_ascii_lowercase().contains("hdr10+"))
    }

    pub fn has_dolby_vision(&self) -> bool {
        self.hdr_types.contains(&1)
            || self
                .hdr_labels
                .iter()
                .any(|l| l.to_ascii_lowercase().contains("dolby"))
    }

    pub fn soft_video_hz_cap(&self) -> u32 {
        let panel = self.refresh_hz.max(24);
        // Soft SW render + iced upload. The atlas-reuse patch (no per-frame texture
        // create + no blocking GPU wait) lets Ultra/High track 60 fps content.
        let base = match self.compat_tier() {
            CompatTier::Ultra => 60,
            CompatTier::High => 60,
            CompatTier::Mid => 30,
            CompatTier::Low => 24,
        };
        base.min(panel).min(60).max(20)
    }

    pub fn soft_gui_hz_cap(&self) -> u32 {
        // Chrome / mosaic can track the panel (120Hz on Pixel 8); soft video is capped separately.
        match self.compat_tier() {
            CompatTier::Ultra => 120,
            CompatTier::High => 90,
            CompatTier::Mid => 60,
            CompatTier::Low => 30,
        }
        .min(self.refresh_hz.max(30))
        .min(120)
    }

    /// Unified soft budget for vf + iced present + display_caps.
    pub fn soft_budget(&self) -> SoftBudget {
        let (max_w, max_h) = self.max_soft_wh();
        SoftBudget {
            max_w,
            max_h,
            video_hz: self.soft_video_hz_cap(),
            gui_hz: self.soft_gui_hz_cap(),
        }
    }

    /// Present ladder: try highest quality first; caller falls back on bind failure.
    pub fn present_ladder(&self) -> Vec<AndroidPresentMode> {
        if let Ok(v) = std::env::var("FLUXPLAY_ANDROID_PRESENT") {
            match v.to_ascii_lowercase().as_str() {
                "soft" | "rgba" | "libmpv" => return vec![AndroidPresentMode::SoftRgba],
                "surface" | "embed" | "mediacodec_embed" => {
                    return vec![
                        AndroidPresentMode::SurfaceEmbed,
                        AndroidPresentMode::SoftRgba,
                    ];
                }
                "gpu" | "egl" | "gpu-egl" => {
                    return vec![
                        AndroidPresentMode::GpuEgl,
                        AndroidPresentMode::SurfaceEmbed,
                        AndroidPresentMode::SoftRgba,
                    ];
                }
                _ => {}
            }
        }
        if let Ok(v) = std::env::var("FLUXPLAY_ANDROID_VO") {
            match v.to_ascii_lowercase().as_str() {
                "gpu" | "gpu-next" => {
                    return vec![
                        AndroidPresentMode::GpuEgl,
                        AndroidPresentMode::SurfaceEmbed,
                        AndroidPresentMode::SoftRgba,
                    ];
                }
                "libmpv" | "sw" | "soft" => return vec![AndroidPresentMode::SoftRgba],
                "mediacodec_embed" | "embed" => {
                    return vec![
                        AndroidPresentMode::SurfaceEmbed,
                        AndroidPresentMode::SoftRgba,
                    ];
                }
                _ => {}
            }
        }

        let mut ladder = Vec::with_capacity(3);
        // SurfaceView + mediacodec_embed = zero-copy HQ (mpv-android). Soft ≤720p fallback.
        // Force soft-only: FLUXPLAY_ANDROID_PRESENT=soft.
        let want_surface = self.mediacodec_video
            || self.mediacodec_4k
            || self.hdr_capable
            || self.compat_tier() >= CompatTier::Mid
            || self.cores >= 4;
        if want_surface {
            ladder.push(AndroidPresentMode::SurfaceEmbed);
        }
        ladder.push(AndroidPresentMode::SoftRgba);
        ladder
    }

    /// Preferred present mode (first step of [`Self::present_ladder`]).
    pub fn select_present_mode(&self) -> AndroidPresentMode {
        self.present_ladder()
            .into_iter()
            .next()
            .unwrap_or(AndroidPresentMode::SoftRgba)
    }

    /// Snap content fps to a panel-friendly rate using live modes when available.
    pub fn snap_present_hz(content_fps: f32, panel_hz: f32) -> f32 {
        Self::snap_present_hz_with_modes(content_fps, panel_hz, &[])
    }

    pub fn snap_present_hz_with_modes(content_fps: f32, panel_hz: f32, modes: &[f32]) -> f32 {
        let panel = if panel_hz.is_finite() && panel_hz >= 24.0 {
            panel_hz
        } else {
            60.0
        };
        let content = if content_fps.is_finite() && content_fps >= 1.0 {
            content_fps
        } else {
            // Unknown content: prefer 60 or panel, not always peak (saves power).
            return panel.min(60.0).max(24.0);
        };

        let mut candidates: Vec<f32> = modes
            .iter()
            .copied()
            .filter(|h| h.is_finite() && *h >= 20.0)
            .collect();
        if candidates.is_empty() {
            candidates.extend_from_slice(&[
                24.0, 25.0, 30.0, 48.0, 50.0, 60.0, 90.0, 120.0, 144.0, 165.0, 240.0,
            ]);
        } else {
            // Always consider common film/TV rates even if OEM omitted them.
            for c in [24.0, 25.0, 30.0, 50.0, 60.0] {
                if !candidates
                    .iter()
                    .any(|h| (*h - c).abs() < 0.5)
                {
                    candidates.push(c);
                }
            }
        }

        let mut best = panel.min(60.0);
        let mut best_err = f32::MAX;
        for c in candidates {
            if c > panel + 0.75 {
                continue;
            }
            let n = (c / content).round().max(1.0);
            let err = (c - content * n).abs() + (c - content).abs() * 0.05;
            if err < best_err {
                best_err = err;
                best = c;
            }
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_budget_ultra_tensor_g3_720p_fallback() {
        let caps = AndroidDeviceCaps {
            cores: 9,
            mediacodec_4k: true,
            mediacodec_video: true,
            soc: "Tensor G3".into(),
            refresh_hz: 120,
            ..Default::default()
        };
        assert!(caps.compat_tier() >= CompatTier::Ultra);
        let b = caps.soft_budget();
        assert_eq!(b.max_w, 1280);
        assert_eq!(b.max_h, 720);
        assert!(b.video_hz >= 30 && b.video_hz <= 48);
        assert!(b.gui_hz >= 90);
    }

    #[test]
    fn present_ladder_ultra_prefers_surface() {
        let caps = AndroidDeviceCaps {
            cores: 9,
            mediacodec_4k: true,
            mediacodec_video: true,
            soc: "Tensor G3".into(),
            refresh_hz: 120,
            ..Default::default()
        };
        assert_eq!(
            caps.select_present_mode(),
            AndroidPresentMode::SurfaceEmbed
        );
    }

    #[test]
    fn present_ladder_prefers_surface_when_mediacodec() {
        let caps = AndroidDeviceCaps {
            mediacodec_video: true,
            cores: 4,
            ..Default::default()
        };
        assert_eq!(
            caps.select_present_mode(),
            AndroidPresentMode::SurfaceEmbed
        );
    }

    #[test]
    fn soft_budget_with_quality_never_raises_tier() {
        let caps = AndroidDeviceCaps {
            cores: 4,
            mediacodec_video: true,
            refresh_hz: 60,
            ..Default::default()
        };
        let auto = caps.soft_budget();
        let forced = caps.soft_budget_with_quality(Some((3840, 2160)));
        assert!(forced.max_w <= auto.max_w);
        assert!(forced.max_h <= auto.max_h);
        assert!(forced.max_w <= auto.max_w);
        assert!(forced.max_h <= auto.max_h);
    }
}
