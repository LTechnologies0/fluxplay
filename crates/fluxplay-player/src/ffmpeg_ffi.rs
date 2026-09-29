//! In-process FFmpeg demux/decode → RGBA (embedded in iced, same model as libmpv SW render).

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::ptr;

use tracing::{debug, error, info};

use crate::video_frame::{ColorMatrix, PixelLayout};
use crate::{PlayerError, Result};

#[repr(C)]
struct FluxFfmpegPlayer {
    _opaque: [u8; 0],
}

#[repr(C)]
struct FluxFfmpegOpenOpts {
    url: *const c_char,
    user_agent: *const c_char,
    referer: *const c_char,
    http_proxy: *const c_char,
    low_latency: c_int,
    hwdec: c_int,
}

#[repr(C)]
#[derive(Default)]
struct FluxFrameInfo {
    kind: c_int,
    format: c_int,
    width: c_int,
    height: c_int,
    pitch: c_int,
    sar_num: c_int,
    sar_den: c_int,
    matrix: c_int,
    full_range: c_int,
    slot: c_int,
    bytes: u64,
}

const FLUX_FRAME_GPU: c_int = 2;

extern "C" {
    fn flux_ffmpeg_open(opts: *const FluxFfmpegOpenOpts) -> *mut FluxFfmpegPlayer;
    fn flux_ffmpeg_close(p: *mut FluxFfmpegPlayer);
    fn flux_ffmpeg_set_av_log_level(level: c_int);
    fn flux_ffmpeg_pull_rgba(
        p: *mut FluxFfmpegPlayer,
        out: *mut u8,
        out_w: c_int,
        out_h: c_int,
        got_w: *mut c_int,
        got_h: *mut c_int,
    ) -> c_int;
    fn flux_ffmpeg_frame_size(p: *mut FluxFfmpegPlayer, w: *mut c_int, h: *mut c_int) -> c_int;
    fn flux_ffmpeg_set_output_size(p: *mut FluxFfmpegPlayer, w: c_int, h: c_int);
    fn flux_ffmpeg_set_present_hz(p: *mut FluxFfmpegPlayer, hz: c_int);
    fn flux_ffmpeg_is_alive(p: *mut FluxFfmpegPlayer) -> c_int;
    fn flux_ffmpeg_has_frame(p: *mut FluxFfmpegPlayer) -> c_int;
    fn flux_ffmpeg_pause(p: *mut FluxFfmpegPlayer, paused: c_int);
    fn flux_ffmpeg_set_volume(p: *mut FluxFfmpegPlayer, volume01: f32);
    fn flux_ffmpeg_position_secs(p: *mut FluxFfmpegPlayer) -> f64;
    fn flux_ffmpeg_duration_secs(p: *mut FluxFfmpegPlayer) -> f64;
    fn flux_ffmpeg_seek(p: *mut FluxFfmpegPlayer, secs: f64);
    fn flux_ffmpeg_buffer_state(
        p: *mut FluxFfmpegPlayer,
        buffered_secs: *mut f64,
        goal_secs: *mut f64,
        net_mbps: *mut f64,
        media_mbps: *mut f64,
    ) -> c_int;
    fn flux_ffmpeg_set_yuv_output(p: *mut FluxFfmpegPlayer, on: c_int);
    fn flux_ffmpeg_frame_info(p: *mut FluxFfmpegPlayer, info: *mut FluxFrameInfo) -> c_int;
    fn flux_ffmpeg_pull_frame(
        p: *mut FluxFfmpegPlayer,
        info: *mut FluxFrameInfo,
        out: *mut u8,
        cap: u64,
    ) -> c_int;
    fn flux_ffmpeg_gpu_need(
        p: *mut FluxFfmpegPlayer,
        w: *mut c_int,
        h: *mut c_int,
        bpc: *mut c_int,
    ) -> c_int;
    #[cfg(unix)]
    fn flux_ffmpeg_gpu_attach(
        p: *mut FluxFfmpegPlayer,
        fds: *const c_int,
        n: c_int,
        slot_bytes: u64,
        pitch: c_int,
        w: c_int,
        h: c_int,
        bpc: c_int,
        uuid: *const u8,
    ) -> c_int;
    fn flux_ffmpeg_gpu_release(p: *mut FluxFfmpegPlayer, slot: c_int);
    fn flux_ffmpeg_gpu_snapshot(
        p: *mut FluxFfmpegPlayer,
        out: *mut u8,
        cap: u64,
        w: *mut c_int,
        h: *mut c_int,
    ) -> c_int;
}

