//! GPU video stage: draws [`VideoFrame`]s with a wgpu shader inside iced.
//!
//! Frames arrive at decoded size in their native layout (NV12 / P010 / RGBA);
//! the shader converts to RGB, applies the sample aspect ratio, letterboxes
//! and resamples to the stage in one pass. CPU frames are uploaded with
//! `write_texture`; zero-copy frames (CUDA → exported Vulkan buffer) only
//! cost a VRAM-to-VRAM copy.
//!
//! The stage becomes "ready" the first time iced builds its pipeline — only
//! with the wgpu renderer, never with tiny-skia. Until then the app keeps the
//! RGBA image path.

use std::sync::{Arc, Mutex};

use fluxplay_player::{
    install_video_stage, yuv_to_rgb_coeffs, ColorMatrix, FrameData, GpuStage, PixelLayout,
    VideoFrame,
};
use iced::widget::shader::{self, Viewport};
use iced::wgpu;
use iced::{mouse, Element, Length, Rectangle};

/// Frames waiting for the render thread, and uploaded ones going back to the
/// player for buffer reuse.
#[derive(Default)]
struct Feed {
    pending: Option<VideoFrame>,
    spent: Vec<VideoFrame>,
    cleared: bool,
    aspect: Option<f32>,
}

static FEED: Mutex<Feed> = Mutex::new(Feed {
    pending: None,
    spent: Vec::new(),
    cleared: false,
    aspect: None,
});

/// At most this many uploaded frames wait for [`take_spent`].
const MAX_SPENT: usize = 2;

pub fn is_ready() -> bool {
    fluxplay_player::video_stage_ready()
}

/// Show `frame` on the next redraw. Returns the frame it replaces if that one
/// was never drawn (recycle it).
pub fn present(frame: VideoFrame) -> Option<VideoFrame> {
    let mut feed = FEED.lock().ok()?;
    feed.cleared = false;
    feed.pending.replace(frame)
}

/// Drop the picture (playback stopped); its textures go at the next frame.
pub fn clear() -> Option<VideoFrame> {
    let mut feed = FEED.lock().ok()?;
    feed.cleared = true;
    feed.spent.clear();
    feed.pending.take()
}

/// Uploaded CPU frames whose buffers the player can reuse.
pub fn take_spent() -> Vec<VideoFrame> {
    FEED.lock()
        .map(|mut feed| std::mem::take(&mut feed.spent))
        .unwrap_or_default()
}

/// Forced display aspect (width / height) for decoded YUV frames. RGBA frames
/// come from libmpv, which already applied it.
pub fn set_aspect(aspect: Option<f32>) {
    if let Ok(mut feed) = FEED.lock() {
        feed.aspect = aspect.filter(|a| a.is_finite() && *a > 0.1);
    }
}

/// The stage widget; fills its parent.
pub fn view<'a, Message: 'a>() -> Element<'a, Message> {
    iced::widget::shader(VideoProgram)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

struct VideoProgram;

impl<Message> shader::Program<Message> for VideoProgram {
    type State = ();
    type Primitive = VideoPrimitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, _bounds: Rectangle) -> VideoPrimitive {
        VideoPrimitive
    }
}

#[derive(Debug)]
struct VideoPrimitive;

impl shader::Primitive for VideoPrimitive {
    type Pipeline = StagePipeline;

    fn prepare(
        &self,
        pipeline: &mut StagePipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        bounds: &Rectangle,
        viewport: &Viewport,
    ) {
        let (frame, cleared, aspect) = match FEED.lock() {
            Ok(mut feed) => (feed.pending.take(), feed.cleared, feed.aspect),
            Err(_) => return,
        };
        if cleared && frame.is_none() {
            pipeline.picture = None;
        }
        if let Some(frame) = frame {
            pipeline.upload(device, queue, frame);
        }
        let scale = viewport.scale_factor() as f32;
        pipeline.write_params(queue, bounds.width * scale, bounds.height * scale, aspect);
    }

    fn draw(&self, pipeline: &StagePipeline, pass: &mut wgpu::RenderPass<'_>) -> bool {
        if let Some(picture) = &pipeline.picture {
            if picture.visible {
                pass.set_pipeline(&pipeline.pipeline);
                pass.set_bind_group(0, &picture.bind, &[]);
                pass.draw(0..4, 0..1);
            }
        }
        true
    }
}

/// What the shader needs to know about the picture.
#[derive(Clone, Copy, Debug)]
struct PictureInfo {
    layout: PixelLayout,
    width: u32,
    height: u32,
    sar: (u32, u32),
    matrix: ColorMatrix,
    full_range: bool,
}

