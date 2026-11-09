//! MediaSession + media-style notification — Rust JNI.

use std::sync::OnceLock;

use jni::objects::{GlobalRef, JObject, JValue};
use jni::JNIEnv;
use tracing::warn;

use super::jni_host as plat;

static SESSION: OnceLock<GlobalRef> = OnceLock::new();
static CHANNEL_ID: &str = "fluxplay_playback";
const NOTIFY_ID: i32 = 1;

pub fn sync_playback_notification(playing: bool, title: &str, position_ms: i64) {
    let Some((vm, raw_ctx)) = plat::vm_context() else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let Some(activity) = plat::activity_object(&mut env, raw_ctx) else {
        return;
    };
    ensure_channel(&mut env, &activity);
    let Some(session) = ensure_session(&mut env, &activity) else {
        return;
    };

    let state = if playing { 3i64 } else { 2i64 };
    let actions = (1i64 << 2) | (1i64 << 3) | (1i64 << 4);

    let Ok(pb_builder) =
        env.new_object("android/media/session/PlaybackState$Builder", "(J)V", &[JValue::Long(0)])
    else {
        plat::clear_ex(&mut env);
        return;
    };
    let Ok(pb_builder) = env.call_method(
        &pb_builder,
        "setState",
        "(JJF)Landroid/media/session/PlaybackState$Builder;",
        &[
            JValue::Long(state),
            JValue::Long(position_ms.max(0)),
            JValue::Float(1.0),
        ],
    ) else {
        return;
    };
    let pb_builder = pb_builder.l().ok();
    let Some(pb_builder) = pb_builder else {
        return;
    };
    let Ok(pb_builder) = env.call_method(
        &pb_builder,
        "setActions",
        "(J)Landroid/media/session/PlaybackState$Builder;",
        &[JValue::Long(actions)],
    ) else {
        return;
    };
    let pb_builder = pb_builder.l().ok();
    let Some(pb_builder) = pb_builder else {
        return;
    };
    let Ok(playback_state) = env.call_method(
        &pb_builder,
        "build",
        "()Landroid/media/session/PlaybackState;",
        &[],
    ) else {
        return;
    };
    let playback_state = playback_state.l().ok();
    let Some(playback_state) = playback_state else {
        return;
    };
    let _ = env.call_method(
        session.as_obj(),
        "setPlaybackState",
        "(Landroid/media/session/PlaybackState;)V",
        &[JValue::Object(&playback_state)],
    );

    if !title.is_empty() {
        if let (Ok(key), Ok(jtitle)) = (
            env.new_string("android.media.metadata.TITLE"),
            env.new_string(title),
        ) {
            if let Ok(md_builder) =
                env.new_object("android/media/MediaMetadata$Builder", "()V", &[])
            {
                if let Ok(md_builder) = env.call_method(
                    &md_builder,
                    "putString",
                    "(Ljava/lang/String;Ljava/lang/String;)Landroid/media/MediaMetadata$Builder;",
                    &[JValue::Object(&key), JValue::Object(&jtitle)],
                ) {
                    if let Ok(md_builder) = md_builder.l() {
                        if let Ok(md) = env.call_method(
                            &md_builder,
                            "build",
                            "()Landroid/media/MediaMetadata;",
                            &[],
                        ) {
                            if let Ok(md) = md.l() {
                                let _ = env.call_method(
                                    session.as_obj(),
                                    "setMetadata",
                                    "(Landroid/media/MediaMetadata;)V",
                                    &[JValue::Object(&md)],
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    let _ = env.call_method(session.as_obj(), "setActive", "(Z)V", &[JValue::Bool(1)]);

    if playing || !title.is_empty() {
        post_notification(&mut env, &activity, title, playing);
    } else {
        hide_playback_notification();
    }
}

pub fn hide_playback_notification() {
    let Some((vm, raw_ctx)) = plat::vm_context() else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let Some(activity) = plat::activity_object(&mut env, raw_ctx) else {
        return;
    };
    let Some(nm) = notification_manager(&mut env, &activity) else {
        return;
    };
    let _ = env.call_method(&nm, "cancel", "(I)V", &[JValue::Int(NOTIFY_ID)]);
    if let Some(session) = SESSION.get() {
        let _ = env.call_method(session.as_obj(), "setActive", "(Z)V", &[JValue::Bool(0)]);
    }
}

fn ensure_channel(env: &mut JNIEnv<'_>, activity: &JObject<'_>) {
    if plat::sdk_int(env) < 26 {
        return;
    }
    let Some(nm) = notification_manager(env, activity) else {
        return;
    };
    let (Ok(id), Ok(name)) = (
        env.new_string(CHANNEL_ID),
        env.new_string("FluxPlay playback"),
    ) else {
        return;
    };
    let Ok(channel) = env.new_object(
        "android/app/NotificationChannel",
        "(Ljava/lang/String;Ljava/lang/CharSequence;I)V",
        &[
            JValue::Object(&id),
            JValue::Object(&name),
            JValue::Int(2),
        ],
    ) else {
        return;
    };
    let _ = env.call_method(
        &nm,
        "createNotificationChannel",
        "(Landroid/app/NotificationChannel;)V",
        &[JValue::Object(&channel)],
    );
}

fn ensure_session(env: &mut JNIEnv<'_>, activity: &JObject<'_>) -> Option<&'static GlobalRef> {
    if let Some(g) = SESSION.get() {
        return Some(g);
    }
    let tag = env.new_string("FluxPlay").ok()?;
    let session = env
        .new_object(
            "android/media/session/MediaSession",
            "(Landroid/content/Context;Ljava/lang/String;)V",
            &[JValue::Object(activity), JValue::Object(&tag)],
        )
        .ok()?;
    let global = env.new_global_ref(&session).ok()?;
    let _ = SESSION.set(global);
    if let Some(g) = SESSION.get() {
        if let Some(cls) = plat::activity_class(env, {
            let ctx = ndk_context::android_context();
            ctx.context() as jni::sys::jobject
        }) {
            let _ = env.call_static_method(
                cls,
                "attachMediaSessionCallback",
                "(Landroid/media/session/MediaSession;)V",
                &[JValue::Object(g.as_obj())],
            );
            plat::clear_ex(env);
        }
    }
    SESSION.get()
}

fn post_notification(env: &mut JNIEnv<'_>, activity: &JObject<'_>, title: &str, playing: bool) {
    let Some(nm) = notification_manager(env, activity) else {
        return;
    };
    let Ok(channel) = env.new_string(CHANNEL_ID) else {
        return;
    };
    let Ok(builder) = env.new_object(
        "android/app/Notification$Builder",
        "(Landroid/content/Context;Ljava/lang/String;)V",
        &[JValue::Object(activity), JValue::Object(&channel)],
    ) else {
        return;
    };
    let display = if title.is_empty() { "FluxPlay" } else { title };
    let Ok(jtitle) = env.new_string(display) else {
        return;
    };
    let Ok(builder) = env.call_method(
        &builder,
        "setContentTitle",
        "(Ljava/lang/CharSequence;)Landroid/app/Notification$Builder;",
        &[JValue::Object(&jtitle)],
    ) else {
        return;
    };
    let builder = match builder.l().ok() {
        Some(b) => b,
        None => return,
    };
    let status = if playing { "Lecture" } else { "Pause" };
    let Ok(jtext) = env.new_string(status) else {
        return;
    };
    let Ok(builder) = env.call_method(
        &builder,
        "setContentText",
        "(Ljava/lang/CharSequence;)Landroid/app/Notification$Builder;",
        &[JValue::Object(&jtext)],
    ) else {
        return;
    };
    let builder = match builder.l().ok() {
        Some(b) => b,
        None => return,
    };
    let Ok(builder) = env.call_method(
        &builder,
        "setSmallIcon",
        "(I)Landroid/app/Notification$Builder;",
        &[JValue::Int(0x01080209)],
    ) else {
        return;
    };
    let builder = match builder.l().ok() {
        Some(b) => b,
        None => return,
    };
    let Ok(builder) = env.call_method(
        &builder,
        "setOngoing",
        "(Z)Landroid/app/Notification$Builder;",
        &[JValue::Bool(u8::from(playing))],
    ) else {
        return;
    };
    let builder = match builder.l().ok() {
        Some(b) => b,
        None => return,
    };
    let Ok(notification) = env.call_method(
        &builder,
        "build",
        "()Landroid/app/Notification;",
        &[],
    ) else {
        warn!(target: "fluxplay::android", "notification build failed");
        return;
    };
    let notification = match notification.l().ok() {
        Some(n) => n,
        None => return,
    };
    let _ = env.call_method(
        &nm,
        "notify",
        "(ILandroid/app/Notification;)V",
        &[JValue::Int(NOTIFY_ID), JValue::Object(&notification)],
    );
}

fn notification_manager<'local>(
    env: &mut JNIEnv<'local>,
    activity: &JObject<'_>,
) -> Option<JObject<'local>> {
    let jsvc = env.new_string("notification").ok()?;
    env.call_method(
        activity,
        "getSystemService",
        "(Ljava/lang/String;)Ljava/lang/Object;",
        &[JValue::Object(&jsvc)],
    )
    .ok()
    .and_then(|v| v.l().ok())
    .filter(|o| !o.is_null())
}
