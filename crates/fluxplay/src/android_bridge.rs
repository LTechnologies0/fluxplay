//! Android JNI bridge: SAF inbox, system insets, OS PiP, keep-screen-on,
//! theme/TV probes, audio focus, immersive chrome, finish Activity.
//!
//! `ndk_context::android_context().context()` is often the **Application**, not
//! `FluxPlayNativeActivity`. Never use `get_object_class(context)` for static
//! helpers — that yields `Landroid/app/Application;` and ART aborts when a
//! pending `getWindow()` / `NoSuchMethodError` collides with the next lookup.
//! Resolve `app.fluxplay.android.FluxPlayNativeActivity` via the app ClassLoader.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use jni::objects::{GlobalRef, JClass, JObject, JValue};
use jni::JNIEnv;
use jni::JavaVM;
use serde::Deserialize;
use tracing::{debug, info, warn};

static SAF_PENDING: AtomicBool = AtomicBool::new(false);
/// Cached chrome inset (f32 bits) — skip JNI when unchanged.
static LAST_CHROME_INSET_BITS: AtomicU32 = AtomicU32::new(u32::MAX);
/// Cached `FluxPlayNativeActivity` class — avoids per-tick `loadClass` local refs
/// on the permanently attached `android_main` thread (local-ref table overflow).
static FP_ACTIVITY_CLASS: OnceLock<GlobalRef> = OnceLock::new();

/// JNI slash name for the NativeActivity subclass that owns our static helpers.
const FLUXPLAY_ACTIVITY_JNI: &str = "app/fluxplay/android/FluxPlayNativeActivity";
const FLUXPLAY_ACTIVITY_DOT: &str = "app.fluxplay.android.FluxPlayNativeActivity";

#[derive(Debug, Clone, Deserialize)]
pub struct SafInbox {
    pub status: String,
    pub name: String,
    pub path: String,
    // Written by the Java SAF callback for parity with the meta schema; Rust does not read it yet.
    #[allow(dead_code)]
    pub mime: String,
}

// Legacy pip.json schema — PiP state now comes from the live JNI poll (`poll_pip_mode`).
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct PipFlag {
    in_pip: bool,
}

fn vm_activity() -> Option<(JavaVM, jni::sys::jobject)> {
    let ctx = ndk_context::android_context();
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
    Some((vm, ctx.context() as jni::sys::jobject))
}

fn clear_ex(env: &mut JNIEnv<'_>) {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
}

/// Resolve (and cache) `FluxPlayNativeActivity` class as a GlobalRef.
fn fp_activity_global<'a>(
    env: &mut JNIEnv<'a>,
    context: jni::sys::jobject,
) -> Option<&'static GlobalRef> {
    if let Some(g) = FP_ACTIVITY_CLASS.get() {
        return Some(g);
    }
    let local = fluxplay_activity_class_local(env, context)?;
    let global = env.new_global_ref(local).ok()?;
    let _ = FP_ACTIVITY_CLASS.set(global);
    FP_ACTIVITY_CLASS.get()
}

    fn fp_class_obj<'a>(env: &mut JNIEnv<'a>, context: jni::sys::jobject) -> Option<&'static GlobalRef> {
    fp_activity_global(env, context)
}

