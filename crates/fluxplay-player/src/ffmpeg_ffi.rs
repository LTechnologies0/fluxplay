//! In-process FFmpeg demux/decode → RGBA (embedded in iced, same model as libmpv SW render).

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::ptr;

use tracing::{debug, error, info};

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

extern "C" {
    fn flux_ffmpeg_open(opts: *const FluxFfmpegOpenOpts) -> *mut FluxFfmpegPlayer;
    fn flux_ffmpeg_close(p: *mut FluxFfmpegPlayer);
    fn flux_ffmpeg_set_av_log_level(level: c_int);
    fn flux_ffmpeg_pull_rgba(
        p: *mut FluxFfmpegPlayer,
        out: *mut u8,
        out_w: c_int,
        out_h: c_int,
    ) -> c_int;
    fn flux_ffmpeg_set_output_size(p: *mut FluxFfmpegPlayer, w: c_int, h: c_int);
    fn flux_ffmpeg_is_alive(p: *mut FluxFfmpegPlayer) -> c_int;
    fn flux_ffmpeg_has_frame(p: *mut FluxFfmpegPlayer) -> c_int;
    fn flux_ffmpeg_pause(p: *mut FluxFfmpegPlayer, paused: c_int);
    fn flux_ffmpeg_set_volume(p: *mut FluxFfmpegPlayer, volume01: f32);
    fn flux_ffmpeg_position_secs(p: *mut FluxFfmpegPlayer) -> f64;
    fn flux_ffmpeg_duration_secs(p: *mut FluxFfmpegPlayer) -> f64;
    fn flux_ffmpeg_seek(p: *mut FluxFfmpegPlayer, secs: f64);
}

/// Embedded FFmpeg player owned by the UI tick thread.
pub struct LibFfmpeg {
    ptr: *mut FluxFfmpegPlayer,
    /// Keep CStrings alive for the open call (decode thread copies them).
    _keepalive: Vec<CString>,
    /// Recycled pull destination — avoid `vec![0; w*h*4]` every present.
    pull_buf: std::sync::Mutex<Vec<u8>>,
}

unsafe impl Send for LibFfmpeg {}

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
        })
    }

    /// Soft-present size (even dims, optional UHD). Matches iced pull clamp.
    pub fn soft_present_dims(w: u32, h: u32) -> (u32, u32) {
        let uhd = std::env::var_os("FLUXPLAY_SOFT_UHD").is_some();
        let max_w = if uhd { 3840u32 } else { 1920 };
        let max_h = if uhd { 2160u32 } else { 1080 };
        let scale = (max_w as f32 / w.max(1) as f32)
            .min(max_h as f32 / h.max(1) as f32)
            .min(1.0);
        let rw = ((w as f32 * scale).round() as u32).max(2) & !1;
        let rh = ((h as f32 * scale).round() as u32).max(2) & !1;
        (rw, rh)
    }

    pub fn set_output_size(&self, w: u32, h: u32) {
        let (w, h) = Self::soft_present_dims(w, h);
        unsafe { flux_ffmpeg_set_output_size(self.ptr, w as c_int, h as c_int) };
    }

    pub fn pull_rgba(&self, w: u32, h: u32) -> Option<Vec<u8>> {
        let (w, h) = Self::soft_present_dims(w, h);
        // Don't pay for a buffer when nothing is ready.
        if !self.has_frame() {
            self.set_output_size(w, h);
            return None;
        }
        self.set_output_size(w, h);
        let need = (w as usize).saturating_mul(h as usize).saturating_mul(4);
        let mut slot = self.pull_buf.lock().ok()?;
        if slot.capacity() < need {
            *slot = Vec::with_capacity(need);
        }
        // SAFETY: C memcpy writes all `need` bytes on success; on failure we don't expose.
        unsafe {
            slot.set_len(need);
        }
        let ok = unsafe {
            flux_ffmpeg_pull_rgba(self.ptr, slot.as_mut_ptr(), w as c_int, h as c_int)
        };
        if ok == 1 {
            // Hand buffer to caller; keep capacity for the next pull (no zero-fill).
            let out = std::mem::replace(&mut *slot, Vec::with_capacity(need));
            Some(out)
        } else {
            // Keep capacity for the next attempt; don't leave a "full" len of stale bytes.
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
