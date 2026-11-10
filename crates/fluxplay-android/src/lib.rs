//! FluxPlay Android entry — boots the same iced desktop UI via NativeActivity.

#[cfg(target_os = "android")]
#[no_mangle]
fn android_main(app: android_activity::AndroidApp) {
    if let Err(e) = fluxplay::run_android(app) {
        log::error!("FluxPlay iced exited: {e:?}");
    }
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_app_fluxplay_android_FluxPlayNativeActivity_nativeOnAudioFocusChange(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
    change: i32,
) {
    fluxplay::android_platform::on_focus_change(change);
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_app_fluxplay_android_FluxPlayNativeActivity_nativeRequestAudioFocus(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
) {
    fluxplay::android_platform::request();
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_app_fluxplay_android_FluxPlayNativeActivity_nativeAbandonAudioFocus(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
) {
    fluxplay::android_platform::abandon();
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_app_fluxplay_android_FluxPlayNativeActivity_nativeNetworkChanged(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
    online: u8,
) {
    fluxplay::android_platform::on_network_changed(online != 0);
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_app_fluxplay_android_FluxPlayNativeActivity_nativeMediaAction(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
    code: i32,
) {
    fluxplay::android_platform::on_media_action(code);
}