/// Textures of the current picture.
struct Picture {
    info: PictureInfo,
    y: wgpu::Texture,
    uv: wgpu::Texture,
    bind: wgpu::BindGroup,
    /// False when the picture rectangle collapsed (zero-sized stage).
    visible: bool,
}

struct StagePipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
    srgb: bool,
    picture: Option<Picture>,
}

impl shader::Pipeline for StagePipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fluxplay video stage"),
            source: wgpu::ShaderSource::Wgsl(include_str!("video_stage.wgsl").into()),
        });
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Uint,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fluxplay video stage"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_entry(1),
                texture_entry(2),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fluxplay video stage"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fluxplay video stage"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fluxplay video stage params"),
            size: PARAMS_BYTES as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        install_video_stage(Arc::new(export::Exporter::new(device)));
        tracing::info!(?format, "GPU video stage ready (YUV → RGB in shader)");

        Self {
            pipeline,
            layout,
            uniform,
            srgb: format.is_srgb(),
            picture: None,
        }
    }

    fn trim(&mut self) {
        if FEED.lock().map(|feed| feed.cleared).unwrap_or(false) {
            self.picture = None;
        }
    }
}

const PARAMS_BYTES: usize = 96;

fn plane_formats(layout: PixelLayout) -> (wgpu::TextureFormat, wgpu::TextureFormat) {
    match layout {
        PixelLayout::Rgba => (wgpu::TextureFormat::Rgba8Uint, wgpu::TextureFormat::Rg8Uint),
        PixelLayout::Nv12 => (wgpu::TextureFormat::R8Uint, wgpu::TextureFormat::Rg8Uint),
        PixelLayout::P010 => (wgpu::TextureFormat::R16Uint, wgpu::TextureFormat::Rg16Uint),
    }
}

/// Luma extent and chroma extent (1×1 placeholder for RGBA).
fn plane_sizes(layout: PixelLayout, w: u32, h: u32) -> (wgpu::Extent3d, wgpu::Extent3d) {
    let y = wgpu::Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    };
    let uv = match layout {
        PixelLayout::Rgba => wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        _ => wgpu::Extent3d {
            width: w.div_ceil(2),
            height: h.div_ceil(2),
            depth_or_array_layers: 1,
        },
    };
    (y, uv)
}

impl StagePipeline {
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, frame: VideoFrame) {
        let (w, h, layout) = (frame.width, frame.height, frame.layout);
        if w < 2 || h < 2 {
            return;
        }
        let reuse = self
            .picture
            .as_ref()
            .is_some_and(|p| p.info.layout == layout && p.info.width == w && p.info.height == h);
        if !reuse {
            self.picture = Some(self.create_picture(device, layout, w, h));
        }
        let Some(picture) = self.picture.as_mut() else {
            return;
        };
        picture.info.sar = frame.sar;
        picture.info.matrix = frame.matrix;
        picture.info.full_range = frame.full_range;
        let (y_size, uv_size) = plane_sizes(layout, w, h);
        let target = |texture| wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        };
        let uv_offset = frame.pitch as u64 * h as u64;
        let copied_on_gpu = match &frame.data {
            FrameData::Cpu(bytes) => {
                if (bytes.len() as u64) < frame.pitch as u64 * layout.rows(h) as u64 {
                    return;
                }
                let plane = |offset| wgpu::TexelCopyBufferLayout {
                    offset,
                    bytes_per_row: Some(frame.pitch),
                    rows_per_image: None,
                };
                queue.write_texture(target(&picture.y), bytes, plane(0), y_size);
                if layout != PixelLayout::Rgba {
                    queue.write_texture(target(&picture.uv), bytes, plane(uv_offset), uv_size);
                }
                false
            }
            FrameData::Gpu(gpu) => {
                let Some(slots) = gpu.slots.downcast_ref::<export::SlotBuffers>() else {
                    return;
                };
                let Some(buffer) = slots.buffers.get(gpu.slot as usize) else {
                    return;
                };
                let plane = |offset| wgpu::TexelCopyBufferInfo {
                    buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset,
                        bytes_per_row: Some(frame.pitch),
                        rows_per_image: None,
                    },
                };
                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("fluxplay video slot copy"),
                });
                encoder.copy_buffer_to_texture(plane(0), target(&picture.y), y_size);
                encoder.copy_buffer_to_texture(plane(uv_offset), target(&picture.uv), uv_size);
                queue.submit(Some(encoder.finish()));
                true
            }
        };
        if copied_on_gpu {
            // The decoder may reuse the slot once the copy has run.
            queue.on_submitted_work_done(move || drop(frame));
        } else {
            push_spent(frame);
        }
    }

    fn create_picture(&self, device: &wgpu::Device, layout: PixelLayout, w: u32, h: u32) -> Picture {
        let (y_format, uv_format) = plane_formats(layout);
        let (y_size, uv_size) = plane_sizes(layout, w, h);
        let texture = |size, format| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("fluxplay video plane"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let y = texture(y_size, y_format);
        let uv = texture(uv_size, uv_format);
        let y_view = y.create_view(&wgpu::TextureViewDescriptor::default());
        let uv_view = uv.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fluxplay video stage"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&y_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&uv_view),
                },
            ],
        });
        Picture {
            info: PictureInfo {
                layout,
                width: w,
                height: h,
                sar: (1, 1),
                matrix: ColorMatrix::Bt709,
                full_range: false,
            },
            y,
            uv,
            bind,
            visible: false,
        }
    }

    /// Letterbox + filter parameters for a `bw`×`bh` pixel stage.
    fn write_params(&mut self, queue: &wgpu::Queue, bw: f32, bh: f32, aspect: Option<f32>) {
        let srgb = self.srgb;
        let Some(picture) = self.picture.as_mut() else {
            return;
        };
        let params = stage_params(&picture.info, bw, bh, aspect, srgb);
        picture.visible = params.is_some();
        if let Some(bytes) = params {
            queue.write_buffer(&self.uniform, 0, &bytes);
        }
    }
}

