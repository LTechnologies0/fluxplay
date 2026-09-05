//! FluxPlay Android entry — boots the same iced desktop UI via NativeActivity.

#[cfg(target_os = "android")]
#[no_mangle]
fn android_main(app: android_activity::AndroidApp) {
    if let Err(e) = fluxplay::run_android(app) {
        log::error!("FluxPlay iced exited: {e:?}");
    }
}
