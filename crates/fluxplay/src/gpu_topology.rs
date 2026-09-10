//! Multi-GPU topology: display GPU owns present caps; decode may differ only via copy.
//!
//! ## Policy (evidence-based)
//! - **Zero-copy** (`vaapi`/`mediacodec`/`vulkan` interop): decode GPU **must** be the
//!   same as the GPU that owns the window/Surface. mpv/Firefox confirm this.
//! - **Copy** (`*-copy`, soft RGBA): decode can run on the stronger GPU, but soft
//!   present / composite budgets follow the **weaker display** GPU.
//! - Android phones rarely expose two GPUs; MediaCodec ≠ a second GL device.

use fluxplay_core::models::GpuTier;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// One enumerated graphics / video device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuDevice {
    pub name: String,
    pub tier: GpuTier,
    /// DRM render node when known (`/dev/dri/renderD128`).
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
        self.policy == GpuDecodePolicy::StrongDecodeCopy
            || self.decode.render_node != self.display.render_node
                && self.decode.name != self.display.name
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

    // Default hybrid policy matching the product rule:
    // - Cap quality to **display** (weaker / iGPU).
    // - Prefer **strong** decode only when we will use a copy path (soft / *-copy).
    //   For zero-copy Surface we keep SameAsDisplay (Android path sets that itself).
    let (decode, policy) = if let (Some(d), Some(_)) = (discrete, integrated) {
        // Soft RGBA / embed always copies through CPU on desktop → strong decode OK.
        (d, GpuDecodePolicy::StrongDecodeCopy)
    } else {
        (display.clone(), GpuDecodePolicy::SameAsDisplay)
    };

    let topo = GpuTopology {
        devices,
        display,
        decode,
        policy,
    };
    info!(summary = %topo.summary(), "GPU topology");
    debug!(?topo, "GPU topology detail");
    topo
}