fn push_spent(frame: VideoFrame) {
    if let Ok(mut feed) = FEED.lock() {
        if feed.spent.len() < MAX_SPENT {
            feed.spent.push(frame);
        }
    }
}

/// Kernel stretch is capped: past 2.5× downscale a few extra taps are not
/// worth their cost (rare: 4K in a small window).
const MAX_KERNEL_STRETCH: f32 = 2.5;

fn stage_params(
    p: &PictureInfo,
    bw: f32,
    bh: f32,
    aspect: Option<f32>,
    srgb: bool,
) -> Option<[u8; PARAMS_BYTES]> {
    if bw < 1.0 || bh < 1.0 {
        return None;
    }
    let (w, h) = (p.width as f32, p.height as f32);
    let dar = match (p.layout, aspect) {
        (PixelLayout::Rgba, _) | (_, None) => {
            w * p.sar.0.max(1) as f32 / (h * p.sar.1.max(1) as f32)
        }
        (_, Some(a)) => a,
    };
    let (mut pw, mut ph) = if bw / bh > dar {
        (bh * dar, bh)
    } else {
        (bw, bw / dar)
    };
    // Whole pixels: a fractional edge blurs the first and last columns.
    pw = pw.round().clamp(1.0, bw);
    ph = ph.round().clamp(1.0, bh);
    let (sx, sy) = (
        (w / pw).clamp(1.0, MAX_KERNEL_STRETCH),
        (h / ph).clamp(1.0, MAX_KERNEL_STRETCH),
    );
    let rgba = p.layout == PixelLayout::Rgba;
    let bits: u32 = if p.layout == PixelLayout::P010 { 10 } else { 8 };
    let shift = if p.layout == PixelLayout::P010 { 6 } else { 0 };
    let unit = (1u32 << (bits - 8)) as f32;
    let max = ((1u32 << bits) - 1) as f32;
    let range = if rgba {
        [0.0, 1.0 / 255.0, 0.0, 0.0]
    } else if p.full_range {
        [0.0, 1.0 / max, 128.0 * unit, 1.0 / max]
    } else {
        [16.0 * unit, 1.0 / (219.0 * unit), 128.0 * unit, 1.0 / (224.0 * unit)]
    };
    let floats: [f32; 20] = {
        let c = yuv_to_rgb_coeffs(p.matrix);
        [
            -pw / bw,
            ph / bh,
            pw / bw,
            -ph / bh,
            w,
            h,
            (w / 2.0).ceil(),
            (h / 2.0).ceil(),
            sx,
            sy,
            (2.0 * sx).ceil(),
            (2.0 * sy).ceil(),
            c[0],
            c[1],
            c[2],
            c[3],
            range[0],
            range[1],
            range[2],
            range[3],
        ]
    };
    let ints: [u32; 4] = [rgba as u32, srgb as u32, shift, 0];
    let mut out = [0u8; PARAMS_BYTES];
    for (i, v) in floats.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    for (i, v) in ints.iter().enumerate() {
        out[80 + i * 4..84 + i * 4].copy_from_slice(&v.to_le_bytes());
    }
    Some(out)
}