/// Load `FluxPlayNativeActivity` (where `requestAudioFocus`, SAF, PiP, … live).
fn fluxplay_activity_class_local<'a>(
    env: &mut JNIEnv<'a>,
    context: jni::sys::jobject,
) -> Option<JClass<'a>> {
    // Always clear first — a stale pending exception turns the next Get*MethodID
    // into an ART abort (`AssertNoPendingExceptionForNewException`).
    clear_ex(env);
    if context.is_null() {
        return None;
    }
    let ctx = unsafe { JObject::from_raw(context) };

    // Preferred: Application/Activity ClassLoader (works from any attached thread).
    if let Ok(cl_v) = env.call_method(&ctx, "getClassLoader", "()Ljava/lang/ClassLoader;", &[]) {
        if let Ok(cl) = cl_v.l() {
            if let Ok(name) = env.new_string(FLUXPLAY_ACTIVITY_DOT) {
                match env.call_method(
                    &cl,
                    "loadClass",
                    "(Ljava/lang/String;)Ljava/lang/Class;",
                    &[JValue::Object(&name)],
                ) {
                    Ok(v) => {
                        if let Ok(obj) = v.l() {
                            return Some(JClass::from(obj));
                        }
                    }
                    Err(e) => {
                        clear_ex(env);
                        debug!(error = %e, "ClassLoader.loadClass(FluxPlayNativeActivity) failed");
                    }
                }
            } else {
                clear_ex(env);
            }
        } else {
            clear_ex(env);
        }
    } else {
        clear_ex(env);
    }

    // Fallback: FindClass (often fails off the main thread / boot ClassLoader).
    match env.find_class(FLUXPLAY_ACTIVITY_JNI) {
        Ok(cls) => Some(cls),
        Err(e) => {
            clear_ex(env);
            warn!(error = %e, "FindClass(FluxPlayNativeActivity) failed");
            None
        }
    }
}

fn call_static_bool(name: &str) -> bool {
    let Some((vm, activity)) = vm_activity() else {
        return false;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return false;
    };
    let Some(cls) = fp_class_obj(&mut env, activity) else {
        return false;
    };
    match env.call_static_method(cls, name, "()Z", &[]) {
        Ok(v) => v.z().unwrap_or(false),
        Err(e) => {
            clear_ex(&mut env);
            debug!(error = %e, %name, "static bool JNI failed");
            false
        }
    }
}

fn call_static_void_bool(name: &str, arg: bool) {
    let Some((vm, activity)) = vm_activity() else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let Some(cls) = fp_class_obj(&mut env, activity) else {
        return;
    };
    if env
        .call_static_method(cls, name, "(Z)V", &[JValue::Bool(u8::from(arg))])
        .is_err()
    {
        clear_ex(&mut env);
        warn!(target: "fluxplay::android", %name, "static void(bool) JNI failed");
    } else {
        debug!(target: "fluxplay::android", %name, arg, "static void(bool) ok");
    }
}

fn call_static_void(name: &str) {
    let Some((vm, activity)) = vm_activity() else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let Some(cls) = fp_class_obj(&mut env, activity) else {
        return;
    };
    if env.call_static_method(cls, name, "()V", &[]).is_err() {
        clear_ex(&mut env);
    }
}

fn files_dir() -> Option<PathBuf> {
    iced::android::ANDROID_APP
        .get()
        .and_then(|app| app.internal_data_path().map(|p| p.to_path_buf()))
}

fn saf_meta_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(base) = files_dir() {
        out.push(base.join("saf_inbox").join("latest.json"));
    }
    out
}

// Legacy pip.json path — kept next to the other flag paths; no reader since the JNI poll.
#[allow(dead_code)]
fn pip_flag_path() -> Option<PathBuf> {
    files_dir().map(|b| b.join("saf_inbox").join("pip.json"))
}

fn audio_focus_flag_path() -> Option<PathBuf> {
    files_dir().map(|b| b.join("saf_inbox").join("audio_focus.json"))
}

fn device_caps_path() -> Option<PathBuf> {
    files_dir().map(|b| b.join("saf_inbox").join("device_caps.json"))
}

fn surface_state_path() -> Option<PathBuf> {
    files_dir().map(|b| b.join("saf_inbox").join("surface_state.json"))
}

/// Cached GlobalRef for the video Surface jobject (mpv wid lifetime).
static VIDEO_SURFACE_REF: OnceLock<Mutex<Option<GlobalRef>>> = OnceLock::new();

fn video_surface_slot() -> &'static Mutex<Option<GlobalRef>> {
    VIDEO_SURFACE_REF.get_or_init(|| Mutex::new(None))
}

