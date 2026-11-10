//! AudioManager focus — Rust-owned state (no JSON file poll).

use std::sync::atomic::{AtomicI8, Ordering};

use jni::objects::{GlobalRef, JObject, JValue};
use jni::JNIEnv;
use tracing::{debug, info, warn};

use super::jni_host as plat;

const HELD_UNKNOWN: i8 = -1;
const HELD_FALSE: i8 = 0;
const HELD_TRUE: i8 = 1;
const HELD_DELAYED: i8 = 2;

static HELD: AtomicI8 = AtomicI8::new(HELD_UNKNOWN);

const AUDIOFOCUS_GAIN: i32 = 1;
const AUDIOFOCUS_LOSS: i32 = -1;
const AUDIOFOCUS_LOSS_TRANSIENT: i32 = -2;
const AUDIOFOCUS_REQUEST_GRANTED: i32 = 1;
const AUDIOFOCUS_REQUEST_DELAYED: i32 = 2;

static FOCUS_LISTENER: std::sync::OnceLock<GlobalRef> = std::sync::OnceLock::new();
static FOCUS_REQUEST: std::sync::OnceLock<GlobalRef> = std::sync::OnceLock::new();

pub fn on_focus_change(change: i32) {
    match change {
        AUDIOFOCUS_GAIN => {
            HELD.store(HELD_TRUE, Ordering::SeqCst);
            info!(target: "fluxplay::android", change, "audio focus GAIN");
        }
        AUDIOFOCUS_LOSS | AUDIOFOCUS_LOSS_TRANSIENT => {
            HELD.store(HELD_FALSE, Ordering::SeqCst);
            info!(target: "fluxplay::android", change, "audio focus LOSS");
        }
        -3 => debug!(target: "fluxplay::android", "audio focus duck"),
        other => debug!(target: "fluxplay::android", change = other, "audio focus other"),
    }
}

pub fn poll_held() -> Option<bool> {
    match HELD.load(Ordering::SeqCst) {
        HELD_TRUE => Some(true),
        HELD_FALSE => Some(false),
        _ => None,
    }
}

pub fn request() {
    let Some((vm, raw_ctx)) = plat::vm_context() else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let Some(act) = plat::activity_object(&mut env, raw_ctx) else {
        warn!(target: "fluxplay::android", "audio focus: no activity");
        return;
    };
    let Some(cls) = plat::activity_class(&mut env, raw_ctx) else {
        return;
    };

    let Some(listener) = ensure_listener(&mut env, cls) else {
        return;
    };
    let Some(am) = audio_manager(&mut env, &act) else {
        return;
    };

    let sdk = sdk_int(&mut env);
    let granted = if sdk >= 26 {
        let Some(req) = ensure_focus_request(&mut env, &listener) else {
            return;
        };
        env.call_method(
            &am,
            "requestAudioFocus",
            "(Landroid/media/AudioFocusRequest;)I",
            &[JValue::Object(&req)],
        )
        .ok()
        .and_then(|v| v.i().ok())
        .unwrap_or(-1)
    } else {
        env.call_method(
            &am,
            "requestAudioFocus",
            "(Landroid/media/AudioManager$OnAudioFocusChangeListener;II)I",
            &[
                JValue::Object(&listener),
                JValue::Int(3),
                JValue::Int(AUDIOFOCUS_GAIN),
            ],
        )
        .ok()
        .and_then(|v| v.i().ok())
        .unwrap_or(-1)
    };

    match granted {
        AUDIOFOCUS_REQUEST_GRANTED => {
            HELD.store(HELD_TRUE, Ordering::SeqCst);
            info!(target: "fluxplay::android", "audio focus GRANTED");
        }
        AUDIOFOCUS_REQUEST_DELAYED => {
            HELD.store(HELD_DELAYED, Ordering::SeqCst);
            info!(target: "fluxplay::android", "audio focus DELAYED");
        }
        _ => {
            HELD.store(HELD_FALSE, Ordering::SeqCst);
            warn!(target: "fluxplay::android", granted, "audio focus denied");
        }
    }
}

pub fn abandon() {
    let Some((vm, raw_ctx)) = plat::vm_context() else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let Some(act) = plat::activity_object(&mut env, raw_ctx) else {
        HELD.store(HELD_FALSE, Ordering::SeqCst);
        return;
    };
    let Some(am) = audio_manager(&mut env, &act) else {
        HELD.store(HELD_FALSE, Ordering::SeqCst);
        return;
    };
    let sdk = sdk_int(&mut env);
    if sdk >= 26 {
        if let Some(req) = FOCUS_REQUEST.get() {
            let _ = env.call_method(
                &am,
                "abandonAudioFocusRequest",
                "(Landroid/media/AudioFocusRequest;)I",
                &[JValue::Object(req.as_obj())],
            );
            plat::clear_ex(&mut env);
        }
    } else if let Some(listener) = FOCUS_LISTENER.get() {
        let _ = env.call_method(
            &am,
            "abandonAudioFocus",
            "(Landroid/media/AudioManager$OnAudioFocusChangeListener;)I",
            &[JValue::Object(listener.as_obj())],
        );
        plat::clear_ex(&mut env);
    }
    HELD.store(HELD_FALSE, Ordering::SeqCst);
}

