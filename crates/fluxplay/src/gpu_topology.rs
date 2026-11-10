//! Multi-GPU topology: display GPU owns present caps; decode may differ only via copy.
//!
//! ## Policy (evidence-based)
//! - **Zero-copy** (`vaapi`/`mediacodec`/`vulkan` interop): decode GPU **must** be the
//!   same as the GPU that owns the window/Surface. mpv/Firefox confirm this.
//! - **Copy** (`*-copy`, soft RGBA): decode can run on the stronger GPU, but soft
//!   present / composite budgets follow the **weaker display** GPU.
//! - Android phones rarely expose two GPUs; MediaCodec ≠ a second GL device.
//!
//! ## Env split (W4-08)
//! - `FLUXPLAY_GPU` — iced/wgpu **present** adapter (display / panel only).
//! - `FLUXPLAY_DECODE_GPU` — mpv/ffmpeg vendor steer (`cuda-copy` / VAAPI / D3D11).
//! - `FLUXPLAY_GPU_NODE` — Linux DRM `/dev/dri/renderD*` for libva only (never DXGI).

use fluxplay_core::models::GpuTier;
use serde::{Deserialize, Serialize};
// Only `topology_from_devices` logs; it is desktop/test-only.
#[cfg(any(test, not(target_os = "android")))]
use tracing::{debug, info};

/// One enumerated graphics / video device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuDevice {
    pub name: String,
    pub tier: GpuTier,
    /// Linux: DRM render node (`/dev/dri/renderD128`). Windows: full DXGI/GDI
    /// adapter description for `d3d11-adapter` (UI `name` may be truncated).
    #[serde(default)]
    pub render_node: Option<String>,
}

impl GpuDevice {
    pub fn label(&self) -> String {
        match &self.render_node {
            Some(n) => format!("{} ({}, {})", self.name, self.tier.label(), n),
            None => format!("{} ({})", self.name, self.tier.label()),
        }
    }
}

/// How decode relates to the display GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GpuDecodePolicy {
    /// Decode on display GPU (zero-copy safe).
    #[default]
    SameAsDisplay,
    /// Decode on discrete / stronger GPU — **requires** hwdec `*-copy` or soft RGBA.
    StrongDecodeCopy,
}

/// Detected GPUs + roles for FluxPlay budgets / mpv device selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuTopology {
    pub devices: Vec<GpuDevice>,
    /// GPU that drives the panel / iced surface — **authority for FPS/res caps**.
    pub display: GpuDevice,
    /// Preferred HW decode device (may equal `display`).
    pub decode: GpuDevice,
    pub policy: GpuDecodePolicy,
}

impl GpuTopology {
    pub fn single(name: impl Into<String>, tier: GpuTier) -> Self {
        let d = GpuDevice {
            name: name.into(),
            tier,
            render_node: None,
        };
        Self {
            devices: vec![d.clone()],
            display: d.clone(),
            decode: d,
            policy: GpuDecodePolicy::SameAsDisplay,
        }
    }

    #[cfg(test)]
    pub fn is_hybrid(&self) -> bool {
        self.devices.len() > 1
            && self.devices.iter().any(|d| d.tier == GpuTier::Discrete)
            && self.devices.iter().any(|d| d.tier == GpuTier::Integrated)
    }

    /// Soft present / GUI budgets must follow the **display** GPU (often the weaker one).
    pub fn display_tier(&self) -> GpuTier {
        self.display.tier
    }

    /// True when decode≠display → force `vaapi-copy` / `mediacodec-copy` / soft.
    pub fn requires_copy_path(&self) -> bool {
        // Two identical cards still sit on different render nodes: either difference
        // means frames cross devices.
        self.policy == GpuDecodePolicy::StrongDecodeCopy
            || self.decode.render_node != self.display.render_node
            || self.decode.name != self.display.name
    }

    pub fn summary(&self) -> String {
        if self.devices.len() <= 1 {
            return format!("display/decode={}", self.display.label());
        }
        format!(
            "hybrid display={} · decode={} · {}",
            self.display.label(),
            self.decode.label(),
            match self.policy {
                GpuDecodePolicy::SameAsDisplay => "zero-copy-ok",
                GpuDecodePolicy::StrongDecodeCopy => "copy-required",
            }
        )
    }
}

