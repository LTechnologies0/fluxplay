//! Android JNI bridge: SAF inbox, system insets, OS PiP, keep-screen-on,
//! theme/TV probes, audio focus, immersive chrome, finish Activity.
//!
//! `ndk_context::android_context().context()` is often the **Application**, not
//! `FluxPlayNativeActivity`. Never use `get_object_class(context)` for static
//! helpers — that yields `Landroid/app/Application;` and ART aborts when a
//! pending `getWindow()` / `NoSuchMethodError` collides with the next lookup.
//! Resolve `app.fluxplay.android.FluxPlayNativeActivity` via the app ClassLoader.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use jni::objects::{GlobalRef, JClass, JObject, JValue};
use jni::JNIEnv;
use jni::JavaVM;
use serde::Deserialize;
use tracing::{debug, info, warn};

static SAF_PENDING: AtomicBool = AtomicBool::new(false);
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
    pub mime: String,
}

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

fn pip_flag_path() -> Option<PathBuf> {
    files_dir().map(|b| b.join("saf_inbox").join("pip.json"))
}

fn audio_focus_flag_path() -> Option<PathBuf> {
    files_dir().map(|b| b.join("saf_inbox").join("audio_focus.json"))
}

#[derive(Debug, Clone, Deserialize)]
struct AudioFocusFlag {
    held: bool,
}

/// True when Java reports we still hold audio focus (false after LOSS*).
pub fn poll_audio_focus_held() -> Option<bool> {
    let path = audio_focus_flag_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str::<AudioFocusFlag>(&text)
        .ok()
        .map(|f| f.held)
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

/// Sync `pip_mode` from Java (`isInPip` + optional pip.json).
pub fn poll_pip_mode() -> bool {
    if let Some(path) = pip_flag_path() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(flag) = serde_json::from_str::<PipFlag>(&text) {
                return flag.in_pip;
            }
        }
    }
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
            (
                buf[0] as f32 / scale,
                buf[1] as f32 / scale,
                buf[2] as f32 / scale,
                buf[3] as f32 / scale,
            )
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
        (0.0, 0.0, 0.0, 24.0)
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

pub fn set_keep_screen_on(enable: bool) {
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