/// Exportable Vulkan buffers for CUDA zero-copy (Linux, Vulkan backend).
mod export {
    use super::*;

    /// Buffers of one exported slot set; comes back in every GPU frame.
    pub struct SlotBuffers {
        pub buffers: Vec<wgpu::Buffer>,
    }

    pub struct Exporter {
        #[cfg(target_os = "linux")]
        device: wgpu::Device,
        /// Device UUID when the stage runs on an NVIDIA GPU through Vulkan
        /// with external memory support; otherwise nothing is exported.
        #[cfg(target_os = "linux")]
        uuid: Option<[u8; 16]>,
    }

    impl Exporter {
        pub fn new(device: &wgpu::Device) -> Self {
            #[cfg(target_os = "linux")]
            {
                let uuid = linux::nvidia_uuid(device);
                if uuid.is_some() {
                    tracing::info!("GPU video stage: CUDA frames can stay in VRAM");
                }
                Self {
                    device: device.clone(),
                    uuid,
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = device;
                Self {}
            }
        }
    }

    impl GpuStage for Exporter {
        #[cfg(unix)]
        fn export_slots(
            &self,
            count: usize,
            layout: PixelLayout,
            width: u32,
            height: u32,
        ) -> Option<fluxplay_player::ExportedSlots> {
            #[cfg(target_os = "linux")]
            {
                linux::export(&self.device, self.uuid?, count, layout, width, height)
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (count, layout, width, height);
                None
            }
        }
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use super::*;
        use ash::vk;
        use std::os::fd::{FromRawFd, OwnedFd};
        use wgpu::hal::api::Vulkan;

        const NVIDIA: u32 = 0x10de;
        const HANDLE: vk::ExternalMemoryHandleTypeFlags = vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD;
        /// `copy_buffer_to_texture` needs 256-byte aligned rows.
        const ROW_ALIGN: u32 = 256;

        pub fn nvidia_uuid(device: &wgpu::Device) -> Option<[u8; 16]> {
            // SAFETY: only reads properties through the raw handles.
            let hal = unsafe { device.as_hal::<Vulkan>() }?;
            if !hal
                .enabled_device_extensions()
                .contains(&ash::khr::external_memory_fd::NAME)
            {
                return None;
            }
            let instance = hal.shared_instance().raw_instance();
            let mut id = vk::PhysicalDeviceIDProperties::default();
            let vendor = {
                let mut props = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
                unsafe { instance.get_physical_device_properties2(hal.raw_physical_device(), &mut props) };
                props.properties.vendor_id
            };
            (vendor == NVIDIA).then_some(id.device_uuid)
        }

        /// Raw buffer + memory not yet owned by wgpu.
        struct Raw {
            buffer: vk::Buffer,
            memory: vk::DeviceMemory,
        }

        pub fn export(
            device: &wgpu::Device,
            uuid: [u8; 16],
            count: usize,
            layout: PixelLayout,
            width: u32,
            height: u32,
        ) -> Option<fluxplay_player::ExportedSlots> {
            let row = (width + 1) / 2 * 2 * layout.bytes_per_sample();
            let pitch = row.div_ceil(ROW_ALIGN) * ROW_ALIGN;
            let bytes = pitch as u64 * layout.rows(height) as u64;
            let mut raws = Vec::with_capacity(count);
            let mut fds = Vec::with_capacity(count);
            let mut alloc_size = 0u64;
            {
                let hal = unsafe { device.as_hal::<Vulkan>() }?;
                let raw = hal.raw_device();
                let instance = hal.shared_instance().raw_instance();
                let props =
                    unsafe { instance.get_physical_device_memory_properties(hal.raw_physical_device()) };
                let fd_api = ash::khr::external_memory_fd::Device::new(instance, raw);
                let free = |r: &Raw| unsafe {
                    raw.destroy_buffer(r.buffer, None);
                    raw.free_memory(r.memory, None);
                };
                for _ in 0..count {
                    match unsafe { allocate(raw, &props, &fd_api, bytes) } {
                        Some((r, fd, size)) if alloc_size == 0 || size == alloc_size => {
                            alloc_size = size;
                            raws.push(r);
                            fds.push(fd);
                        }
                        Some((r, _fd, _)) => {
                            free(&r);
                            break;
                        }
                        None => break,
                    }
                }
                if raws.len() != count {
                    raws.iter().for_each(free);
                    tracing::warn!("GPU video stage: exportable VRAM allocation failed");
                    return None;
                }
            }
            let buffers = raws
                .into_iter()
                .map(|r| unsafe {
                    let hal_buffer =
                        wgpu::hal::vulkan::Buffer::from_raw_managed(r.buffer, r.memory, 0, bytes);
                    device.create_buffer_from_hal::<Vulkan>(
                        hal_buffer,
                        &wgpu::BufferDescriptor {
                            label: Some("fluxplay video slot"),
                            size: bytes,
                            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        },
                    )
                })
                .collect();
            tracing::info!(count, width, height, ?layout, pitch, "GPU video stage: exported frame slots");
            Some(fluxplay_player::ExportedSlots {
                slots: Arc::new(SlotBuffers { buffers }),
                fds,
                slot_bytes: alloc_size,
                pitch,
                device_uuid: uuid,
            })
        }