/// Build topology from an ordered list (best discrete first preferred for decode).
/// Called by the Linux `lspci` probe and unit tests; Android uses `GpuTopology::single`.
#[cfg(any(test, not(target_os = "android")))]
pub fn topology_from_devices(mut devices: Vec<GpuDevice>) -> GpuTopology {
    if devices.is_empty() {
        return GpuTopology::single("GPU", GpuTier::Unknown);
    }
    // Stable: discrete first for ranking, then integrated, then unknown.
    devices.sort_by_key(|d| match d.tier {
        GpuTier::Discrete => 0u8,
        GpuTier::Integrated => 1,
        GpuTier::Unknown => 2,
    });
    let discrete = devices
        .iter()
        .find(|d| d.tier == GpuTier::Discrete)
        .cloned();
    let integrated = devices
        .iter()
        .find(|d| d.tier == GpuTier::Integrated)
        .cloned();

    // Display: prefer iGPU on hybrid laptops (panel usually wired there). If only
    // discrete (desktop), display = discrete.
    let display = integrated
        .clone()
        .or_else(|| discrete.clone())
        .unwrap_or_else(|| devices[0].clone());

    // Env override.
    if let Ok(v) = std::env::var("FLUXPLAY_GPU_POLICY") {
        match v.to_ascii_lowercase().as_str() {
            "same" | "display" | "zero-copy" => {
                return GpuTopology {
                    devices,
                    decode: display.clone(),
                    display,
                    policy: GpuDecodePolicy::SameAsDisplay,
                };
            }
            "strong" | "discrete" | "copy" => {
                let decode = discrete.clone().unwrap_or_else(|| display.clone());
                let policy = if decode.name != display.name {
                    GpuDecodePolicy::StrongDecodeCopy
                } else {
                    GpuDecodePolicy::SameAsDisplay
                };
                return GpuTopology {
                    devices,
                    display,
                    decode,
                    policy,
                };
            }
            _ => {}
        }
    }

    // One GPU does decode and display. The panel is wired to `display`
    // (the integrated GPU on a hybrid laptop). A second GPU is only used
    // when the user picks it in the settings.
    let _ = discrete;
    let decode = display.clone();
    let policy = GpuDecodePolicy::SameAsDisplay;

    let topo = GpuTopology {
        devices,
        display,
        decode,
        policy,
    };
    // resolve_caps may re-probe often — log the summary once per process.
    static LOGGED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    let _ = LOGGED.get_or_init(|| {
        info!(summary = %topo.summary(), "GPU topology");
    });
    debug!(?topo, "GPU topology detail");
    topo
}

/// Windows pin for mpv `--d3d11-adapter=` (DXGI description, prefix match).
/// Prefers `render_node` when it holds a non-DRM adapter string; never returns a
/// truncated UI name that contains an ellipsis (would fail to match).
pub fn d3d11_adapter_from_device(dev: &GpuDevice) -> Option<String> {
    if let Some(n) = dev.render_node.as_deref() {
        if !n.is_empty() && !n.starts_with("/dev/dri/") {
            return Some(n.to_owned());
        }
    }
    let n = dev.name.as_str();
    (!n.is_empty() && !n.contains('…')).then(|| dev.name.clone())
}

#[cfg(test)]
mod d3d11_pin_tests {
    use super::*;
    use fluxplay_core::models::GpuTier;

    #[test]
    fn prefers_full_render_node_over_truncated_name() {
        let dev = GpuDevice {
            name: "NVIDIA GeForce RTX 4090 Laptop GPU With A Very Long…".into(),
            tier: GpuTier::Discrete,
            render_node: Some("NVIDIA GeForce RTX 4090 Laptop GPU".into()),
        };
        assert_eq!(
            d3d11_adapter_from_device(&dev).as_deref(),
            Some("NVIDIA GeForce RTX 4090 Laptop GPU")
        );
    }

    #[test]
    fn rejects_ellipsis_only_name_without_render_node() {
        let dev = GpuDevice {
            name: "Some Extremely Long Adapter Name That Got…".into(),
            tier: GpuTier::Integrated,
            render_node: None,
        };
        assert_eq!(d3d11_adapter_from_device(&dev), None);
    }

    #[test]
    fn ignores_linux_drm_render_node() {
        let dev = GpuDevice {
            name: "Intel UHD Graphics".into(),
            tier: GpuTier::Integrated,
            render_node: Some("/dev/dri/renderD128".into()),
        };
        assert_eq!(
            d3d11_adapter_from_device(&dev).as_deref(),
            Some("Intel UHD Graphics")
        );
    }
}


#[cfg(test)]
mod topology_policy_tests {
    use super::*;
    use fluxplay_core::models::GpuTier;

    #[test]
    fn strong_decode_requires_copy() {
        let igpu = GpuDevice {
            name: "Intel".into(),
            tier: GpuTier::Integrated,
            render_node: Some("/dev/dri/renderD128".into()),
        };
        let dgpu = GpuDevice {
            name: "NVIDIA".into(),
            tier: GpuTier::Discrete,
            render_node: Some("/dev/dri/renderD129".into()),
        };
        let topo = GpuTopology {
            devices: vec![dgpu.clone(), igpu.clone()],
            display: igpu,
            decode: dgpu,
            policy: GpuDecodePolicy::StrongDecodeCopy,
        };
        assert!(topo.requires_copy_path());
    }

    #[test]
    fn same_display_no_copy() {
        let topo = GpuTopology::single("Intel", GpuTier::Integrated);
        assert!(!topo.requires_copy_path());
    }
}
