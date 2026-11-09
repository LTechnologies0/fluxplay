//! Window insets, immersive mode, PiP probe, night mode — Rust JNI.

use std::sync::atomic::{AtomicU32, Ordering};

use jni::objects::{JObject, JValue};
use jni::JNIEnv;

use super::jni_host as plat;

static INSET_L: AtomicU32 = AtomicU32::new(0);
static INSET_T: AtomicU32 = AtomicU32::new(0);
static INSET_R: AtomicU32 = AtomicU32::new(0);
static INSET_B: AtomicU32 = AtomicU32::new(0);
static INSET_DPI: AtomicU32 = AtomicU32::new(160);

pub fn refresh_insets() {
    let Some((vm, raw_ctx)) = plat::vm_context() else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let Some(activity) = plat::activity_object(&mut env, raw_ctx) else {
        return;
    };
    let Some(decor) = env
        .call_method(&activity, "getWindow", "()Landroid/view/Window;", &[])
        .ok()
        .and_then(|v| v.l().ok())
        .and_then(|w| {
            env.call_method(&w, "getDecorView", "()Landroid/view/View;", &[])
                .ok()
                .and_then(|v| v.l().ok())
        })
    else {
        return;
    };
    let dpi = env
        .call_method(&activity, "getResources", "()Landroid/content/res/Resources;", &[])
        .ok()
        .and_then(|v| v.l().ok())
        .and_then(|res| {
            env.call_method(&res, "getDisplayMetrics", "()Landroid/util/DisplayMetrics;", &[])
                .ok()
                .and_then(|v| v.l().ok())
                .and_then(|dm| {
                    env.get_field(&dm, "densityDpi", "I")
                        .ok()
                        .and_then(|v| v.i().ok())
                })
        })
        .unwrap_or(160)
        .max(120) as u32;
    INSET_DPI.store(dpi, Ordering::Relaxed);

    let insets = env.call_method(&decor, "getRootWindowInsets", "()Landroid/view/WindowInsets;", &[]);
    let Ok(insets) = insets else {
        return;
    };
    let Ok(insets) = insets.l() else {
        return;
    };
    if insets.is_null() {
        return;
    }
    read_insets(&mut env, &insets, dpi);
}