        /// One device-local buffer with exportable memory, its fd and the
        /// allocation size (what CUDA must import).
        unsafe fn allocate(
            raw: &ash::Device,
            props: &vk::PhysicalDeviceMemoryProperties,
            fd_api: &ash::khr::external_memory_fd::Device,
            bytes: u64,
        ) -> Option<(Raw, OwnedFd, u64)> {
            let mut external = vk::ExternalMemoryBufferCreateInfo::default().handle_types(HANDLE);
            let info = vk::BufferCreateInfo::default()
                .size(bytes)
                .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .push_next(&mut external);
            let buffer = unsafe { raw.create_buffer(&info, None) }.ok()?;
            let req = unsafe { raw.get_buffer_memory_requirements(buffer) };
            let Some(type_index) = (0..props.memory_type_count).find(|&i| {
                req.memory_type_bits & (1 << i) != 0
                    && props.memory_types[i as usize]
                        .property_flags
                        .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            }) else {
                unsafe { raw.destroy_buffer(buffer, None) };
                return None;
            };
            let mut export = vk::ExportMemoryAllocateInfo::default().handle_types(HANDLE);
            let alloc = vk::MemoryAllocateInfo::default()
                .allocation_size(req.size)
                .memory_type_index(type_index)
                .push_next(&mut export);
            let Ok(memory) = (unsafe { raw.allocate_memory(&alloc, None) }) else {
                unsafe { raw.destroy_buffer(buffer, None) };
                return None;
            };
            let r = Raw { buffer, memory };
            let fd = unsafe { raw.bind_buffer_memory(buffer, memory, 0) }.ok().and_then(|()| {
                let get = vk::MemoryGetFdInfoKHR::default().memory(memory).handle_type(HANDLE);
                unsafe { fd_api.get_memory_fd(&get) }.ok()
            });
            match fd {
                Some(fd) if fd >= 0 => Some((r, unsafe { OwnedFd::from_raw_fd(fd) }, req.size)),
                _ => {
                    unsafe {
                        raw.destroy_buffer(r.buffer, None);
                        raw.free_memory(r.memory, None);
                    }
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(info: PictureInfo, bw: f32, bh: f32, aspect: Option<f32>) -> ([f32; 20], [u32; 4]) {
        let bytes = stage_params(&info, bw, bh, aspect, false).expect("visible");
        let word = |i: usize| [bytes[i * 4], bytes[i * 4 + 1], bytes[i * 4 + 2], bytes[i * 4 + 3]];
        let floats = std::array::from_fn(|i| f32::from_le_bytes(word(i)));
        let ints = std::array::from_fn(|i| u32::from_le_bytes(word(20 + i)));
        (floats, ints)
    }

    fn info(layout: PixelLayout, width: u32, height: u32, sar: (u32, u32)) -> PictureInfo {
        PictureInfo {
            layout,
            width,
            height,
            sar,
            matrix: ColorMatrix::Bt709,
            full_range: false,
        }
    }

    #[test]
    fn letterbox_keeps_display_aspect() {
        // 16:9 picture in a 4:3 stage → bars top and bottom.
        let (f, _) = params(info(PixelLayout::Nv12, 1920, 1080, (1, 1)), 1600.0, 1200.0, None);
        assert_eq!((f[0], f[2]), (-1.0, 1.0));
        assert!((f[1] - 900.0 / 1200.0).abs() < 1e-6 && f[3] == -f[1]);
        // Anamorphic PAL DVD (720×576, SAR 64:45) fills a 16:9 stage.
        let (f, _) = params(info(PixelLayout::Nv12, 720, 576, (64, 45)), 1920.0, 1080.0, None);
        assert_eq!((f[1], f[2]), (1.0, 1.0));
        // A forced 4:3 pillarboxes the same stage.
        let (f, _) = params(info(PixelLayout::Nv12, 1920, 1080, (1, 1)), 1920.0, 1080.0, Some(4.0 / 3.0));
        assert_eq!(f[2], 1440.0 / 1920.0);
        // libmpv RGBA already carries the forced aspect.
        let (f, i) = params(info(PixelLayout::Rgba, 1920, 1080, (1, 1)), 1920.0, 1080.0, Some(4.0 / 3.0));
        assert_eq!((f[2], i[0]), (1.0, 1));
    }

    #[test]
    fn kernel_widens_when_downscaling_only() {
        let (f, _) = params(info(PixelLayout::Nv12, 3840, 2160, (1, 1)), 1920.0, 1080.0, None);
        assert_eq!((f[8], f[10]), (2.0, 4.0));
        let (f, _) = params(info(PixelLayout::Nv12, 1280, 720, (1, 1)), 2560.0, 1440.0, None);
        assert_eq!((f[8], f[10]), (1.0, 2.0));
    }

    #[test]
    fn ten_bit_video_range_matches_eight_bit() {
        let (f8, i8) = params(info(PixelLayout::Nv12, 64, 64, (1, 1)), 64.0, 64.0, None);
        let (f10, i10) = params(info(PixelLayout::P010, 64, 64, (1, 1)), 64.0, 64.0, None);
        assert_eq!((i8[2], i10[2]), (0, 6));
        // Reference white: 235 (8-bit) and 940 (10-bit) both map to 1.0.
        assert!(((235.0 - f8[16]) * f8[17] - 1.0).abs() < 1e-6);
        assert!(((940.0 - f10[16]) * f10[17] - 1.0).abs() < 1e-6);
    }
}

/// End to end on a real GPU: FFmpeg decodes the clips, the stage renders them
/// offscreen and the pixels are compared with FFmpeg's own RGB conversion.
///
/// ```text
/// ffmpeg -f lavfi -i color=c=0xC03020:s=1280x720:r=25:d=6 -vf "format=rgb24,\
///   drawbox=x=0:y=360:w=1280:h=360:color=0x2040C0:t=fill,\
///   scale=out_color_matrix=bt709:out_range=tv,format=yuv420p" -c:v libx264 \
///   -colorspace bt709 /tmp/fluxplay-stage-8bit.mp4   # and yuv420p10le + libx265 → -10bit
/// cargo test -p fluxplay --lib gpu_stage -- --ignored --nocapture
/// ```
#[cfg(test)]
mod gpu_tests {
    use super::*;
    use fluxplay_core::models::{Channel, ContentKind, PlayerBackendPref};
    use fluxplay_player::{PlayOptions, StreamSession, VideoRect};
    use std::time::{Duration, Instant};

    const TOP: [u8; 3] = [189, 44, 30];
    const BOTTOM: [u8; 3] = [30, 62, 192];
    const TOLERANCE: i32 = 6;

    fn gpu() -> (wgpu::Device, wgpu::Queue, wgpu::AdapterInfo) {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });
        let want = std::env::var("FLUXPLAY_GPU").unwrap_or_else(|_| "nvidia".into()).to_lowercase();
        let mut adapters = instance.enumerate_adapters(wgpu::Backends::VULKAN);
        let pick = adapters
            .iter()
            .position(|a| a.get_info().name.to_lowercase().contains(&want))
            .unwrap_or(0);
        let adapter = adapters.swap_remove(pick);
        let info = adapter.get_info();
        let rt = tokio::runtime::Builder::new_current_thread().build().expect("runtime");
        let (device, queue) = rt
            .block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("stage test"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits {
                    max_bind_groups: 2,
                    ..wgpu::Limits::default()
                },
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            }))
            .expect("device");
        (device, queue, info)
    }

