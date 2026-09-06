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
    low_latency: c_int,
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
}

unsafe impl Send for LibFfmpeg {}

impl LibFfmpeg {
    pub fn open(
        url: &str,
        user_agent: Option<&str>,
        referer: Option<&str>,
        low_latency: bool,
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

        let opts = FluxFfmpegOpenOpts {
            url: url_c.as_ptr(),
            user_agent: ua_c.as_ref().map(|c| c.as_ptr()).unwrap_or(ptr::null()),
            referer: ref_c.as_ref().map(|c| c.as_ptr()).unwrap_or(ptr::null()),
            low_latency: if low_latency { 1 } else { 0 },
        };
        keepalive.push(url_c);
        if let Some(c) = ua_c {
            keepalive.push(c);
        }
        if let Some(c) = ref_c {
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
        })
    }

    pub fn set_output_size(&self, w: u32, h: u32) {
        let w = (w.clamp(2, 3840) & !1).max(2) as c_int;
        let h = (h.clamp(2, 2160) & !1).max(2) as c_int;
        unsafe { flux_ffmpeg_set_output_size(self.ptr, w, h) };
    }

    pub fn pull_rgba(&self, w: u32, h: u32) -> Option<Vec<u8>> {
        let w = (w.clamp(2, 3840) & !1).max(2);
        let h = (h.clamp(2, 2160) & !1).max(2);
        self.set_output_size(w, h);
        let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
        let ok = unsafe {
            flux_ffmpeg_pull_rgba(self.ptr, buf.as_mut_ptr(), w as c_int, h as c_int)
        };
        if ok == 1 {
            Some(buf)
        } else {
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