/// Frame description from [`LibFfmpeg::pull_frame`].
pub(crate) struct PulledFrame {
    pub layout: PixelLayout,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub sar: (u32, u32),
    pub matrix: ColorMatrix,
    pub full_range: bool,
    /// Zero-copy slot holding the planes; `None` when they were copied into
    /// the caller's buffer.
    pub gpu_slot: Option<u32>,
}

/// Embedded FFmpeg player owned by the UI tick thread.
pub struct LibFfmpeg {
    ptr: *mut FluxFfmpegPlayer,
    /// Keep CStrings alive for the open call (decode thread copies them).
    _keepalive: Vec<CString>,
    /// Recycled pull destination — avoid `vec![0; w*h*4]` every present.
    pull_buf: std::sync::Mutex<Vec<u8>>,
    last_out_w: std::sync::atomic::AtomicU32,
    last_out_h: std::sync::atomic::AtomicU32,
    /// Stage buffers CUDA writes into; dropped only after the decoder closed.
    gpu_slots: std::sync::Mutex<Option<std::sync::Arc<dyn std::any::Any + Send + Sync>>>,
}

unsafe impl Send for LibFfmpeg {}
unsafe impl Sync for LibFfmpeg {}

impl LibFfmpeg {
    pub fn open(
        url: &str,
        user_agent: Option<&str>,
        referer: Option<&str>,
        http_proxy: Option<&str>,
        low_latency: bool,
        hwdec: bool,
    ) -> Result<Self> {
        debug!("LibFfmpeg::open");
        if let Some(level) = crate::native_log::ffmpeg_av_log_level() {
            unsafe { flux_ffmpeg_set_av_log_level(level) };
            info!(av_log_level = level, "libav* verbose logging enabled");
        }
        let mut keepalive = Vec::new();
        let url_c = CString::new(url).map_err(|e| PlayerError::Backend(e.to_string()))?;
        let ua_c = user_agent
            .map(CString::new)
            .transpose()
            .map_err(|e| PlayerError::Backend(e.to_string()))?;
        let ref_c = referer
            .map(CString::new)
            .transpose()
            .map_err(|e| PlayerError::Backend(e.to_string()))?;
        let proxy_c = http_proxy
            .map(CString::new)
            .transpose()
            .map_err(|e| PlayerError::Backend(e.to_string()))?;

        let opts = FluxFfmpegOpenOpts {
            url: url_c.as_ptr(),
            user_agent: ua_c.as_ref().map(|c| c.as_ptr()).unwrap_or(ptr::null()),
            referer: ref_c.as_ref().map(|c| c.as_ptr()).unwrap_or(ptr::null()),
            http_proxy: proxy_c.as_ref().map(|c| c.as_ptr()).unwrap_or(ptr::null()),
            low_latency: if low_latency { 1 } else { 0 },
            hwdec: if hwdec { 1 } else { 0 },
        };
        keepalive.push(url_c);
        if let Some(c) = ua_c {
            keepalive.push(c);
        }
        if let Some(c) = ref_c {
            keepalive.push(c);
        }
        if let Some(c) = proxy_c {
            keepalive.push(c);
        }

        let ptr = unsafe { flux_ffmpeg_open(&opts) };
        if ptr.is_null() {
            error!("flux_ffmpeg_open returned null");
            return Err(PlayerError::Backend(
                "FFmpeg natif: ouverture du flux impossible".into(),
            ));
        }
        info!("LibFfmpeg decode thread started");
        Ok(Self {
            ptr,
            _keepalive: keepalive,
            pull_buf: std::sync::Mutex::new(Vec::new()),
            last_out_w: std::sync::atomic::AtomicU32::new(0),
            last_out_h: std::sync::atomic::AtomicU32::new(0),
            gpu_slots: std::sync::Mutex::new(None),
        })
    }

    /// Soft-present size: trust the iced/app budget `(w,h)` (already quality-clamped).
    /// Only enforce even dims and a hard decode ceiling — do not re-bucket by core count
    /// (that fought `AndroidDeviceCaps::soft_budget` and discarded frames).
    pub fn soft_present_dims(w: u32, h: u32) -> (u32, u32) {
        let rw = (w.clamp(2, 3840) & !1).max(2);
        let rh = (h.clamp(2, 2160) & !1).max(2);
        (rw, rh)
    }