    /// First frame of `clip` that went through RAM (`gpu == false`) or stayed
    /// in VRAM (`gpu == true`), with its still-running session.
    fn first_frame(clip: &str, hwdec: bool, gpu: bool) -> (StreamSession, VideoFrame) {
        let mut opts = PlayOptions::default();
        opts.preferred = PlayerBackendPref::Ffmpeg;
        opts.hwdec = hwdec;
        let mut session = StreamSession::with_options(opts);
        session.set_video_rect(VideoRect::overlay(0, 0, 640, 360));
        session
            .open_channel(Channel {
                id: "stage-test".into(),
                name: "Stage test".into(),
                stream_url: format!("file://{clip}"),
                logo: None,
                group: None,
                tvg_id: None,
                tvg_name: None,
                tvg_logo: None,
                epg_channel_id: None,
                scheme: None,
                source_id: None,
                kind: ContentKind::Vod,
                catchup: None,
            })
            .expect("open");
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut seen = 0;
        while Instant::now() < deadline {
            if let Some(frame) = session.pull_frame(640, 360) {
                seen += 1;
                if matches!(frame.data, FrameData::Gpu(_)) == gpu {
                    return (session, frame);
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("{clip}: no {} frame in 8 s ({seen} other frames)", if gpu { "zero-copy" } else { "CPU" });
    }

    fn render(
        pipe: &mut StagePipeline,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: VideoFrame,
        (w, h): (u32, u32),
        readback: bool,
    ) -> Vec<u8> {
        let size = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        pipe.upload(device, queue, frame);
        pipe.write_params(queue, w as f32, h as f32, None);
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            <VideoPrimitive as shader::Primitive>::draw(&VideoPrimitive, pipe, &mut pass);
        }
        if !readback {
            queue.submit(Some(encoder.finish()));
            let _ = device.poll(wgpu::PollType::Poll);
            return Vec::new();
        }
        let row = (w * 4).div_ceil(256) * 256;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: row as u64 * h as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            size,
        );
        queue.submit(Some(encoder.finish()));
        readback.slice(..).map_async(wgpu::MapMode::Read, |r| r.expect("map"));
        device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        let mapped = readback.slice(..).get_mapped_range();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            out.extend_from_slice(&mapped[y * row as usize..][..(w * 4) as usize]);
        }
        out
    }

