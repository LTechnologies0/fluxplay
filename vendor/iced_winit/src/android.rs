//! Access the global Android application object.
//!
//! On Android, the [`AndroidApp`] handed to the entry point is the only way to
//! reach platform APIs—like the app's internal data path.
//!
//! [`AndroidApp`]: winit::platform::android::activity::AndroidApp

use std::sync::OnceLock;

use winit::platform::android::activity::AndroidApp;

/// Global [`AndroidApp`] set by [`crate::run_android`].
pub static ANDROID_APP: OnceLock<AndroidApp> = OnceLock::new();
