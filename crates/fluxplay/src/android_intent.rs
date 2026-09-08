//! Android Intent / Activity helpers (ACTION_VIEW, keep-screen-on).

use jni::objects::{JObject, JValue};
use jni::JavaVM;
use tracing::{debug, error, info, warn};

/// FLAG_KEEP_SCREEN_ON — keep display awake while playback is active.
const FLAG_KEEP_SCREEN_ON: i32 = 0x0000_0080;

/// Toggle Activity window keep-screen-on (playback).
pub fn set_keep_screen_on(enable: bool) {
    let ctx = ndk_context::android_context();
    let Ok(vm) = (unsafe { JavaVM::from_raw(ctx.vm().cast()) }) else {
        return;
    };
    let activity = ctx.context() as jni::sys::jobject;
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let activity_obj = unsafe { JObject::from_raw(activity) };
    let Ok(window) = env.call_method(&activity_obj, "getWindow", "()Landroid/view/Window;", &[])
    else {
        return;
    };
    let Ok(window) = window.l() else {
        return;
    };
    let method = if enable {
        "addFlags"
    } else {
        "clearFlags"
    };
    match env.call_method(
        &window,
        method,
        "(I)V",
        &[JValue::Int(FLAG_KEEP_SCREEN_ON)],
    ) {
        Ok(_) => debug!(enable, "keep_screen_on"),
        Err(e) => warn!(error = %e, "keep_screen_on failed"),
    }
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
    let activity = ctx.context() as jni::sys::jobject;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;

    let action = env
        .new_string("android.intent.action.VIEW")
        .map_err(|e| e.to_string())?;
    let url_j = env.new_string(url).map_err(|e| e.to_string())?;

    let uri_class = env
        .find_class("android/net/Uri")
        .map_err(|e| e.to_string())?;
    let uri = env
        .call_static_method(
            uri_class,
            "parse",
            "(Ljava/lang/String;)Landroid/net/Uri;",
            &[JValue::Object(&url_j)],
        )
        .map_err(|e| e.to_string())?
        .l()
        .map_err(|e| e.to_string())?;

    let intent_class = env
        .find_class("android/content/Intent")
        .map_err(|e| e.to_string())?;
    let intent = env
        .new_object(
            &intent_class,
            "(Ljava/lang/String;Landroid/net/Uri;)V",
            &[JValue::Object(&action), JValue::Object(&uri)],
        )
        .map_err(|e| e.to_string())?;

    if let Some(mime_str) = mime {
        let mime_j = env.new_string(mime_str).map_err(|e| e.to_string())?;
        let _ = env.call_method(
            &intent,
            "setDataAndType",
            "(Landroid/net/Uri;Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&uri), JValue::Object(&mime_j)],
        );
    }

    let _ = env.call_method(
        &intent,
        "addFlags",
        "(I)Landroid/content/Intent;",
        &[JValue::Int(0x1000_0000)],
    );

    let activity_obj = unsafe { JObject::from_raw(activity) };
    match env.call_method(
        &activity_obj,
        "startActivity",
        "(Landroid/content/Intent;)V",
        &[JValue::Object(&intent)],
    ) {
        Ok(_) => {
            info!("startActivity ok");
            Ok(())
        }
        Err(e) => {
            error!(error = %e, "startActivity failed");
            warn!("Install VLC / a video player if no handler is registered");
            Err(format!("startActivity: {e}"))
        }
    }
}

/// Launch the platform player / chooser for a stream URL (`video/*`).
pub fn open_stream_url(url: &str) -> Result<(), String> {
    open_url(url, Some("video/*"))
}
