//! Android platform APIs in Rust via JNI (MediaCodec caps, audio focus, MediaSession).
//!
//! SurfaceView / SAF / PiP stay in the thin Java activity; everything else is here.

mod audio_focus;
mod connectivity;
mod device_caps;
mod jni_host;
mod media_controls;
mod media_session;
mod notifications;
mod window;

pub use audio_focus::{abandon, on_focus_change, poll_held, request};
pub use connectivity::{
    is_online, on_network_changed, register_default_callback, take_reconnect_pulse,
    unregister_default_callback,
};
pub use device_caps::poll_device_caps;
pub use media_controls::{on_media_action, take_pending, MediaAction};
pub use notifications::ensure_post_notifications_permission;
pub use media_session::{hide_playback_notification, sync_playback_notification};
pub use window::{
    in_pip_mode, insets_dp, is_night_mode, refresh_insets, set_display_color_mode,
    set_immersive_mode, set_keep_screen_on,
};
