//! Android Intent / Activity helpers (ACTION_VIEW, keep-screen-on).
//!
//! `ndk_context` may be `Application` — never call `Activity.getWindow()` on it.
//! Prefer `FluxPlayNativeActivity` static helpers (see `android_bridge`).

use jni::objects::{JClass, JObject, JValue};
use jni::JNIEnv;
use jni::JavaVM;
use tracing::{debug, error, info, warn};

const FLUXPLAY_ACTIVITY_JNI: &str = "app/fluxplay/android/FluxPlayNativeActivity";
const FLUXPLAY_ACTIVITY_DOT: &str = "app.fluxplay.android.FluxPlayNativeActivity";

fn clear_ex(env: &mut JNIEnv<'_>) {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
}

fn fluxplay_activity_class<'a>(
    env: &mut JNIEnv<'a>,
    context: jni::sys::jobject,
) -> Option<JClass<'a>> {
    clear_ex(env);
    if context.is_null() {
        return None;
    }
    let ctx = unsafe { JObject::from_raw(context) };
    if let Ok(cl_v) = env.call_method(&ctx, "getClassLoader", "()Ljava/lang/ClassLoader;", &[]) {
        if let Ok(cl) = cl_v.l() {
            if let Ok(name) = env.new_string(FLUXPLAY_ACTIVITY_DOT) {
                if let Ok(v) = env.call_method(
                    &cl,
                    "loadClass",
                    "(Ljava/lang/String;)Ljava/lang/Class;",
                    &[JValue::Object(&name)],
                ) {
                    if let Ok(obj) = v.l() {
                        return Some(JClass::from(obj));
                    }
                }
                clear_ex(env);
            } else {
                clear_ex(env);
            }
        } else {
            clear_ex(env);
        }
    } else {
        clear_ex(env);
    }
    match env.find_class(FLUXPLAY_ACTIVITY_JNI) {
        Ok(cls) => Some(cls),
        Err(_) => {
            clear_ex(env);
            None
        }
    }
}

/// Toggle Activity window keep-screen-on (playback) via FluxPlayNativeActivity.
pub fn set_keep_screen_on(enable: bool) {
    let ctx = ndk_context::android_context();
    let Ok(vm) = (unsafe { JavaVM::from_raw(ctx.vm().cast()) }) else {
        return;
    };
    let context = ctx.context() as jni::sys::jobject;
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let Some(cls) = fluxplay_activity_class(&mut env, context) else {
        warn!("keep_screen_on: FluxPlayNativeActivity class missing");
        return;
    };
    if env
        .call_static_method(
            cls,
            "setKeepScreenOn",
            "(Z)V",
            &[JValue::Bool(u8::from(enable))],
        )
        .is_err()
    {
        clear_ex(&mut env);
        warn!("keep_screen_on: setKeepScreenOn JNI failed");
        return;
    }
    debug!(enable, "keep_screen_on");
}

/// Launch ACTION_VIEW for `url`. When `mime` is `Some`, uses `setDataAndType`.
pub fn open_url(url: &str, mime: Option<&str>) -> Result<(), String> {
    // External players ignore app SOCKS / WireGuard — refuse unless explicitly allowed.
    let tunnel_up = crate::wg_tunnel::tunnel_is_up()
        || fluxplay_providers::socks_proxy().is_some();
    if tunnel_up {
        let allow = std::env::var("FLUXPLAY_ALLOW_CLEARNET_EXTERNAL")
            .map(|v| v == "1")
            .unwrap_or(false);
        if !allow {
            warn!(%url, "external Intent blocked — tunnel/SOCKS active");
            return Err(
                "Lecteur externe ignore le tunnel SOCKS — utilisez libmpv".into(),
            );
        }
        warn!(%url, "FLUXPLAY_ALLOW_CLEARNET_EXTERNAL=1 — external Intent on clearnet");
    }

    info!(%url, mime = ?mime, "android ACTION_VIEW");
    let ctx = ndk_context::android_context();
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| e.to_string())?;
    let context = ctx.context() as jni::sys::jobject;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    clear_ex(&mut env);

    let Some(cls) = fluxplay_activity_class(&mut env, context) else {
        return Err("FluxPlayNativeActivity class missing".into());
    };
    let url_j = env.new_string(url).map_err(|e| e.to_string())?;
    let mime_j = env
        .new_string(mime.unwrap_or(""))
        .map_err(|e| e.to_string())?;
    match env.call_static_method(
        cls,
        "openUrl",
        "(Ljava/lang/String;Ljava/lang/String;)V",
        &[JValue::Object(&url_j), JValue::Object(&mime_j)],
    ) {
        Ok(_) => {
            info!("openUrl ok");
            Ok(())
        }
        Err(e) => {
            clear_ex(&mut env);
            error!(error = %e, "openUrl JNI failed");
            warn!("Install VLC / a video player if no handler is registered");
            Err(format!("openUrl: {e}"))
        }
    }
}

/// Launch the platform player / chooser for a stream URL (`video/*`).
pub fn open_stream_url(url: &str) -> Result<(), String> {
    open_url(url, Some("video/*"))
}