#[derive(Debug, Clone, Deserialize)]
struct DeviceCapsJson {
    #[serde(default)]
    soc: String,
    #[serde(default)]
    board: String,
    #[serde(default)]
    hardware: String,
    #[serde(default)]
    manufacturer: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    gl_renderer: String,
    #[serde(default)]
    cores: u32,
    #[serde(default)]
    refresh_hz: u32,
    #[serde(default)]
    refresh_modes: Vec<f32>,
    #[serde(default)]
    hdr_types: Vec<i32>,
    #[serde(default)]
    hdr_capable: bool,
    #[serde(default)]
    mediacodec_4k: bool,
    #[serde(default)]
    mediacodec_hdr: bool,
    #[serde(default)]
    mediacodec_video: bool,
    #[serde(default)]
    mediacodec_max_w: u32,
    #[serde(default)]
    mediacodec_max_h: u32,
    #[serde(default)]
    sdk_int: u32,
    #[serde(default)]
    panel_oled: bool,
    #[serde(default)]
    hdr_labels: Vec<String>,
    #[serde(default)]
    surface_ready: bool,
    #[serde(default)]
    surface_w: u32,
    #[serde(default)]
    surface_h: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct SurfaceStateJson {
    #[serde(default)]
    ready: bool,
    #[serde(default)]
    w: u32,
    #[serde(default)]
    h: u32,
    #[serde(default)]
    gen: u32,
}

/// Full device caps for quality matrix (Phases B–C).
pub fn poll_android_device_caps() -> Option<fluxplay_player::AndroidDeviceCaps> {
    let path = device_caps_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let caps: DeviceCapsJson = serde_json::from_str(&text).ok()?;
    Some(fluxplay_player::AndroidDeviceCaps {
        soc: caps.soc,
        board: caps.board,
        hardware: caps.hardware,
        manufacturer: caps.manufacturer,
        model: caps.model,
        gl_renderer: caps.gl_renderer,
        cores: caps.cores,
        refresh_hz: caps.refresh_hz,
        refresh_modes: caps.refresh_modes,
        hdr_types: caps.hdr_types,
        hdr_capable: caps.hdr_capable,
        mediacodec_4k: caps.mediacodec_4k,
        mediacodec_hdr: caps.mediacodec_hdr,
        mediacodec_video: caps.mediacodec_video,
        mediacodec_max_w: caps.mediacodec_max_w,
        mediacodec_max_h: caps.mediacodec_max_h,
        sdk_int: caps.sdk_int,
        panel_oled: caps.panel_oled,
        hdr_labels: caps.hdr_labels,
        surface_ready: caps.surface_ready,
        surface_w: caps.surface_w,
        surface_h: caps.surface_h,
    })
}

/// Best-effort SoC / GPU label from Java `device_caps.json` (written at Activity start).
pub fn poll_device_gpu() -> Option<(String, fluxplay_core::models::GpuTier)> {
    let caps = poll_android_device_caps()?;
    let mut parts: Vec<String> = Vec::new();
    for s in [
        caps.manufacturer.as_str(),
        caps.model.as_str(),
        caps.soc.as_str(),
        caps.board.as_str(),
        caps.hardware.as_str(),
    ] {
        let t = s.trim();
        if !t.is_empty() && !parts.iter().any(|p| p.eq_ignore_ascii_case(t)) {
            parts.push(t.to_string());
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some((parts.join(" · "), fluxplay_core::models::GpuTier::Integrated))
}

pub fn set_video_surface_visible(visible: bool) {
    ensure_ffmpeg_java_vm();
    call_static_void_bool("setVideoSurfaceVisible", visible);
}

/// Register Android JavaVM with FFmpeg once — required for MediaCodec Surface decode.
fn ensure_ffmpeg_java_vm() {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.load(Ordering::Acquire) {
        return;
    }
    let vm_ptr = ndk_context::android_context().vm() as *mut std::ffi::c_void;
    if vm_ptr.is_null() {
        warn!("ensure_ffmpeg_java_vm: null JavaVM");
        return;
    }
    if fluxplay_player::register_android_java_vm(vm_ptr) {
        DONE.store(true, Ordering::Release);
    }
}

pub fn set_window_punch_through(enable: bool) {
    call_static_void_bool("setWindowPunchThrough", enable);
}

/// Refresh insets / caps / Surface session after rotate or resume.
pub fn stabilize_android_session() {
    call_static_void("stabilizeAndroidSession");
}

/// Freeze SurfaceView buffer size after wid is acquired (MediaCodec safety).
pub fn lock_video_surface_size() {
    call_static_void("lockVideoSurfaceSize");
}

pub fn surface_generation() -> u32 {
    let Some((vm, activity)) = vm_activity() else {
        return 0;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return 0;
    };
    let Some(cls) = fp_class_obj(&mut env, activity) else {
        return 0;
    };
    match env.call_static_method(cls, "getSurfaceGeneration", "()I", &[]) {
        Ok(v) => v.i().unwrap_or(0).max(0) as u32,
        Err(_) => {
            clear_ex(&mut env);
            0
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SurfaceState {
    pub ready: bool,
    pub w: u32,
    pub h: u32,
    pub gen: u32,
}

pub fn poll_surface_state() -> SurfaceState {
    // Prefer JNI — avoid reading surface_state.json every PlayerTick (I/O stutter).
    let mut st = SurfaceState {
        ready: call_static_bool("isVideoSurfaceReady"),
        gen: surface_generation(),
        ..SurfaceState::default()
    };
    if let Some((w, h)) = video_surface_size() {
        st.w = w;
        st.h = h;
        return st;
    }
    // File fallback only when JNI size is missing (UI-thread race).
    if let Some(path) = surface_state_path() {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(j) = serde_json::from_str::<SurfaceStateJson>(&text) {
                st.ready = j.ready || st.ready;
                st.w = j.w;
                st.h = j.h;
                if j.gen > 0 {
                    st.gen = j.gen;
                }
            }
        }
    }
    st
}

pub fn is_video_surface_ready() -> bool {
    // Trust live JNI only. Stale surface_state.json ready=true after GONE/destroy
    // made bind "succeed" the wait then fail acquire in ~1ms (Pixel Soft black path).
    if vm_activity().is_some() {
        return call_static_bool("isVideoSurfaceReady");
    }
    let path = match surface_state_path() {
        Some(p) => p,
        None => return false,
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    serde_json::from_str::<SurfaceStateJson>(&text)
        .ok()
        .map(|s| s.ready)
        .unwrap_or(false)
}

pub fn video_surface_size() -> Option<(u32, u32)> {
    // Prefer live JNI ints — JSON can lag the UI thread after surfaceCreated.
    if let Some((vm, activity)) = vm_activity() {
        if let Ok(mut env) = vm.attach_current_thread() {
            if let Some(cls) = fp_class_obj(&mut env, activity) {
                let read = env.with_local_frame(8, |env| -> Result<(u32, u32), jni::errors::Error> {
                    let arr = env.call_static_method(cls, "videoSurfaceSizePx", "()[I", &[])?;
                    let obj = arr.l()?;
                    let jint_arr: jni::objects::JIntArray =
                        unsafe { jni::objects::JIntArray::from_raw(obj.into_raw()) };
                    let len = env.get_array_length(&jint_arr)?;
                    if len < 2 {
                        return Err(jni::errors::Error::JavaException);
                    }
                    let mut buf = [0i32; 2];
                    env.get_int_array_region(&jint_arr, 0, &mut buf)?;
                    Ok((buf[0].max(0) as u32, buf[1].max(0) as u32))
                });
                if let Ok((w, h)) = read {
                    if w >= 64 && h >= 64 {
                        return Some((w, h));
                    }
                } else {
                    clear_ex(&mut env);
                }
            }
        }
    }
    let path = surface_state_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let st: SurfaceStateJson = serde_json::from_str(&text).ok()?;
    if st.ready && st.w >= 64 && st.h >= 64 {
        Some((st.w, st.h))
    } else {
        None
    }
}

/// Acquire Surface jobject as mpv `wid` (GlobalRef kept alive until release).
pub fn acquire_video_surface_wid() -> Option<i64> {
    let (vm, activity) = vm_activity()?;
    let mut env = vm.attach_current_thread().ok()?;
    let cls = fp_class_obj(&mut env, activity)?;
    let surface_v = env
        .call_static_method(cls, "getVideoSurface", "()Landroid/view/Surface;", &[])
        .ok()?;
    let obj = surface_v.l().ok()?;
    if obj.is_null() {
        return None;
    }
    let global = env.new_global_ref(&obj).ok()?;
    // jobject pointer value is what mpv's ANativeWindow_fromSurface expects as wid.
    let wid = global.as_raw() as isize as i64;
    if let Ok(mut slot) = video_surface_slot().lock() {
        *slot = Some(global);
    }
    info!(wid, "android video Surface wid acquired");
    Some(wid)
}

pub fn release_video_surface_wid() {
    if let Ok(mut slot) = video_surface_slot().lock() {
        *slot = None;
    }
    // End Surface session — stop reattach loop; keep punch-through off.
    // Caller must have already run NativePlayer::detach_android_surface (vo=null, wid=0).
    set_video_surface_visible(false);
    set_window_punch_through(false);
}

pub fn set_video_frame_rate(fps: f32) {
    // Cache: Surface.setFrameRate re-posted ~10×/s otherwise.
    static LAST: AtomicU32 = AtomicU32::new(0);
    let bits = fps.to_bits();
    if LAST.swap(bits, Ordering::Relaxed) == bits {
        return;
    }
    let Some((vm, activity)) = vm_activity() else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let Some(cls) = fp_class_obj(&mut env, activity) else {
        return;
    };
    if env
        .call_static_method(cls, "setVideoFrameRate", "(F)V", &[JValue::Float(fps)])
        .is_err()
    {
        clear_ex(&mut env);
    }
}

/// Match MediaCodec buffer to video size — stops SurfaceFlinger stretch/squash.
/// Push video geometry: `bw/bh` = decoded buffer size, `dw/dh` = display
/// (SAR-corrected) size. Java cover-fits the buffer onto the screen via a
/// SurfaceControl transform — the SurfaceView itself is never relayouted.
pub fn set_video_buffer_size(bw: u32, bh: u32, dw: u32, dh: u32) {
    // Cache: re-pushing identical dims re-runs the transform needlessly.
    static LAST: AtomicU32 = AtomicU32::new(0);
    static LASTH: AtomicU32 = AtomicU32::new(0);
    static LASTDW: AtomicU32 = AtomicU32::new(0);
    static LASTDH: AtomicU32 = AtomicU32::new(0);
    if LAST.swap(bw, Ordering::Relaxed) == bw
        && LASTH.swap(bh, Ordering::Relaxed) == bh
        && LASTDW.swap(dw, Ordering::Relaxed) == dw
        && LASTDH.swap(dh, Ordering::Relaxed) == dh
    {
        return;
    }
    let Some((vm, activity)) = vm_activity() else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let Some(cls) = fp_class_obj(&mut env, activity) else {
        log::warn!("set_video_buffer_size: no class");
        return;
    };
    if env
        .call_static_method(
            cls,
            "setVideoBufferSize",
            "(IIII)V",
            &[
                JValue::Int(bw as i32),
                JValue::Int(bh as i32),
                JValue::Int(dw as i32),
                JValue::Int(dh as i32),
            ],
        )
        .is_err()
    {
        log::warn!("set_video_buffer_size: JNI call failed");
        clear_ex(&mut env);
    }
}

pub fn set_hdr_color_mode(hdr: bool) {
    set_display_color_mode(if hdr { "hdr" } else { "default" });
}

/// Force sensor-landscape while a video plays (portrait UX dropped); restore on close.
pub fn set_force_landscape(force: bool) {
    call_static_void_bool("setForceLandscape", force);
}

/// Android window color mode: `default` | `wide` | `hdr`.
pub fn set_display_color_mode(mode: &str) {
    let Some((vm, activity)) = vm_activity() else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let Some(cls) = fp_class_obj(&mut env, activity) else {
        return;
    };
    let Ok(jmode) = env.new_string(mode) else {
        clear_ex(&mut env);
        return;
    };
    if env
        .call_static_method(
            cls,
            "setDisplayColorMode",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&jmode)],
        )
        .is_err()
    {
        clear_ex(&mut env);
    }
}

/// Prepare SurfaceView present (mpv-android style): show view, return wid if ready.
/// Non-blocking — caller retries via `maintain_android_surface_session`.
pub fn prepare_surface_present() -> Option<(i64, (u32, u32))> {
    ensure_ffmpeg_java_vm();
    // Fast path: Surface already live — do not poke visibility/reattach every tick.
    if is_video_surface_ready() {
        let wh = video_surface_size().or_else(|| {
            let st = poll_surface_state();
            (st.ready && st.w >= 64 && st.h >= 64).then_some((st.w, st.h))
        })?;
        let wid = acquire_video_surface_wid()?;
        return Some((wid, wh));
    }
    set_window_punch_through(false);
    // SurfaceView above iced GLES; chrome inset keeps dock tappable.
    set_video_surface_z_on_top(true);
    set_video_surface_visible(true);
    if !is_video_surface_ready() {
        return None;
    }
    let wh = video_surface_size().or_else(|| {
        let st = poll_surface_state();
        (st.ready && st.w >= 64 && st.h >= 64).then_some((st.w, st.h))
    })?;
    let wid = acquire_video_surface_wid()?;
    info!(wid, ?wh, "android SurfaceView present ready");
    Some((wid, wh))
}

/// Leave bottom chrome (dp) uncovered so iced transport stays tappable above Z-order video.
pub fn layout_video_surface_chrome_inset_dp(bottom_dp: f32) {
    let next = bottom_dp.max(0.0);
    let bits = next.to_bits();
    let prev_bits = LAST_CHROME_INSET_BITS.load(Ordering::Relaxed);
    if prev_bits != u32::MAX {
        let prev = f32::from_bits(prev_bits);
        if (prev - next).abs() < 0.25 {
            return;
        }
    }
    LAST_CHROME_INSET_BITS.store(bits, Ordering::Relaxed);
    let Some((vm, activity)) = vm_activity() else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let Some(cls) = fp_class_obj(&mut env, activity) else {
        return;
    };
    if env
        .call_static_method(
            cls,
            "layoutVideoSurfaceChromeInsetDp",
            "(F)V",
            &[JValue::Float(next)],
        )
        .is_err()
    {
        clear_ex(&mut env);
    }
}

pub fn set_video_surface_z_on_top(on_top: bool) {
    call_static_void_bool("setVideoSurfaceZOrderOnTop", on_top);
}

/// Select present mode from device caps + Surface readiness (Phase C).
// Phase-C helper retained for the upcoming present-mode wiring; no caller yet.
#[allow(dead_code)]
pub fn select_android_present_mode() -> fluxplay_player::AndroidPresentMode {
    let mut caps = poll_android_device_caps().unwrap_or_default();
    // Refresh surface_ready from live probe.
    caps.surface_ready = is_video_surface_ready() || caps.surface_ready;
    caps.select_present_mode()
}

#[derive(Debug, Clone, Deserialize)]
struct AudioFocusFlag {
    held: bool,
}

/// True when Java reports we still hold audio focus (false after LOSS*).
/// Cached ~250 ms: this was a file read + JSON parse up to 3× per UI tick.
pub fn poll_audio_focus_held() -> Option<bool> {
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};
    // None until first successful poll — Instant::now is not const.
    static CACHE: OnceLock<std::sync::Mutex<(Instant, Option<bool>)>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new((Instant::now(), None)));
    {
        let c = cache.lock().ok()?;
        if c.0.elapsed() < Duration::from_millis(250) && c.1.is_some() {
            return c.1;
        }
    }
    let path = audio_focus_flag_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let v = serde_json::from_str::<AudioFocusFlag>(&text)
        .ok()
        .map(|f| f.held);
    if let Ok(mut c) = cache.lock() {
        *c = (Instant::now(), v);
    }
    v
}

/// Launch SAF OPEN_DOCUMENT / CREATE_DOCUMENT via FluxPlayNativeActivity.
pub fn start_saf_open(mime: &str) {
    SAF_PENDING.store(true, Ordering::SeqCst);
    for p in saf_meta_candidates() {
        let _ = std::fs::remove_file(p);
    }
    call_start_saf("open", mime, "");
}

pub fn start_saf_create(mime: &str, source_path: &str) {
    SAF_PENDING.store(true, Ordering::SeqCst);
    for p in saf_meta_candidates() {
        let _ = std::fs::remove_file(p);
    }
    call_start_saf("create", mime, source_path);
}

fn call_start_saf(mode: &str, mime: &str, source: &str) {
    let Some((vm, activity)) = vm_activity() else {
        warn!("start_saf: no android context");
        SAF_PENDING.store(false, Ordering::SeqCst);
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        SAF_PENDING.store(false, Ordering::SeqCst);
        return;
    };
    let Some(cls) = fp_class_obj(&mut env, activity) else {
        warn!("FluxPlayNativeActivity class missing — run scripts/build-android-apk.sh");
        SAF_PENDING.store(false, Ordering::SeqCst);
        return;
    };
    let Ok(jmode) = env.new_string(mode) else {
        clear_ex(&mut env);
        SAF_PENDING.store(false, Ordering::SeqCst);
        return;
    };
    let Ok(jmime) = env.new_string(mime) else {
        clear_ex(&mut env);
        SAF_PENDING.store(false, Ordering::SeqCst);
        return;
    };
    let Ok(jsrc) = env.new_string(source) else {
        clear_ex(&mut env);
        SAF_PENDING.store(false, Ordering::SeqCst);
        return;
    };
    match env.call_static_method(
        cls,
        "startSaf",
        "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Z",
        &[
            JValue::Object(&jmode),
            JValue::Object(&jmime),
            JValue::Object(&jsrc),
        ],
    ) {
        Ok(v) => {
            let ok = v.z().unwrap_or(false);
            if ok {
                info!(%mode, %mime, "SAF launched");
            } else {
                warn!(%mode, "startSaf returned false (no activity?)");
                SAF_PENDING.store(false, Ordering::SeqCst);
            }
        }
        Err(e) => {
            clear_ex(&mut env);
            warn!(error = %e, "startSaf JNI failed");
            SAF_PENDING.store(false, Ordering::SeqCst);
        }
    }
}

// Public probe kept for diagnostics; `poll_saf_inbox` short-circuits on the same flag.
#[allow(dead_code)]
pub fn saf_is_pending() -> bool {
    SAF_PENDING.load(Ordering::SeqCst)
}

/// Poll inbox written by Java `onActivityResult`. Consumes the meta file.
pub fn poll_saf_inbox() -> Option<SafInbox> {
    if !SAF_PENDING.load(Ordering::SeqCst) {
        return None;
    }
    for meta in saf_meta_candidates() {
        let Ok(text) = std::fs::read_to_string(&meta) else {
            continue;
        };
        let _ = std::fs::remove_file(&meta);
        SAF_PENDING.store(false, Ordering::SeqCst);
        match serde_json::from_str::<SafInbox>(&text) {
            Ok(inbox) => {
                debug!(status = %inbox.status, name = %inbox.name, "SAF inbox");
                return Some(inbox);
            }
            Err(e) => {
                warn!(error = %e, "SAF meta parse");
                return None;
            }
        }
    }
    None
}

/// Sync `pip_mode` from live Java `isInPip` (ignore stale pip.json).
pub fn poll_pip_mode() -> bool {
    call_static_bool("isInPip")
}

pub fn is_night_mode() -> bool {
    call_static_bool("isNightMode")
}

pub fn is_television() -> bool {
    call_static_bool("isTelevision")
}

pub fn request_audio_focus() {
    call_static_void("requestAudioFocus");
}

pub fn abandon_audio_focus() {
    call_static_void("abandonAudioFocus");
}

pub fn set_immersive_mode(enable: bool) {
    call_static_void_bool("setImmersiveMode", enable);
}

/// Ask the Activity to refresh WindowInsets into the JNI cache (UI thread).
pub fn refresh_system_insets() {
    call_static_void("refreshSystemInsets");
}

pub fn finish_activity() {
    call_static_void("finishActivity");
}

/// System bar insets in logical dp: (left, top, right, bottom).
pub fn system_insets_dp() -> (f32, f32, f32, f32) {
    let fallback = if is_television() {
        (0.0, 0.0, 0.0, 0.0)
    } else {
        (0.0, 0.0, 0.0, 24.0)
    };
    let Some((vm, activity)) = vm_activity() else {
        return fallback;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return fallback;
    };
    let Some(cls) = fp_class_obj(&mut env, activity) else {
        return content_rect_insets_dp();
    };
    // Nested attach on android_main never pops locals — frame the JNI call.
    let read = env.with_local_frame(16, |env| -> Result<[i32; 5], jni::errors::Error> {
        let arr = env.call_static_method(cls, "systemInsetsPx", "()[I", &[])?;
        let obj = arr.l()?;
        let jint_arr: jni::objects::JIntArray =
            unsafe { jni::objects::JIntArray::from_raw(obj.into_raw()) };
        let len = env.get_array_length(&jint_arr)?;
        if len < 5 {
            return Err(jni::errors::Error::JavaException);
        }
        let mut buf = [0i32; 5];
        env.get_int_array_region(&jint_arr, 0, &mut buf)?;
        Ok(buf)
    });
    match read {
        Ok(buf) => {
            let dpi = buf[4].max(120) as f32;
            let scale = dpi / 160.0;
            let (l, t, r, b) = (
                buf[0] as f32 / scale,
                buf[1] as f32 / scale,
                buf[2] as f32 / scale,
                buf[3] as f32 / scale,
            );
            // All-zero before first insets dispatch — keep non-TV bottom fallback.
            if l == 0.0 && t == 0.0 && r == 0.0 && b == 0.0 {
                return fallback;
            }
            (l, t, r, b)
        }
        Err(_) => {
            clear_ex(&mut env);
            content_rect_insets_dp()
        }
    }
}

fn content_rect_insets_dp() -> (f32, f32, f32, f32) {
    if is_television() {
        (0.0, 0.0, 0.0, 0.0)
    } else {
        (0.0, 0.0, 0.0, 48.0)
    }
}

pub fn enter_pip(width: i32, height: i32) {
    let Some((vm, activity)) = vm_activity() else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let Some(cls) = fp_class_obj(&mut env, activity) else {
        warn!("enter_pip: activity class missing");
        return;
    };
    if env
        .call_static_method(
            cls,
            "enterPip",
            "(II)V",
            &[JValue::Int(width.max(1)), JValue::Int(height.max(1))],
        )
        .is_err()
    {
        clear_ex(&mut env);
        warn!("enter_pip: JNI call failed");
    }
}

pub fn exit_pip() {
    call_static_void("exitPip");
}

pub fn set_keep_screen_on(enable: bool) {
    // Cache: this is called every PlayerTick (36-48 Hz) — JNI runOnUiThread each time
    // contends with Choreographer. Only cross JNI when the value actually changes.
    static LAST: AtomicU32 = AtomicU32::new(u32::MAX);
    let bits = u32::from(enable);
    if LAST.swap(bits, Ordering::Relaxed) == bits {
        return;
    }
    if let Some((vm, activity)) = vm_activity() {
        if let Ok(mut env) = vm.attach_current_thread() {
            if let Some(cls) = fp_class_obj(&mut env, activity) {
                if env
                    .call_static_method(
                        cls,
                        "setKeepScreenOn",
                        "(Z)V",
                        &[JValue::Bool(u8::from(enable))],
                    )
                    .is_ok()
                {
                    return;
                }
                clear_ex(&mut env);
            }
        }
    }
    crate::android_intent::set_keep_screen_on(enable);
}

/// Resolve Activity filesDir path for last SAF import when meta path empty.
pub fn default_picked_path() -> Option<PathBuf> {
    let base = files_dir()?;
    let imports = base.join("imports");
    if imports.is_dir() {
        let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
        if let Ok(rd) = std::fs::read_dir(&imports) {
            for ent in rd.flatten() {
                let p = ent.path();
                if p.is_file() {
                    let modified = ent
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    if newest.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
                        newest = Some((modified, p));
                    }
                }
            }
        }
        if let Some((_, p)) = newest {
            return Some(p);
        }
    }
    let p = base.join("saf_inbox").join("picked.bin");
    p.is_file().then_some(p)
}
