//! Shared JNI helpers — Activity class loader, context, exception hygiene.

use jni::objects::{GlobalRef, JClass, JObject, JValue};
use jni::JNIEnv;
use jni::JavaVM;

pub(crate) const FLUXPLAY_ACTIVITY_JNI: &str = "app/fluxplay/android/FluxPlayNativeActivity";
pub(crate) const FLUXPLAY_ACTIVITY_DOT: &str = "app.fluxplay.android.FluxPlayNativeActivity";

static FP_ACTIVITY_CLASS: std::sync::OnceLock<GlobalRef> = std::sync::OnceLock::new();

pub(crate) fn clear_ex(env: &mut JNIEnv<'_>) {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
}

pub(crate) fn vm_context() -> Option<(JavaVM, jni::sys::jobject)> {
    let ctx = ndk_context::android_context();
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
    Some((vm, ctx.context() as jni::sys::jobject))
}

fn activity_class_local<'a>(
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
                    Err(_) => clear_ex(env),
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
    match env.find_class(FLUXPLAY_ACTIVITY_JNI) {
        Ok(cls) => Some(cls),
        Err(_) => {
            clear_ex(env);
            None
        }
    }
}

pub(crate) fn activity_class<'a>(
    env: &mut JNIEnv<'a>,
    context: jni::sys::jobject,
) -> Option<&'static GlobalRef> {
    if let Some(g) = FP_ACTIVITY_CLASS.get() {
        return Some(g);
    }
    let local = activity_class_local(env, context)?;
    let global = env.new_global_ref(local).ok()?;
    let _ = FP_ACTIVITY_CLASS.set(global);
    FP_ACTIVITY_CLASS.get()
}

/// Prefer live `FluxPlayNativeActivity` instance; fall back to `ndk_context` context.
pub(crate) fn activity_object<'local>(
    env: &mut JNIEnv<'local>,
    context: jni::sys::jobject,
) -> Option<JObject<'local>> {
    let cls = activity_class(env, context)?;
    match env.call_static_method(
        cls,
        "activityForNative",
        "()Landroid/app/Activity;",
        &[],
    ) {
        Ok(v) => {
            let obj = v.l().ok()?;
            if obj.is_null() {
                None
            } else {
                Some(obj)
            }
        }
        Err(_) => {
            clear_ex(env);
            if context.is_null() {
                None
            } else {
                Some(unsafe { JObject::from_raw(context) })
            }
        }
    }
}

pub(crate) fn with_activity<R>(
    f: impl FnOnce(&mut JNIEnv<'_>, JObject<'_>) -> Option<R>,
) -> Option<R> {
    let (vm, raw_ctx) = vm_context()?;
    let mut env = vm.attach_current_thread().ok()?;
    let act = activity_object(&mut env, raw_ctx)?;
    f(&mut env, act)
}

pub(crate) fn sdk_int(env: &mut JNIEnv<'_>) -> i32 {
    env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")
        .ok()
        .and_then(|v| v.i().ok())
        .unwrap_or(26)
}