    pub fn set_output_size(&self, w: u32, h: u32) {
        let (w, h) = Self::soft_present_dims(w, h);
        use std::sync::atomic::Ordering;
        if self.last_out_w.load(Ordering::Relaxed) == w
            && self.last_out_h.load(Ordering::Relaxed) == h
        {
            return;
        }
        self.last_out_w.store(w, Ordering::Relaxed);
        self.last_out_h.store(h, Ordering::Relaxed);
        unsafe { flux_ffmpeg_set_output_size(self.ptr, w as c_int, h as c_int) };
    }

    pub fn set_present_hz(&self, hz: u32) {
        let hz = hz.clamp(24, 120);
        unsafe { flux_ffmpeg_set_present_hz(self.ptr, hz as c_int) };
    }

    pub fn pull_rgba(&self, w: u32, h: u32) -> Option<(u32, u32, Vec<u8>)> {
        let (w, h) = Self::soft_present_dims(w, h);
        self.set_output_size(w, h);
        if !self.has_frame() {
            return None;
        }
        // Size the buffer to the ready frame — never drop it (black screen) or
        // overflow a smaller pull buffer (SEGV).
        let (fw, fh) = {
            let mut fw = 0;
            let mut fh = 0;
            let ok = unsafe { flux_ffmpeg_frame_size(self.ptr, &mut fw, &mut fh) };
            if ok != 1 || fw < 2 || fh < 2 {
                return None;
            }
            (fw as u32, fh as u32)
        };
        let aw = fw.max(w);
        let ah = fh.max(h);
        let need = (aw as usize).saturating_mul(ah as usize).saturating_mul(4);
        let mut slot = self.pull_buf.lock().ok()?;
        if slot.capacity() < need {
            *slot = Vec::with_capacity(need);
        }
        // SAFETY: C memcpy writes all got_w*got_h*4 bytes on success.
        unsafe {
            slot.set_len(need);
        }
        let mut got_w = 0;
        let mut got_h = 0;
        let ok = unsafe {
            flux_ffmpeg_pull_rgba(
                self.ptr,
                slot.as_mut_ptr(),
                aw as c_int,
                ah as c_int,
                &mut got_w,
                &mut got_h,
            )
        };
        if ok == 1 && got_w >= 2 && got_h >= 2 {
            let n = (got_w as usize).saturating_mul(got_h as usize).saturating_mul(4);
            slot.truncate(n);
            let out = std::mem::replace(&mut *slot, Vec::with_capacity(need));
            Some((got_w as u32, got_h as u32, out))
        } else {
            slot.clear();
            None
        }
    }

    pub fn is_alive(&self) -> bool {
        unsafe { flux_ffmpeg_is_alive(self.ptr) != 0 }
    }

    pub fn has_frame(&self) -> bool {
        unsafe { flux_ffmpeg_has_frame(self.ptr) != 0 }
    }

    pub fn pause(&self, paused: bool) {
        unsafe { flux_ffmpeg_pause(self.ptr, if paused { 1 } else { 0 }) };
    }

    pub fn set_volume(&self, volume01: f32) {
        unsafe { flux_ffmpeg_set_volume(self.ptr, volume01.clamp(0.0, 1.0)) };
    }

    pub fn position_secs(&self) -> f64 {
        unsafe { flux_ffmpeg_position_secs(self.ptr) }
    }

    pub fn duration_secs(&self) -> f64 {
        unsafe { flux_ffmpeg_duration_secs(self.ptr) }
    }

    pub fn seek(&self, secs: f64) {
        unsafe { flux_ffmpeg_seek(self.ptr, secs.max(0.0)) };
    }

    pub fn buffer_state(&self) -> crate::BufferState {
        let (mut buffered, mut goal, mut net, mut media) = (0.0, 0.0, 0.0, 0.0);
        let rebuffering = unsafe {
            flux_ffmpeg_buffer_state(self.ptr, &mut buffered, &mut goal, &mut net, &mut media)
        } != 0;
        crate::BufferState {
            rebuffering,
            buffered_secs: buffered,
            goal_secs: goal,
            net_mbps: net,
            media_mbps: media,
        }
    }

    /// Native YUV at decoded size (GPU stage) instead of RGBA scaled to the
    /// output size.
    pub fn set_yuv_output(&self, on: bool) {
        unsafe { flux_ffmpeg_set_yuv_output(self.ptr, on as c_int) };
    }