fn read_insets(env: &mut JNIEnv<'_>, insets: &JObject<'_>, dpi: u32) {
    if plat::sdk_int(env) >= 30 {
        let types = (1i32 << 0) | (1i32 << 1) | (1i32 << 2) | (1i32 << 3) | (1i32 << 9); // status, nav, cutout bits approx
        let type_cls = env.find_class("android/view/WindowInsets$Type").ok();
        if let Some(tc) = type_cls {
            if let Ok(sys) = env.call_static_method(
                &tc,
                "systemBars",
                "()I",
                &[],
            ) {
                if let Ok(mask) = sys.i() {
                    if let Ok(g) = env.call_static_method(
                        &tc,
                        "displayCutout",
                        "()I",
                        &[],
                    ) {
                        if let Ok(cut) = g.i() {
                            let combined = mask | cut;
                            if let Ok(bars) = env.call_method(
                                insets,
                                "getInsetsIgnoringVisibility",
                                "(I)Landroid/graphics/Insets;",
                                &[JValue::Int(combined)],
                            ) {
                                if let Ok(b) = bars.l() {
                                    store_insets_from_graphics_insets(env, &b);
                                    let _ = types;
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let l = env
        .call_method(insets, "getSystemWindowInsetLeft", "()I", &[])
        .ok()
        .and_then(|v| v.i().ok())
        .unwrap_or(0)
        .max(0) as u32;
    let t = env
        .call_method(insets, "getSystemWindowInsetTop", "()I", &[])
        .ok()
        .and_then(|v| v.i().ok())
        .unwrap_or(0)
        .max(0) as u32;
    let r = env
        .call_method(insets, "getSystemWindowInsetRight", "()I", &[])
        .ok()
        .and_then(|v| v.i().ok())
        .unwrap_or(0)
        .max(0) as u32;
    let b = env
        .call_method(insets, "getSystemWindowInsetBottom", "()I", &[])
        .ok()
        .and_then(|v| v.i().ok())
        .unwrap_or(0)
        .max(0) as u32;
    INSET_L.store(l, Ordering::Relaxed);
    INSET_T.store(t, Ordering::Relaxed);
    INSET_R.store(r, Ordering::Relaxed);
    INSET_B.store(b, Ordering::Relaxed);
    INSET_DPI.store(dpi, Ordering::Relaxed);
}

fn store_insets_from_graphics_insets(env: &mut JNIEnv<'_>, insets: &JObject<'_>) {
    let mut read = |field: &str| {
        env.get_field(insets, field, "I")
            .ok()
            .and_then(|v| v.i().ok())
            .unwrap_or(0)
            .max(0) as u32
    };
    INSET_L.store(read("left"), Ordering::Relaxed);
    INSET_T.store(read("top"), Ordering::Relaxed);
    INSET_R.store(read("right"), Ordering::Relaxed);
    INSET_B.store(read("bottom"), Ordering::Relaxed);
}

pub fn insets_dp() -> (f32, f32, f32, f32) {
    let dpi = INSET_DPI.load(Ordering::Relaxed).max(120) as f32;
    let scale = dpi / 160.0;
    let (l, t, r, b) = (
        INSET_L.load(Ordering::Relaxed) as f32 / scale,
        INSET_T.load(Ordering::Relaxed) as f32 / scale,
        INSET_R.load(Ordering::Relaxed) as f32 / scale,
        INSET_B.load(Ordering::Relaxed) as f32 / scale,
    );
    if l == 0.0 && t == 0.0 && r == 0.0 && b == 0.0 {
        return (0.0, 0.0, 0.0, 24.0);
    }
    (l, t, r, b)
}

pub fn set_keep_screen_on(enable: bool) {
    with_window(|env, window| {
        let flag = env
            .get_static_field(
                "android/view/WindowManager$LayoutParams",
                "FLAG_KEEP_SCREEN_ON",
                "I",
            )
            .ok()
            .and_then(|v| v.i().ok())
            .unwrap_or(128);
        if enable {
            let _ = env.call_method(
                &window,
                "addFlags",
                "(I)V",
                &[JValue::Int(flag)],
            );
        } else {
            let _ = env.call_method(
                &window,
                "clearFlags",
                "(I)V",
                &[JValue::Int(flag)],
            );
        }
    });
}

pub fn set_immersive_mode(immersive: bool) {
    with_window(|env, window| {
        if plat::sdk_int(env) >= 30 {
            let decor = env
                .call_method(&window, "getDecorView", "()Landroid/view/View;", &[])
                .ok()
                .and_then(|v| v.l().ok());
            let Some(decor) = decor else {
                return;
            };
            let controller = env
                .call_method(&window, "getInsetsController", "()Landroid/view/WindowInsetsController;", &[])
                .ok()
                .and_then(|v| v.l().ok());
            let Some(controller) = controller else {
                return;
            };
            let Ok(type_cls) = env.find_class("android/view/WindowInsets$Type") else {
                return;
            };
            let Ok(types) = env.call_static_method(&type_cls, "systemBars", "()I", &[]) else {
                return;
            };
            let Ok(types) = types.i() else {
                return;
            };
            if immersive {
                let _ = env.call_method(
                    &controller,
                    "hide",
                    "(I)V",
                    &[JValue::Int(types)],
                );
                let _ = env.call_method(
                    &controller,
                    "setSystemBarsBehavior",
                    "(I)V",
                    &[JValue::Int(2)],
                );
            } else {
                let _ = env.call_method(
                    &controller,
                    "show",
                    "(I)V",
                    &[JValue::Int(types)],
                );
            }
            let _ = decor;
        }
    });
}

pub fn in_pip_mode() -> bool {
    let Some((vm, raw_ctx)) = plat::vm_context() else {
        return false;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return false;
    };
    let Some(activity) = plat::activity_object(&mut env, raw_ctx) else {
        return false;
    };
    if plat::sdk_int(&mut env) < 24 {
        return false;
    }
    env.call_method(&activity, "isInPictureInPictureMode", "()Z", &[])
        .ok()
        .and_then(|v| v.z().ok())
        .unwrap_or(false)
}

pub fn is_night_mode() -> bool {
    let Some((vm, raw_ctx)) = plat::vm_context() else {
        return false;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return false;
    };
    let Some(activity) = plat::activity_object(&mut env, raw_ctx) else {
        return false;
    };
    let config = env
        .call_method(&activity, "getResources", "()Landroid/content/res/Resources;", &[])
        .ok()
        .and_then(|v| v.l().ok())
        .and_then(|res| {
            env.call_method(&res, "getConfiguration", "()Landroid/content/res/Configuration;", &[])
                .ok()
                .and_then(|v| v.l().ok())
        });
    let Some(config) = config else {
        return false;
    };
    let ui = env
        .get_field(&config, "uiMode", "I")
        .ok()
        .and_then(|v| v.i().ok())
        .unwrap_or(0);
    (ui & 0x30) == 0x20
}

pub fn set_display_color_mode(mode: &str) {
    let m = mode.to_ascii_lowercase();
    let color_mode = if m.contains("hdr") || m == "dv" || m == "dolby" {
        2i32
    } else if m.contains("wide") || m == "p3" || m == "gamut" {
        1i32
    } else {
        0i32
    };
    with_window(|env, window| {
        if plat::sdk_int(env) >= 26 {
            let _ = env.call_method(
                &window,
                "setColorMode",
                "(I)V",
                &[JValue::Int(color_mode)],
            );
        }
    });
}

fn with_window(f: impl FnOnce(&mut JNIEnv<'_>, JObject<'_>)) {
    let Some((vm, raw_ctx)) = plat::vm_context() else {
        return;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return;
    };
    let Some(activity) = plat::activity_object(&mut env, raw_ctx) else {
        return;
    };
    let Some(window) = env
        .call_method(&activity, "getWindow", "()Landroid/view/Window;", &[])
        .ok()
        .and_then(|v| v.l().ok())
    else {
        return;
    };
    f(&mut env, window);
}
