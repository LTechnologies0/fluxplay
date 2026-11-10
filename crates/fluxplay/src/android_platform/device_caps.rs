//! Device / MediaCodec / display caps via JNI (parity with Java `writeDeviceCaps`).

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use fluxplay_player::AndroidDeviceCaps;
use jni::objects::{JIntArray, JObject, JObjectArray, JValue};
use tracing::debug;

use super::jni_host as plat;

struct Cache {
    at: Instant,
    caps: AndroidDeviceCaps,
}

static CACHE: OnceLock<Mutex<Option<Cache>>> = OnceLock::new();

fn cache_slot() -> &'static Mutex<Option<Cache>> {
    CACHE.get_or_init(|| Mutex::new(None))
}

pub fn poll_device_caps() -> Option<AndroidDeviceCaps> {
    {
        let guard = cache_slot().lock().ok()?;
        if let Some(c) = guard.as_ref() {
            if c.at.elapsed() < Duration::from_secs(3) {
                return Some(c.caps.clone());
            }
        }
    }
    let caps = probe_live()?;
    if let Ok(mut guard) = cache_slot().lock() {
        *guard = Some(Cache {
            at: Instant::now(),
            caps: caps.clone(),
        });
    }
    Some(caps)
}

fn merge_json_fallback(
    env: &mut jni::JNIEnv<'_>,
    activity: &JObject<'_>,
    caps: &mut AndroidDeviceCaps,
) {
    let Ok(files) = env.call_method(activity, "getFilesDir", "()Ljava/io/File;", &[]) else {
        return;
    };
    let Ok(files) = files.l() else {
        return;
    };
    let Ok(path_obj) = env.call_method(&files, "getAbsolutePath", "()Ljava/lang/String;", &[]) else {
        return;
    };
    let Ok(path_obj) = path_obj.l() else {
        return;
    };
    let Ok(path_str) = env.get_string((&path_obj).into()) else {
        return;
    };
    let path = std::path::PathBuf::from(String::from(path_str)).join("saf_inbox/device_caps.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(file_caps) = serde_json::from_str::<AndroidDeviceCaps>(&text) else {
        return;
    };
    if caps.gl_renderer.is_empty() && !file_caps.gl_renderer.is_empty() {
        caps.gl_renderer = file_caps.gl_renderer;
    }
    if caps.refresh_modes.is_empty() && !file_caps.refresh_modes.is_empty() {
        caps.refresh_modes = file_caps.refresh_modes;
    }
    if caps.hdr_types.is_empty() && !file_caps.hdr_types.is_empty() {
        caps.hdr_types = file_caps.hdr_types;
        caps.hdr_labels = file_caps.hdr_labels;
        caps.hdr_capable = file_caps.hdr_capable;
    }
    if caps.mediacodec_max_w == 0 && file_caps.mediacodec_max_w > 0 {
        caps.mediacodec_max_w = file_caps.mediacodec_max_w;
        caps.mediacodec_max_h = file_caps.mediacodec_max_h;
    }
    if !caps.mediacodec_hdr && file_caps.mediacodec_hdr {
        caps.mediacodec_hdr = true;
    }
}

