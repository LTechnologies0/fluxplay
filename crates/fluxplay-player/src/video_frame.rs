//! Decoded pictures handed to the UI video stage.
//!
//! The stage (a wgpu shader in the app) converts YUV → RGB and scales on the
//! GPU, so frames arrive at decoded size in their native layout. With a CUDA
//! decoder and a Vulkan stage on the same GPU, the planes never leave VRAM:
//! the app exports buffers ([`GpuStage::export_slots`]), FFmpeg copies each
//! surface into one of them, and the frame only names the slot.

use std::any::Any;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, RwLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelLayout {
    /// 8-bit RGBA, one plane.
    Rgba,
    /// 8-bit Y plane, then interleaved UV at half height (same pitch).
    Nv12,
    /// NV12 with 16-bit little-endian samples, 10 significant bits on top.
    P010,
}

impl PixelLayout {
    /// Rows of `pitch` bytes needed for a `height`-pixel picture.
    pub fn rows(self, height: u32) -> u32 {
        match self {
            PixelLayout::Rgba => height,
            PixelLayout::Nv12 | PixelLayout::P010 => height + height.div_ceil(2),
        }
    }

    /// Bytes per sample in the Y plane.
    pub fn bytes_per_sample(self) -> u32 {
        match self {
            PixelLayout::Rgba => 4,
            PixelLayout::Nv12 => 1,
            PixelLayout::P010 => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorMatrix {
    Bt601,
    Bt709,
    Bt2020,
}

pub struct VideoFrame {
    pub layout: PixelLayout,
    pub width: u32,
    pub height: u32,
    /// Bytes per row, shared by every plane.
    pub pitch: u32,
    /// Sample aspect ratio (1:1 for square pixels).
    pub sar: (u32, u32),
    pub matrix: ColorMatrix,
    /// Full-range (JPEG) YUV instead of video range.
    pub full_range: bool,
    pub data: FrameData,
}

pub enum FrameData {
    /// Planes in system memory, `pitch * layout.rows(height)` bytes.
    Cpu(Vec<u8>),
    /// Planes already in a buffer exported by the stage.
    Gpu(GpuFrame),
}

impl VideoFrame {
    /// Width shown on screen once the sample aspect ratio is applied.
    pub fn display_width(&self) -> f32 {
        let (n, d) = self.sar;
        self.width as f32 * n.max(1) as f32 / d.max(1) as f32
    }

    /// RGBA pixels at decoded size for CPU frames (screenshots); `None` for
    /// GPU frames.
    pub fn to_rgba(&self) -> Option<(u32, u32, Vec<u8>)> {
        let FrameData::Cpu(bytes) = &self.data else {
            return None;
        };
        let (w, h, pitch) = (self.width as usize, self.height as usize, self.pitch as usize);
        if bytes.len() < pitch * self.layout.rows(self.height) as usize {
            return None;
        }
        let mut out = vec![0u8; w * h * 4];
        match self.layout {
            PixelLayout::Rgba => {
                for y in 0..h {
                    out[y * w * 4..][..w * 4].copy_from_slice(&bytes[y * pitch..][..w * 4]);
                }
            }
            PixelLayout::Nv12 | PixelLayout::P010 => {
                let deep = self.layout == PixelLayout::P010;
                let sample = |off: usize| -> f32 {
                    if deep {
                        u16::from_le_bytes([bytes[off], bytes[off + 1]]) as f32 / 65535.0
                    } else {
                        bytes[off] as f32 / 255.0
                    }
                };
                let bps = if deep { 2 } else { 1 };
                let uv_base = pitch * h;
                let m = yuv_to_rgb_coeffs(self.matrix);
                for y in 0..h {
                    for x in 0..w {
                        let luma = sample(y * pitch + x * bps);
                        let c = uv_base + (y / 2) * pitch + (x / 2) * 2 * bps;
                        let (cb, cr) = (sample(c), sample(c + bps));
                        let (yn, u, v) = normalize_yuv(luma, cb, cr, self.full_range);
                        let px = &mut out[(y * w + x) * 4..][..4];
                        px[0] = to_u8(yn + m[0] * v);
                        px[1] = to_u8(yn + m[1] * u + m[2] * v);
                        px[2] = to_u8(yn + m[3] * u);
                        px[3] = 255;
                    }
                }
            }
        }
        Some((self.width, self.height, out))
    }
}

/// Coefficients `[r_v, g_u, g_v, b_u]` for R = Y + r_v·V, G = Y + g_u·U + g_v·V,
/// B = Y + b_u·U. Shared with the stage shader so both produce the same colours.
pub fn yuv_to_rgb_coeffs(matrix: ColorMatrix) -> [f32; 4] {
    let (kr, kb) = match matrix {
        ColorMatrix::Bt601 => (0.299, 0.114),
        ColorMatrix::Bt709 => (0.2126, 0.0722),
        ColorMatrix::Bt2020 => (0.2627, 0.0593),
    };
    let kg = 1.0 - kr - kb;
    [
        2.0 * (1.0 - kr),
        -2.0 * kb * (1.0 - kb) / kg,
        -2.0 * kr * (1.0 - kr) / kg,
        2.0 * (1.0 - kb),
    ]
}

/// Y in 0..1 and centred chroma in -0.5..0.5 from normalized samples.
fn normalize_yuv(y: f32, u: f32, v: f32, full_range: bool) -> (f32, f32, f32) {
    if full_range {
        (y, u - 0.5, v - 0.5)
    } else {
        (
            (y - 16.0 / 255.0) * (255.0 / 219.0),
            (u - 128.0 / 255.0) * (255.0 / 224.0),
            (v - 128.0 / 255.0) * (255.0 / 224.0),
        )
    }
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// A frame whose planes sit in slot `slot` of a stage-exported buffer set.
pub struct GpuFrame {
    /// What [`GpuStage::export_slots`] returned as `slots`.
    pub slots: Arc<dyn Any + Send + Sync>,
    pub slot: u32,
    _lease: SlotLease,
}

impl GpuFrame {
    pub(crate) fn new(slots: Arc<dyn Any + Send + Sync>, slot: u32, release: Arc<SlotRelease>) -> Self {
        Self {
            slots,
            slot,
            _lease: SlotLease { release, slot },
        }
    }
}

/// Decode may overwrite the slot once this is dropped: keep the frame (or the
/// whole [`VideoFrame`]) alive until the GPU has finished reading the slot.
struct SlotLease {
    release: Arc<SlotRelease>,
    slot: u32,
}

impl Drop for SlotLease {
    fn drop(&mut self) {
        self.release.released.fetch_or(1 << self.slot, Ordering::AcqRel);
    }
}

/// Slots handed back by the UI, forwarded to the decoder by the pump.
#[derive(Default)]
pub(crate) struct SlotRelease {
    released: AtomicU32,
}

impl SlotRelease {
    pub(crate) fn take(&self) -> u32 {
        self.released.swap(0, Ordering::AcqRel)
    }
}

/// Buffers exported by the stage for zero-copy frames.
#[cfg(unix)]
pub struct ExportedSlots {
    /// Stage-side handle (buffers); comes back in every [`GpuFrame`].
    pub slots: Arc<dyn Any + Send + Sync>,
    /// One `VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD` per slot.
    pub fds: Vec<std::os::fd::OwnedFd>,
    /// Size of each exported allocation.
    pub slot_bytes: u64,
    /// Row pitch the stage reads the planes with.
    pub pitch: u32,
    /// `VkPhysicalDeviceIDProperties::deviceUUID` of the stage device.
    pub device_uuid: [u8; 16],
}

/// The app's video stage, installed once its GPU pipeline exists.
pub trait GpuStage: Send + Sync {
    /// `count` buffers able to hold `layout` pictures of `width`×`height`,
    /// exportable to CUDA. `None` when the device cannot export memory.
    #[cfg(unix)]
    fn export_slots(
        &self,
        count: usize,
        layout: PixelLayout,
        width: u32,
        height: u32,
    ) -> Option<ExportedSlots>;
}

static STAGE: RwLock<Option<Arc<dyn GpuStage>>> = RwLock::new(None);
static STAGE_READY: AtomicBool = AtomicBool::new(false);

/// Called by the app when its GPU video stage can draw [`VideoFrame`]s. From
/// then on embedded players deliver native YUV instead of scaled RGBA.
pub fn install_video_stage(stage: Arc<dyn GpuStage>) {
    if let Ok(mut s) = STAGE.write() {
        *s = Some(stage);
    }
    STAGE_READY.store(true, Ordering::Release);
}

/// True once [`install_video_stage`] ran.
pub fn video_stage_ready() -> bool {
    STAGE_READY.load(Ordering::Acquire)
}

pub(crate) fn video_stage() -> Option<Arc<dyn GpuStage>> {
    STAGE.read().ok()?.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nv12(w: u32, h: u32, y: u8, u: u8, v: u8) -> VideoFrame {
        let pitch = w;
        let mut bytes = vec![y; (pitch * h) as usize];
        for _ in 0..(h / 2) {
            for _ in 0..(w / 2) {
                bytes.push(u);
                bytes.push(v);
            }
        }
        VideoFrame {
            layout: PixelLayout::Nv12,
            width: w,
            height: h,
            pitch,
            sar: (1, 1),
            matrix: ColorMatrix::Bt709,
            full_range: false,
            data: FrameData::Cpu(bytes),
        }
    }

    #[test]
    fn video_range_grey_and_primaries() {
        let (_, _, px) = nv12(4, 2, 235, 128, 128).to_rgba().unwrap();
        assert_eq!(&px[..4], &[255, 255, 255, 255]);
        let (_, _, px) = nv12(4, 2, 16, 128, 128).to_rgba().unwrap();
        assert_eq!(&px[..4], &[0, 0, 0, 255]);
        // BT.709 pure red in video range: Y=63, Cb=102, Cr=240.
        let (_, _, px) = nv12(2, 2, 63, 102, 240).to_rgba().unwrap();
        assert!(px[0] >= 250 && px[1] <= 6 && px[2] <= 6, "{:?}", &px[..4]);
    }

    #[test]
    fn released_slots_reach_the_pump_once() {
        let release = Arc::new(SlotRelease::default());
        let slots: Arc<dyn Any + Send + Sync> = Arc::new(());
        drop(GpuFrame::new(Arc::clone(&slots), 3, Arc::clone(&release)));
        drop(GpuFrame::new(slots, 0, Arc::clone(&release)));
        assert_eq!(release.take(), 0b1001);
        assert_eq!(release.take(), 0);
    }
}
