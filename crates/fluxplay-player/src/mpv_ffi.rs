//! Minimal libmpv client bindings (no pkg-config, controlled static/shared link via build.rs).

#![allow(non_camel_case_types, dead_code)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;

use tracing::{debug, error, info, trace, warn};

use crate::{PlayerError, Result};

#[repr(C)]
pub struct mpv_handle {
    _opaque: [u8; 0],
}

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

/// In-process libmpv player handle.
pub struct LibMpv {
    ctx: *mut mpv_handle,
}

// libmpv documents client API as usable from one thread at a time per handle;
// we only touch it from the UI/player thread.
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
        Ok(Self { ctx })
    }

    pub fn set_option(&self, name: &str, value: &str) -> Result<()> {
        trace!(%name, "LibMpv::set_option");
        let name = CString::new(name).map_err(|e| PlayerError::Backend(e.to_string()))?;
        let value = CString::new(value).map_err(|e| PlayerError::Backend(e.to_string()))?;
        mpv_err(unsafe { mpv_set_option_string(self.ctx, name.as_ptr(), value.as_ptr()) })
    }

    pub fn initialize(&self) -> Result<()> {
        debug!("LibMpv::initialize");
        mpv_err(unsafe { mpv_initialize(self.ctx) })?;
        info!("LibMpv initialized");
        Ok(())
    }

    pub fn set_property(&self, name: &str, value: &str) -> Result<()> {
        trace!(%name, "LibMpv::set_property");
        let name = CString::new(name).map_err(|e| PlayerError::Backend(e.to_string()))?;
        let value = CString::new(value).map_err(|e| PlayerError::Backend(e.to_string()))?;
        mpv_err(unsafe { mpv_set_property_string(self.ctx, name.as_ptr(), value.as_ptr()) })
    }

    pub fn command(&self, args: &[&str]) -> Result<()> {
        // Avoid logging full stream URLs that may carry tokens in loadfile args.
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
}

impl Drop for LibMpv {
    fn drop(&mut self) {
        if !self.ctx.is_null() {
            debug!("LibMpv::drop terminate_destroy");
            unsafe { mpv_terminate_destroy(self.ctx) };
            self.ctx = ptr::null_mut();
        }
    }
}