fn probe_live() -> Option<AndroidDeviceCaps> {
    let Some((vm, raw_ctx)) = plat::vm_context() else {
        return None;
    };
    let Ok(mut env) = vm.attach_current_thread() else {
        return None;
    };
    let Some(activity) = plat::activity_object(&mut env, raw_ctx) else {
        return None;
    };
    let Some(cls) = plat::activity_class(&mut env, raw_ctx) else {
        return None;
    };

    let mut caps = AndroidDeviceCaps::default();
    caps.cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4) as u32;
    caps.sdk_int = plat::sdk_int(&mut env) as u32;
    caps.manufacturer = jstring_static(&mut env, "android/os/Build", "MANUFACTURER");
    caps.model = jstring_static(&mut env, "android/os/Build", "MODEL");
    caps.board = jstring_static(&mut env, "android/os/Build", "BOARD");
    caps.hardware = jstring_static(&mut env, "android/os/Build", "HARDWARE");
    if caps.sdk_int >= 31 {
        caps.soc = jstring_static(&mut env, "android/os/Build", "SOC_MODEL");
    }
    if caps.soc.is_empty() {
        caps.soc = caps.hardware.clone();
    }
    probe_display(&mut env, &activity, &mut caps);
    probe_mediacodec(&mut env, &mut caps);
    caps.panel_oled = guess_oled_panel(&caps);
    caps.surface_ready = env
        .call_static_method(cls, "isVideoSurfaceReady", "()Z", &[])
        .ok()
        .and_then(|v| v.z().ok())
        .unwrap_or(false);
    if let Ok(arr) = env.call_static_method(cls, "videoSurfaceSizePx", "()[I", &[]) {
        if let Ok(obj) = arr.l() {
            let jarr: JIntArray = unsafe { JIntArray::from_raw(obj.into_raw()) };
            if env.get_array_length(&jarr).unwrap_or(0) >= 2 {
                let mut buf = [0i32; 2];
                if env.get_int_array_region(&jarr, 0, &mut buf).is_ok() {
                    caps.surface_w = buf[0].max(0) as u32;
                    caps.surface_h = buf[1].max(0) as u32;
                }
            }
        }
    }
    merge_json_fallback(&mut env, &activity, &mut caps);
    debug!(
        target: "fluxplay::android",
        soc = %caps.soc,
        hz = caps.refresh_hz,
        hdr = caps.hdr_capable,
        mc4k = caps.mediacodec_4k,
        mc_hdr = caps.mediacodec_hdr,
        max = %format!("{}x{}", caps.mediacodec_max_w, caps.mediacodec_max_h),
        surface = caps.surface_ready,
        "device caps (rust jni)"
    );
    Some(caps)
}

fn jstring_static(env: &mut jni::JNIEnv<'_>, class: &str, field: &str) -> String {
    env.get_static_field(class, field, "Ljava/lang/String;")
        .ok()
        .and_then(|v| v.l().ok())
        .filter(|o| !o.is_null())
        .and_then(|o| {
            env.get_string((&o).into())
                .ok()
                .map(|s| s.into())
        })
        .unwrap_or_default()
}

fn hdr_type_label(t: i32) -> &'static str {
    match t {
        1 => "Dolby Vision",
        2 => "HDR10",
        3 => "HLG",
        4 => "HDR10+",
        _ => "HDR",
    }
}

fn guess_oled_panel(caps: &AndroidDeviceCaps) -> bool {
    let blob = format!(
        "{} {} {} {}",
        caps.manufacturer, caps.model, caps.board, caps.hardware
    )
    .to_ascii_lowercase();
    if blob.contains("pixel")
        || blob.contains("galaxy")
        || blob.contains("sm-s")
        || blob.contains("sm-g")
        || blob.contains("sm-n")
        || blob.contains("oneplus")
        || blob.contains("oppo")
        || blob.contains("vivo")
        || blob.contains("xiaomi")
        || blob.contains("mi ")
        || blob.contains("redmi")
        || blob.contains("poco")
        || blob.contains("nothing")
        || blob.contains("amoled")
        || blob.contains("oled")
    {
        return true;
    }
    caps.hdr_capable
        && (blob.contains("google") || blob.contains("samsung"))
}