    /// Take the published frame. CPU planes are copied into `buf` (resized).
    pub(crate) fn pull_frame(&self, buf: &mut Vec<u8>) -> Option<PulledFrame> {
        let mut info = FluxFrameInfo::default();
        if unsafe { flux_ffmpeg_frame_info(self.ptr, &mut info) } != 1 {
            return None;
        }
        let gpu = info.kind == FLUX_FRAME_GPU;
        let need = if gpu { 0 } else { usize::try_from(info.bytes).ok()? };
        if buf.len() < need {
            buf.resize(need, 0);
        }
        let ok = unsafe {
            flux_ffmpeg_pull_frame(self.ptr, &mut info, buf.as_mut_ptr(), buf.len() as u64)
        };
        if ok != 1 || info.width < 2 || info.height < 2 || info.pitch <= 0 {
            return None;
        }
        if !gpu {
            buf.truncate(info.bytes as usize);
        }
        let layout = match info.format {
            1 => PixelLayout::Nv12,
            2 => PixelLayout::P010,
            _ => PixelLayout::Rgba,
        };
        let matrix = match info.matrix {
            1 => ColorMatrix::Bt709,
            2 => ColorMatrix::Bt2020,
            _ => ColorMatrix::Bt601,
        };
        Some(PulledFrame {
            layout,
            width: info.width as u32,
            height: info.height as u32,
            pitch: info.pitch as u32,
            sar: (info.sar_num.max(1) as u32, info.sar_den.max(1) as u32),
            matrix,
            full_range: info.full_range != 0,
            gpu_slot: (gpu && info.slot >= 0).then_some(info.slot as u32),
        })
    }

    /// Size and depth (1 = NV12, 2 = P010) of CUDA frames waiting for GPU slots.
    pub(crate) fn gpu_need(&self) -> Option<(u32, u32, u32)> {
        let (mut w, mut h, mut bpc) = (0, 0, 0);
        let need = unsafe { flux_ffmpeg_gpu_need(self.ptr, &mut w, &mut h, &mut bpc) } == 1;
        (need && w >= 2 && h >= 2 && bpc >= 1).then_some((w as u32, h as u32, bpc as u32))
    }

    /// Hand exported stage buffers to CUDA. Ownership of the fds moves to C.
    #[cfg(unix)]
    pub(crate) fn gpu_attach(
        &self,
        slots: crate::video_frame::ExportedSlots,
        w: u32,
        h: u32,
        bpc: u32,
    ) -> bool {
        use std::os::fd::IntoRawFd;
        let n = slots.fds.len();
        let fds: Vec<c_int> = slots.fds.into_iter().map(IntoRawFd::into_raw_fd).collect();
        let ok = unsafe {
            flux_ffmpeg_gpu_attach(
                self.ptr,
                fds.as_ptr(),
                n as c_int,
                slots.slot_bytes,
                slots.pitch as c_int,
                w as c_int,
                h as c_int,
                bpc as c_int,
                slots.device_uuid.as_ptr(),
            )
        } == 1;
        if ok {
            if let Ok(mut keep) = self.gpu_slots.lock() {
                *keep = Some(slots.slots);
            }
        }
        ok
    }

    /// The UI finished reading `slot`; decode may overwrite it.
    pub(crate) fn gpu_release(&self, slot: u32) {
        unsafe { flux_ffmpeg_gpu_release(self.ptr, slot as c_int) };
    }

    /// RGBA copy of the last zero-copy frame (`w`×`h` known from its info).
    pub(crate) fn gpu_snapshot(&self, w: u32, h: u32) -> Option<(u32, u32, Vec<u8>)> {
        let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
        let (mut gw, mut gh) = (0, 0);
        let ok = unsafe {
            flux_ffmpeg_gpu_snapshot(self.ptr, out.as_mut_ptr(), out.len() as u64, &mut gw, &mut gh)
        } == 1;
        if !ok || gw < 2 || gh < 2 {
            return None;
        }
        out.truncate((gw as usize) * (gh as usize) * 4);
        Some((gw as u32, gh as u32, out))
    }

    pub fn shutdown(self) {
        // Drop closes.
    }
}

impl Drop for LibFfmpeg {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            debug!("LibFfmpeg::drop close");
            unsafe { flux_ffmpeg_close(self.ptr) };
            self.ptr = ptr::null_mut();
            info!("LibFfmpeg closed");
        }
    }
}
