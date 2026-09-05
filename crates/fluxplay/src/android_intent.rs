//! Open stream URLs via Android `ACTION_VIEW` (system / leanback player).

use jni::objects::{JObject, JValue};
use jni::JavaVM;
use tracing::{error, info, warn};

/// Launch the platform player / chooser for `url` (HTTP(S) HLS, etc.).
pub fn open_stream_url(url: &str) -> Result<(), String> {
    info!(%url, "android ACTION_VIEW");
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
