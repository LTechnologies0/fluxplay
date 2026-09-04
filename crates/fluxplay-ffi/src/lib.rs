//! C ABI for Android (JNI/UniFFI) and iOS (Swift) shells.
//!
//! Desktop apps use `fluxplay` (iced). Mobile apps link this crate and call
//! into playlist / EPG logic while decoding with ExoPlayer / AVPlayer.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::sync::Mutex;

use fluxplay_core::m3u;
use fluxplay_core::models::PlaylistBundle;
use fluxplay_player::{target_profile, Platform};

static LAST_ERROR: Mutex<Option<CString>> = Mutex::new(None);
static BUNDLE_JSON: Mutex<Option<CString>> = Mutex::new(None);

fn set_err(msg: impl Into<String>) {
    let s = CString::new(msg.into()).unwrap_or_else(|_| CString::new("error").unwrap());
    *LAST_ERROR.lock().unwrap() = Some(s);
}

/// Returns last error message (borrowed; valid until next call), or null.
#[no_mangle]
pub extern "C" fn fluxplay_last_error() -> *const c_char {
    LAST_ERROR
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.as_ptr())
        .unwrap_or(std::ptr::null())
}

/// Platform id: 0 linux, 1 macos, 2 windows, 3 android, 4 ios, 255 unknown.
#[no_mangle]
pub extern "C" fn fluxplay_platform_id() -> u32 {
    match Platform::current() {
        Platform::Linux => 0,
        Platform::MacOs => 1,
        Platform::Windows => 2,
        Platform::Android => 3,
        Platform::Ios => 4,
        Platform::Unknown => 255,
    }
}

/// Recommended native decoder name for this target (static C string).
#[no_mangle]
pub extern "C" fn fluxplay_recommended_decoder() -> *const c_char {
    let p = target_profile();
    let name = p.preferred_backends.first().copied().unwrap_or("external");
    match name {
        "mpv" => c"mpv".as_ptr(),
        "ffmpeg" => c"ffmpeg".as_ptr(),
        "exoplayer" => c"exoplayer".as_ptr(),
        "avplayer" => c"avplayer".as_ptr(),
        _ => c"external".as_ptr(),
    }
}

/// Parse M3U body → JSON playlist cached internally. Returns 0 on success.
///
/// # Safety
/// `body` must be a valid NUL-terminated UTF-8 C string.
#[no_mangle]
pub unsafe extern "C" fn fluxplay_parse_m3u(body: *const c_char) -> i32 {
    if body.is_null() {
        set_err("null body");
        return -1;
    }
    let cstr = unsafe { CStr::from_ptr(body) };
    let text = match cstr.to_str() {
        Ok(s) => s,
        Err(e) => {
            set_err(e.to_string());
            return -2;
        }
    };
    match m3u::parse_m3u(text, None) {
        Ok(bundle) => store_bundle(bundle),
        Err(e) => {
            set_err(e.to_string());
            -3
        }
    }
}

fn store_bundle(bundle: PlaylistBundle) -> i32 {
    match serde_json::to_string(&bundle) {
        Ok(json) => match CString::new(json) {
            Ok(c) => {
                *BUNDLE_JSON.lock().unwrap() = Some(c);
                0
            }
            Err(e) => {
                set_err(e.to_string());
                -4
            }
        },
        Err(e) => {
            set_err(e.to_string());
            -5
        }
    }
}

/// Pointer to last parsed playlist JSON (NUL-terminated). Valid until next parse.
#[no_mangle]
pub extern "C" fn fluxplay_playlist_json() -> *const c_char {
    BUNDLE_JSON
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.as_ptr())
        .unwrap_or(std::ptr::null())
}

/// Channel count of last parsed playlist.
#[no_mangle]
pub extern "C" fn fluxplay_channel_count() -> i32 {
    let guard = BUNDLE_JSON.lock().unwrap();
    let Some(raw) = guard.as_ref() else {
        return 0;
    };
    let Ok(s) = raw.to_str() else {
        return 0;
    };
    serde_json::from_str::<PlaylistBundle>(s)
        .map(|b| b.channels.len() as i32)
        .unwrap_or(0)
}
