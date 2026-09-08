//! Access the global Android application object.
//!
//! On Android, the [`AndroidApp`] handed to the entry point is the only way to
//! reach platform APIs—like the app's internal data path.
//!
//! [`AndroidApp`]: winit::platform::android::activity::AndroidApp

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use winit::platform::android::activity::AndroidApp;

/// Global [`AndroidApp`] set by [`crate::run_android`].
pub static ANDROID_APP: OnceLock<AndroidApp> = OnceLock::new();

/// `true` while the Activity is resumed (NativeWindow may exist).
/// Cleared on `Suspended` so apps can pause media before surfaces are gone.
static FOREGROUND: AtomicBool = AtomicBool::new(true);

/// Whether the Android Activity is in the foreground (not Suspended).
pub fn is_foreground() -> bool {
    FOREGROUND.load(Ordering::SeqCst)
}

pub(crate) fn set_foreground(v: bool) {
    FOREGROUND.store(v, Ordering::SeqCst);
}

/// `true` while [`AndroidApp::native_window`] is available (InitWindow…TerminateWindow).
///
/// Creating a wgpu surface without this handle panics with
/// `CreateSurfaceError::RawHandle(Unavailable)`.
pub fn has_native_window() -> bool {
    ANDROID_APP
        .get()
        .and_then(|app| app.native_window())
        .is_some()
}