    fn assert_near(px: &[u8], x: u32, y: u32, w: u32, want: [u8; 3], what: &str) {
        let i = ((y * w + x) * 4) as usize;
        let got = &px[i..i + 3];
        let ok = got.iter().zip(want).all(|(&g, e)| (g as i32 - e as i32).abs() <= TOLERANCE);
        assert!(ok, "{what} at ({x},{y}): got {got:?}, want {want:?}");
    }

    #[test]
    #[ignore = "needs a Vulkan GPU, FFmpeg and /tmp/fluxplay-stage-{8,10}bit.mp4"]
    fn gpu_stage_renders_decoded_frames() {
        // Local files open only under the download root (as for a downloaded film).
        std::env::set_var("FLUXPLAY_DOWNLOAD_ROOT", "/tmp");
        let (device, queue, info) = gpu();
        println!("adapter: {} ({:?})", info.name, info.backend);
        let mut pipe = <StagePipeline as shader::Pipeline>::new(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
        );
        let zero_copy = info.vendor == 0x10de;
        for (clip, layout) in [
            ("/tmp/fluxplay-stage-8bit.mp4", PixelLayout::Nv12),
            ("/tmp/fluxplay-stage-10bit.mp4", PixelLayout::P010),
        ] {
            let mut paths = vec![(false, false)];
            if zero_copy {
                paths.push((true, true));
            }
            for (hwdec, gpu) in paths {
                let what = format!("{clip} hwdec={hwdec} zero-copy={gpu}");
                let (mut session, frame) = first_frame(clip, hwdec, gpu);
                assert_eq!(frame.layout, layout, "{what}");
                assert_eq!((frame.width, frame.height), (1280, 720), "{what}");
                assert_eq!(frame.matrix, ColorMatrix::Bt709, "{what}");
                // 4:3 stage: 16:9 picture letterboxed to 640×360, bars of 60 px.
                let px = render(&mut pipe, &device, &queue, frame, (640, 480), true);
                assert_near(&px, 320, 150, 640, TOP, &what);
                assert_near(&px, 320, 330, 640, BOTTOM, &what);
                assert_near(&px, 320, 20, 640, [0, 0, 0], &what);
                assert_near(&px, 320, 460, 640, [0, 0, 0], &what);
                // Screenshot fallback: CPU planes converted in Rust, zero-copy
                // frames read back from the decoder.
                let (sw, sh, shot) = session.soft_rgba_snapshot().expect("snapshot");
                assert_eq!((sw, sh), (1280, 720), "{what} snapshot");
                assert_near(&shot, 640, 180, sw, TOP, &format!("{what} snapshot"));
                assert_near(&shot, 640, 540, sw, BOTTOM, &format!("{what} snapshot"));
                println!("ok: {what}");
                session.stop();
            }
        }
    }

    /// CPU seconds (user + system) used by this process so far.
    fn cpu_secs() -> f64 {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
        let after = stat.rsplit_once(')').map(|(_, rest)| rest).unwrap_or("");
        let fields: Vec<&str> = after.split_whitespace().collect();
        let ticks = |i: usize| fields.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        (ticks(11) + ticks(12)) / 100.0
    }