fn sdk_int(env: &mut JNIEnv<'_>) -> i32 {
    env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")
        .ok()
        .and_then(|v| v.i().ok())
        .unwrap_or(26)
}

fn audio_manager<'local>(
    env: &mut JNIEnv<'local>,
    activity: &JObject<'_>,
) -> Option<JObject<'local>> {
    let jsvc = env.new_string("audio").ok()?;
    let v = env
        .call_method(
            activity,
            "getSystemService",
            "(Ljava/lang/String;)Ljava/lang/Object;",
            &[JValue::Object(&jsvc)],
        )
        .ok()?;
    let obj = v.l().ok()?;
    (!obj.is_null()).then_some(obj)
}

fn ensure_listener<'local>(
    env: &mut JNIEnv<'local>,
    activity_cls: &GlobalRef,
) -> Option<JObject<'local>> {
    if let Some(g) = FOCUS_LISTENER.get() {
        return Some(unsafe { JObject::from_raw(g.as_obj().as_raw()) });
    }
    match env.call_static_method(
        activity_cls,
        "audioFocusListenerForNative",
        "()Landroid/media/AudioManager$OnAudioFocusChangeListener;",
        &[],
    ) {
        Ok(v) => {
            let obj = v.l().ok()?;
            let global = env.new_global_ref(&obj).ok()?;
            let _ = FOCUS_LISTENER.set(global);
            Some(obj)
        }
        Err(_) => {
            plat::clear_ex(env);
            None
        }
    }
}

fn ensure_focus_request<'local>(
    env: &mut JNIEnv<'local>,
    listener: &JObject<'_>,
) -> Option<JObject<'local>> {
    if let Some(g) = FOCUS_REQUEST.get() {
        return Some(unsafe { JObject::from_raw(g.as_obj().as_raw()) });
    }
    let Some(attrs) = build_audio_attributes(env) else {
        return None;
    };
    let builder = env
        .new_object(
            "android/media/AudioFocusRequest$Builder",
            "(I)V",
            &[JValue::Int(AUDIOFOCUS_GAIN)],
        )
        .ok()?;
    let builder = env
        .call_method(
            &builder,
            "setAudioAttributes",
            "(Landroid/media/AudioAttributes;)Landroid/media/AudioFocusRequest$Builder;",
            &[JValue::Object(&attrs)],
        )
        .ok()
        .and_then(|v| v.l().ok())?;
    let builder = env
        .call_method(
            &builder,
            "setOnAudioFocusChangeListener",
            "(Landroid/media/AudioManager$OnAudioFocusChangeListener;)Landroid/media/AudioFocusRequest$Builder;",
            &[JValue::Object(listener)],
        )
        .ok()
        .and_then(|v| v.l().ok())?;
    let builder = env
        .call_method(
            &builder,
            "setWillPauseWhenDucked",
            "(Z)Landroid/media/AudioFocusRequest$Builder;",
            &[JValue::Bool(0)],
        )
        .ok()
        .and_then(|v| v.l().ok())?;
    let builder = env
        .call_method(
            &builder,
            "setAcceptsDelayedFocusGain",
            "(Z)Landroid/media/AudioFocusRequest$Builder;",
            &[JValue::Bool(1)],
        )
        .ok()
        .and_then(|v| v.l().ok())?;
    let req = env
        .call_method(
            &builder,
            "build",
            "()Landroid/media/AudioFocusRequest;",
            &[],
        )
        .ok()
        .and_then(|v| v.l().ok())?;
    let global = env.new_global_ref(&req).ok()?;
    let _ = FOCUS_REQUEST.set(global);
    Some(req)
}

fn build_audio_attributes<'local>(env: &mut JNIEnv<'local>) -> Option<JObject<'local>> {
    let builder = env
        .new_object("android/media/AudioAttributes$Builder", "()V", &[])
        .ok()?;
    let builder = env
        .call_method(
            &builder,
            "setUsage",
            "(I)Landroid/media/AudioAttributes$Builder;",
            &[JValue::Int(1)],
        )
        .ok()
        .and_then(|v| v.l().ok())?;
    let builder = env
        .call_method(
            &builder,
            "setContentType",
            "(I)Landroid/media/AudioAttributes$Builder;",
            &[JValue::Int(3)],
        )
        .ok()
        .and_then(|v| v.l().ok())?;
    env.call_method(
        &builder,
        "build",
        "()Landroid/media/AudioAttributes;",
        &[],
    )
    .ok()
    .and_then(|v| v.l().ok())
}
