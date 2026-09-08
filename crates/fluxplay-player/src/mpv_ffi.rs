//! Minimal libmpv client + software render bindings (embedded video in iced).

#![allow(non_camel_case_types, dead_code)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tracing::{debug, error, info, warn};

use crate::{PlayerError, Result};

#[repr(C)]
pub struct mpv_handle {
    _opaque: [u8; 0],
}

#[repr(C)]
struct mpv_render_context {
    _opaque: [u8; 0],
}

#[repr(C)]
struct MpvRenderParam {
    type_: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct MpvEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

const MPV_EVENT_SHUTDOWN: c_int = 1;
const MPV_EVENT_NONE: c_int = 0;

const MPV_RENDER_PARAM_INVALID: c_int = 0;
const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
const MPV_RENDER_PARAM_SW_SIZE: c_int = 17;
const MPV_RENDER_PARAM_SW_FORMAT: c_int = 18;
const MPV_RENDER_PARAM_SW_STRIDE: c_int = 19;
const MPV_RENDER_PARAM_SW_POINTER: c_int = 20;

/// MPV_RENDER_UPDATE_FRAME — new video frame available for render.
const MPV_RENDER_UPDATE_FRAME: u64 = 1 << 0;

extern "C" {
    fn mpv_create() -> *mut mpv_handle;
    fn mpv_initialize(ctx: *mut mpv_handle) -> c_int;
    fn mpv_terminate_destroy(ctx: *mut mpv_handle);
    fn mpv_set_option_string(
        ctx: *mut mpv_handle,
        name: *const c_char,
        data: *const c_char,
    ) -> c_int;
    fn mpv_set_property_string(
        ctx: *mut mpv_handle,
        name: *const c_char,
        data: *const c_char,
    ) -> c_int;
    fn mpv_command(ctx: *mut mpv_handle, args: *const *const c_char) -> c_int;
    fn mpv_get_property_string(ctx: *mut mpv_handle, name: *const c_char) -> *mut c_char;
    fn mpv_free(data: *mut c_void);
    fn mpv_error_string(error: c_int) -> *const c_char;
    fn mpv_wait_event(ctx: *mut mpv_handle, timeout: f64) -> *mut MpvEvent;

    fn mpv_render_context_create(
        res: *mut *mut mpv_render_context,
        mpv: *mut mpv_handle,
        params: *mut MpvRenderParam,
    ) -> c_int;
    fn mpv_render_context_free(ctx: *mut mpv_render_context);
    fn mpv_render_context_render(ctx: *mut mpv_render_context, params: *mut MpvRenderParam)
        -> c_int;
    fn mpv_render_context_update(ctx: *mut mpv_render_context) -> u64;
    fn mpv_render_context_set_update_callback(
        ctx: *mut mpv_render_context,
        callback: Option<unsafe extern "C" fn(*mut c_void)>,
        callback_ctx: *mut c_void,
    );
}

fn mpv_err(code: c_int) -> Result<()> {
    if code >= 0 {
        return Ok(());
    }
    let msg = unsafe {
        let p = mpv_error_string(code);
        if p.is_null() {
            format!("libmpv error {code}")
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    };
    warn!(code, %msg, "libmpv error");
    Err(PlayerError::Backend(format!("libmpv: {msg}")))
}

unsafe extern "C" fn render_update_cb(ctx: *mut c_void) {
    if ctx.is_null() {
        return;
    }
    let flag = &*(ctx as *const AtomicBool);
    flag.store(true, Ordering::Release);
}

/// In-process libmpv player with optional software render target (no OS video window).
pub struct LibMpv {
    ctx: *mut mpv_handle,
    render: *mut mpv_render_context,
    frame_dirty: Arc<AtomicBool>,
    /// Reused tightly-packed RGBA scratch (render target).
    sw_rgba: Vec<u8>,
    sw_w: u32,
    sw_h: u32,
    /// First frame must render even if dirty flag races.
    force_render: bool,
}

// Client API: one thread at a time per handle; UI/player tick owns it.
unsafe impl Send for LibMpv {}

impl LibMpv {
    pub fn create() -> Result<Self> {
        debug!("LibMpv::create");
        let ctx = unsafe { mpv_create() };
        if ctx.is_null() {
            error!("mpv_create returned null");
            return Err(PlayerError::Backend(
                "mpv_create failed (mémoire / LC_NUMERIC)".into(),
            ));
        }
        info!("LibMpv handle created");
        Ok(Self {
            ctx,
            render: ptr::null_mut(),
            frame_dirty: Arc::new(AtomicBool::new(false)),
            sw_rgba: Vec::new(),
            sw_w: 0,
            sw_h: 0,
            force_render: true,
        })
    }

    pub fn set_option(&self, name: &str, value: &str) -> Result<()> {
        let c_name = CString::new(name).map_err(|e| PlayerError::Backend(e.to_string()))?;
        let c_value = CString::new(value).map_err(|e| PlayerError::Backend(e.to_string()))?;
        let code = unsafe { mpv_set_option_string(self.ctx, c_name.as_ptr(), c_value.as_ptr()) };
        if code >= 0 {
            return Ok(());
        }
        let msg = unsafe {
            let p = mpv_error_string(code);
            if p.is_null() {
                format!("libmpv error {code}")
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        // Soft callers use debug; hard callers surface this string.
        debug!(%name, %value, code, %msg, "libmpv set_option failed");
        Err(PlayerError::Backend(format!(
            "libmpv option {name}={value}: {msg}"
        )))
    }

    pub fn initialize(&self) -> Result<()> {
        debug!("LibMpv::initialize");
        mpv_err(unsafe { mpv_initialize(self.ctx) })?;
        info!("LibMpv initialized");
        Ok(())
    }

    /// Create software render context so video is drawn into our buffer (embedded UI).
    pub fn init_sw_render(&mut self) -> Result<()> {
        if !self.render.is_null() {
            return Ok(());
        }
        let api = CString::new("sw").unwrap();
        let mut params = [
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_API_TYPE,
                data: api.as_ptr() as *mut c_void,
            },
            MpvRenderParam {
                type_: MPV_RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];
        let mut render = ptr::null_mut();
        mpv_err(unsafe {
            mpv_render_context_create(&mut render, self.ctx, params.as_mut_ptr())
        })?;
        if render.is_null() {
            return Err(PlayerError::Backend("mpv_render_context_create null".into()));
        }
        self.render = render;
        let cb_ptr = Arc::as_ptr(&self.frame_dirty) as *mut c_void;
        unsafe {
            mpv_render_context_set_update_callback(self.render, Some(render_update_cb), cb_ptr);
        }
        self.frame_dirty.store(true, Ordering::Release);
        self.force_render = true;
        info!("LibMpv software render context ready");
        Ok(())
    }

    pub fn set_property(&self, name: &str, value: &str) -> Result<()> {
        let name = CString::new(name).map_err(|e| PlayerError::Backend(e.to_string()))?;
        let value = CString::new(value).map_err(|e| PlayerError::Backend(e.to_string()))?;
        mpv_err(unsafe { mpv_set_property_string(self.ctx, name.as_ptr(), value.as_ptr()) })
    }

    pub fn command(&self, args: &[&str]) -> Result<()> {
        let cmd = args.first().copied().unwrap_or("");
        debug!(%cmd, argc = args.len(), "LibMpv::command");
        let c_args: Result<Vec<CString>> = args
            .iter()
            .map(|s| CString::new(*s).map_err(|e| PlayerError::Backend(e.to_string())))
            .collect();
        let c_args = c_args?;
        let mut ptrs: Vec<*const c_char> = c_args.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(ptr::null());
        mpv_err(unsafe { mpv_command(self.ctx, ptrs.as_ptr()) })
    }

    pub fn get_property_string(&self, name: &str) -> Option<String> {
        let name = CString::new(name).ok()?;
        let p = unsafe { mpv_get_property_string(self.ctx, name.as_ptr()) };
        if p.is_null() {
            return None;
        }
        let s = unsafe { CStr::from_ptr(p) }
            .to_string_lossy()
            .into_owned();
        unsafe { mpv_free(p as *mut c_void) };
        Some(s)
    }

    pub fn get_property_f64(&self, name: &str) -> Option<f64> {
        self.get_property_string(name)?.parse().ok()
    }

    pub fn frame_needs_redraw(&self) -> bool {
        self.force_render || self.frame_dirty.load(Ordering::Acquire)
    }

    /// Render current video into tightly packed RGBA (`w * h * 4`).
    /// Returns `None` when there is no new frame (caller should keep the last handle).
    pub fn render_sw_rgba(&mut self, w: u32, h: u32) -> Option<Vec<u8>> {
        if self.render.is_null() || w < 2 || h < 2 {
            return None;
        }
        // Even dims — required by some SW paths; allow full UHD for RTX / 4K displays.
        let w = (w.min(3840) & !1).max(2);
        let h = (h.min(2160) & !1).max(2);

        let flags = unsafe { mpv_render_context_update(self.render) };
        let dirty = self
            .frame_dirty
            .swap(false, Ordering::AcqRel)
            || self.force_render
            || (flags & MPV_RENDER_UPDATE_FRAME) != 0
            || self.sw_w != w
            || self.sw_h != h;
        if !dirty {
            return None;
        }

        let need = (w as usize).saturating_mul(h as usize).saturating_mul(4);
        if self.sw_rgba.len() != need {
            self.sw_rgba.resize(need, 0);
        }

        let mut size = [w as c_int, h as c_int];
        let mut stride = (w as usize) * 4;
        // Prefer packed rgba (no rgb0→RGBA permute). Fall back to rgb0 if needed.
        let fmt_rgba = CString::new("rgba").ok()?;
        let fmt_rgb0 = CString::new("rgb0").ok()?;

        let mut try_render = |fmt: &CString, buf: &mut [u8], stride: &mut usize| -> c_int {
            let mut params = [
                MpvRenderParam {
                    type_: MPV_RENDER_PARAM_SW_SIZE,
                    data: size.as_mut_ptr() as *mut c_void,
                },
                MpvRenderParam {
                    type_: MPV_RENDER_PARAM_SW_FORMAT,
                    data: fmt.as_ptr() as *mut c_void,
                },
                MpvRenderParam {
                    type_: MPV_RENDER_PARAM_SW_STRIDE,
                    data: (stride as *mut usize) as *mut c_void,
                },
                MpvRenderParam {
                    type_: MPV_RENDER_PARAM_SW_POINTER,
                    data: buf.as_mut_ptr() as *mut c_void,
                },
                MpvRenderParam {
                    type_: MPV_RENDER_PARAM_INVALID,
                    data: ptr::null_mut(),
                },
            ];
            unsafe { mpv_render_context_render(self.render, params.as_mut_ptr()) }
        };

        let code = try_render(&fmt_rgba, &mut self.sw_rgba, &mut stride);
        if code < 0 {
            // rgb0: 4 bytes/pixel with unused alpha — write into a scratch then pack.
            let pad_stride = ((w as usize * 4 + 63) / 64) * 64;
            let mut scratch = vec![0u8; pad_stride * h as usize];
            let mut st = pad_stride;
            if try_render(&fmt_rgb0, &mut scratch, &mut st) < 0 {
                self.frame_dirty.store(true, Ordering::Release);
                return None;
            }
            for y in 0..h as usize {
                let src = &scratch[y * st..y * st + w as usize * 4];
                let dst = &mut self.sw_rgba[y * w as usize * 4..(y + 1) * w as usize * 4];
                for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                    d[0] = s[0];
                    d[1] = s[1];
                    d[2] = s[2];
                    d[3] = 255;
                }
            }
        }

        self.sw_w = w;
        self.sw_h = h;
        self.force_render = false;
        // One memcpy into a fresh Vec owned by iced's ImageHandle.
        Some(self.sw_rgba.clone())
    }

    fn free_render(&mut self) {
        if !self.render.is_null() {
            debug!("LibMpv free render context");
            unsafe {
                mpv_render_context_set_update_callback(self.render, None, ptr::null_mut());
                mpv_render_context_free(self.render);
            }
            self.render = ptr::null_mut();
        }
    }

    /// Safe teardown — free render context before destroying the handle.
    pub fn shutdown(mut self) {
        debug!("LibMpv::shutdown");
        self.free_render();
        let _ = self.command(&["stop"]);
        let _ = self.set_property("pause", "yes");
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
        while std::time::Instant::now() < deadline {
            let ev = unsafe { mpv_wait_event(self.ctx, 0.05) };
            if ev.is_null() {
                break;
            }
            let id = unsafe { (*ev).event_id };
            if id == MPV_EVENT_NONE || id == MPV_EVENT_SHUTDOWN {
                break;
            }
        }
        self.ctx_destroy();
    }

    fn ctx_destroy(&mut self) {
        self.free_render();
        if !self.ctx.is_null() {
            debug!("LibMpv terminate_destroy");
            unsafe { mpv_terminate_destroy(self.ctx) };
            self.ctx = ptr::null_mut();
        }
    }
}

impl Drop for LibMpv {
    fn drop(&mut self) {
        self.ctx_destroy();
    }
}