    /// Plays `clip` for `secs` at a 1080p stage, uploading every frame like the
    /// app does; returns (frames shown, CPU seconds).
    fn play_for(
        clip: &str,
        secs: u64,
        mut show: impl FnMut(&mut StreamSession) -> bool,
    ) -> (u32, f64) {
        let mut opts = PlayOptions::default();
        opts.preferred = PlayerBackendPref::Ffmpeg;
        opts.hwdec = true;
        let mut session = StreamSession::with_options(opts);
        session.set_video_rect(VideoRect::overlay(0, 0, 1920, 1080));
        session
            .open_channel(Channel {
                id: "stage-bench".into(),
                name: "Stage bench".into(),
                stream_url: format!("file://{clip}"),
                logo: None,
                group: None,
                tvg_id: None,
                tvg_name: None,
                tvg_logo: None,
                epg_channel_id: None,
                scheme: None,
                source_id: None,
                kind: ContentKind::Vod,
                catchup: None,
            })
            .expect("open");
        // Warm-up: decoder init, first attach.
        let warm = Instant::now() + Duration::from_secs(2);
        while Instant::now() < warm {
            show(&mut session);
            std::thread::sleep(Duration::from_millis(4));
        }
        let cpu0 = cpu_secs();
        let end = Instant::now() + Duration::from_secs(secs);
        let mut frames = 0;
        while Instant::now() < end {
            if show(&mut session) {
                frames += 1;
            }
            std::thread::sleep(Duration::from_millis(4));
        }
        let cpu = cpu_secs() - cpu0;
        session.stop();
        (frames, cpu)
    }

    /// Same 4K HEVC Main10 clip through the RGBA image path, then through the
    /// stage (zero-copy on NVIDIA).
    ///
    /// ```text
    /// ffmpeg -f lavfi -i testsrc2=s=3840x2160:r=25:d=14 -vf format=p010le \\
    ///   -c:v hevc_nvenc -profile:v main10 -b:v 25M /tmp/fluxplay-stage-4k10.mp4
    /// cargo test -p fluxplay --lib gpu_stage_cpu -- --ignored --nocapture --test-threads=1
    /// ```
    #[test]
    #[ignore = "needs a Vulkan GPU, FFmpeg and /tmp/fluxplay-stage-4k10.mp4"]
    fn gpu_stage_cpu_cost() {
        std::env::set_var("FLUXPLAY_DOWNLOAD_ROOT", "/tmp");
        let clip = "/tmp/fluxplay-stage-4k10.mp4";
        let secs = 8;
        let (device, queue, _) = gpu();
        assert!(!is_ready(), "run this test alone: the stage must not be installed yet");

        // Legacy: scaled RGBA from the decoder, uploaded into a texture.
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 1920,
                height: 1080,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let (legacy_frames, legacy_cpu) = play_for(clip, secs, |session| {
            if !session.frame_needs_redraw() {
                return false;
            }
            let Some((w, h, rgba)) = session.pull_video_frame(1920, 1080) else {
                return false;
            };
            if w == 1920 && h == 1080 {
                queue.write_texture(
                    texture.as_image_copy(),
                    &rgba,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(w * 4),
                        rows_per_image: None,
                    },
                    texture.size(),
                );
                queue.submit(None);
            }
            session.recycle_soft_rgba(rgba);
            true
        });

        // Stage: native frames drawn by the shader into a 1080p target.
        let mut pipe = <StagePipeline as shader::Pipeline>::new(
            &device,
            &queue,
            wgpu::TextureFormat::Rgba8Unorm,
        );
        let mut zero_copy = 0;
        let (stage_frames, stage_cpu) = play_for(clip, secs, |session| {
            if !session.frame_needs_redraw() {
                return false;
            }
            let Some(frame) = session.pull_frame(1920, 1080) else {
                return false;
            };
            if matches!(frame.data, FrameData::Gpu(_)) {
                zero_copy += 1;
            }
            render(&mut pipe, &device, &queue, frame, (1920, 1080), false);
            for spent in take_spent() {
                session.recycle_video_frame(spent);
            }
            true
        });

        let per = |cpu: f64| cpu / secs as f64 * 100.0;
        println!(
            "4K HEVC Main10 → 1080p stage, {secs} s:\n  image path: {legacy_frames} frames, CPU {:.0} % of one core\n  GPU stage : {stage_frames} frames ({zero_copy} zero-copy), CPU {:.0} % of one core",
            per(legacy_cpu),
            per(stage_cpu)
        );
        assert!(stage_frames as f64 >= secs as f64 * 25.0 * 0.9, "stage dropped frames");
    }
}