fn probe_display(env: &mut jni::JNIEnv<'_>, activity: &JObject<'_>, caps: &mut AndroidDeviceCaps) {
    let wm = match env.call_method(
        activity,
        "getWindowManager",
        "()Landroid/view/WindowManager;",
        &[],
    ) {
        Ok(v) => v.l().ok(),
        Err(_) => {
            plat::clear_ex(env);
            None
        }
    };
    let Some(wm) = wm else {
        return;
    };
    let Some(display) = env
        .call_method(&wm, "getDefaultDisplay", "()Landroid/view/Display;", &[])
        .ok()
        .and_then(|v| v.l().ok())
    else {
        return;
    };
    caps.refresh_hz = env
        .call_method(&display, "getRefreshRate", "()F", &[])
        .ok()
        .and_then(|v| v.f().ok())
        .map(|f| f.round() as u32)
        .unwrap_or(60)
        .max(24);

    if caps.sdk_int >= 24 {
        if let Ok(hdr) = env.call_method(
            &display,
            "getHdrCapabilities",
            "()Landroid/view/Display$HdrCapabilities;",
            &[],
        ) {
            if let Ok(hobj) = hdr.l() {
                if !hobj.is_null() {
                    if let Ok(types) = env.call_method(
                        &hobj,
                        "getSupportedHdrTypes",
                        "()[I",
                        &[],
                    ) {
                        if let Ok(tarr) = types.l() {
                            let jarr: JIntArray = unsafe { JIntArray::from_raw(tarr.into_raw()) };
                            let len = env.get_array_length(&jarr).unwrap_or(0);
                            if len > 0 {
                                let mut buf = vec![0i32; len as usize];
                                if env.get_int_array_region(&jarr, 0, &mut buf).is_ok() {
                                    caps.hdr_types = buf;
                                    caps.hdr_capable = !caps.hdr_types.is_empty();
                                    caps.hdr_labels = caps
                                        .hdr_types
                                        .iter()
                                        .map(|&t| {
                                            if t == 1 || t == 2 || t == 3 || t == 4 {
                                                hdr_type_label(t).to_string()
                                            } else {
                                                format!("HDR({t})")
                                            }
                                        })
                                        .collect();
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if caps.sdk_int >= 23 {
        if let Ok(mode) = env.call_method(
            &display,
            "getMode",
            "()Landroid/view/Display$Mode;",
            &[],
        ) {
            if let Ok(mobj) = mode.l() {
                if !mobj.is_null() {
                    if let Some(hz) = env
                        .call_method(&mobj, "getRefreshRate", "()F", &[])
                        .ok()
                        .and_then(|v| v.f().ok())
                    {
                        caps.refresh_hz = caps.refresh_hz.max(hz.round() as u32);
                    }
                }
            }
        }
        if let Ok(modes) = env.call_method(
            &display,
            "getSupportedModes",
            "()[Landroid/view/Display$Mode;",
            &[],
        ) {
            if let Ok(marr) = modes.l() {
                if !marr.is_null() {
                    let arr = JObjectArray::from(marr);
                    let len = env.get_array_length(&arr).unwrap_or(0);
                    for i in 0..len {
                        let Ok(m) = env.get_object_array_element(&arr, i) else {
                            continue;
                        };
                        if let Some(hz) = env
                            .call_method(&m, "getRefreshRate", "()F", &[])
                            .ok()
                            .and_then(|v| v.f().ok())
                        {
                            caps.refresh_hz = caps.refresh_hz.max(hz.round() as u32);
                            let dup = caps
                                .refresh_modes
                                .iter()
                                .any(|&e| (e - hz).abs() < 0.5);
                            if !dup {
                                caps.refresh_modes.push(hz);
                            }
                        }
                    }
                }
            }
        }
    }
}

fn probe_mediacodec(env: &mut jni::JNIEnv<'_>, caps: &mut AndroidDeviceCaps) {
    let Ok(list) = env.new_object("android/media/MediaCodecList", "(I)V", &[JValue::Int(0)]) else {
        plat::clear_ex(env);
        return;
    };
    let Ok(codecs) = env.call_method(
        &list,
        "getCodecInfos",
        "()[Landroid/media/MediaCodecInfo;",
        &[],
    ) else {
        plat::clear_ex(env);
        return;
    };
    let Ok(arr) = codecs.l() else {
        return;
    };
    if arr.is_null() {
        return;
    }
    let obj_arr = JObjectArray::from(arr);
    let len = env.get_array_length(&obj_arr).unwrap_or(0);
    for i in 0..len {
        let Ok(info) = env.get_object_array_element(&obj_arr, i) else {
            continue;
        };
        if env
            .call_method(&info, "isEncoder", "()Z", &[])
            .ok()
            .and_then(|v| v.z().ok())
            .unwrap_or(true)
        {
            continue;
        }
        let Ok(types) = env.call_method(
            &info,
            "getSupportedTypes",
            "()[Ljava/lang/String;",
            &[],
        ) else {
            continue;
        };
        let Ok(tarr) = types.l() else {
            continue;
        };
        let tarr = JObjectArray::from(tarr);
        let tlen = env.get_array_length(&tarr).unwrap_or(0);
        for ti in 0..tlen {
            let Ok(tobj) = env.get_object_array_element(&tarr, ti) else {
                continue;
            };
            let Ok(tname) = env.get_string((&tobj).into()) else {
                continue;
            };
            let tname: String = tname.into();
            if !tname.starts_with("video/") {
                continue;
            }
            caps.mediacodec_video = true;
            if let Ok(caps_obj) = env.call_method(
                &info,
                "getCapabilitiesForType",
                "(Ljava/lang/String;)Landroid/media/MediaCodecInfo$CodecCapabilities;",
                &[JValue::Object(&tobj)],
            ) {
                if let Ok(cobj) = caps_obj.l() {
                    if !cobj.is_null() {
                        if let Ok(vc) = env.call_method(
                            &cobj,
                            "getVideoCapabilities",
                            "()Landroid/media/MediaCodecInfo$VideoCapabilities;",
                            &[],
                        ) {
                            if let Ok(vobj) = vc.l() {
                                if !vobj.is_null() {
                                    if env
                                        .call_method(
                                            &cobj,
                                            "isSizeSupported",
                                            "(II)Z",
                                            &[JValue::Int(3840), JValue::Int(2160)],
                                        )
                                        .ok()
                                        .and_then(|v| v.z().ok())
                                        .unwrap_or(false)
                                    {
                                        caps.mediacodec_4k = true;
                                    }
                                    if let (Some(uw), Some(uh)) = (
                                        read_range_upper(env, &vobj, "getSupportedWidths"),
                                        read_range_upper(env, &vobj, "getSupportedHeights"),
                                    ) {
                                        if uw as u32 > caps.mediacodec_max_w {
                                            caps.mediacodec_max_w = uw as u32;
                                        }
                                        if uh as u32 > caps.mediacodec_max_h {
                                            caps.mediacodec_max_h = uh as u32;
                                        }
                                        if uw >= 3840 && uh >= 2160 {
                                            caps.mediacodec_4k = true;
                                        }
                                    }
                                }
                            }
                        }
                        if is_hdr_codec_type(&tname) {
                            if let Ok(levels) = env.get_field(
                                &cobj,
                                "profileLevels",
                                "[Landroid/media/MediaCodecInfo$CodecProfileLevel;",
                            ) {
                                if let Ok(larr) = levels.l() {
                                    if !larr.is_null() {
                                        let arr = JObjectArray::from(larr);
                                        let n = env.get_array_length(&arr).unwrap_or(0);
                                        for li in 0..n {
                                            let Ok(pl) =
                                                env.get_object_array_element(&arr, li)
                                            else {
                                                continue;
                                            };
                                            if let Ok(p) = env.get_field(&pl, "profile", "I") {
                                                if let Ok(p) = p.i() {
                                                    if matches!(p, 2 | 4 | 4096 | 8192 | 16384)
                                                    {
                                                        caps.mediacodec_hdr = true;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn is_hdr_codec_type(mime: &str) -> bool {
    mime == "video/hevc" || mime == "video/x-vnd.on2.vp9" || mime == "video/av01"
}

fn read_range_upper(
    env: &mut jni::JNIEnv<'_>,
    vc: &JObject<'_>,
    method: &str,
) -> Option<i32> {
    let range = env
        .call_method(
            vc,
            method,
            "()Landroid/util/Range;",
            &[],
        )
        .ok()
        .and_then(|v| v.l().ok())?;
    if range.is_null() {
        return None;
    }
    env.call_method(&range, "getUpper", "()Ljava/lang/Comparable;", &[])
        .ok()
        .and_then(|v| v.l().ok())
        .and_then(|c| {
            env.call_method(&c, "intValue", "()I", &[])
                .ok()
                .and_then(|v| v.i().ok())
        })
}
