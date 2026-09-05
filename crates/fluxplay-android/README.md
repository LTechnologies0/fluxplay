# FluxPlay on Android / Android TV (pure Rust + iced desktop UI)

Same **iced** UI as the desktop app (`crates/fluxplay`), forced onto Android via a patched `iced` / `iced_winit` (`vendor/`) with `run_android` + deferred window create until `Resumed`.

| Layer | Choice |
|-------|--------|
| Activity | `android-activity` **NativeActivity** |
| UI | **iced 0.14** (same widgets / themes / browser as desktop) |
| GPU | `wgpu` (GLES forced on Android: `WGPU_BACKEND=gl`) |
| Catalog | `fluxplay-core` + `fluxplay-providers` + SQLite |
| Playback | `ACTION_VIEW` Intent → system / leanback player (no libmpv NDK yet) |

## Build

```bash
export ANDROID_HOME=$HOME/Android/Sdk
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/28.0.13004108   # or your NDK
unset ANDROID_SDK_ROOT

# Waydroid (x86_64) — prefer release-ci (debug hits android-activity debug_assert)
cargo apk build -p fluxplay-android --target x86_64-linux-android --profile release-ci

# Phone / Android TV (arm64)
cargo apk build -p fluxplay-android --target aarch64-linux-android --profile release-ci
```

APK: `target/release-ci/apk/FluxPlay.apk`

Signing uses the Android debug keystore (see `Cargo.toml` metadata).

## Waydroid

```bash
adb connect 192.168.240.112:5555
adb install -r -g target/release-ci/apk/FluxPlay.apk
adb shell am start -W -n app.fluxplay.android/android.app.NativeActivity
adb logcat -s FluxPlay
```

## Notes

- Desktop binary unchanged: `cargo run -p fluxplay` (iced daemon + libmpv).
- Android uses `iced::application` (single window); player UI embeds in-place.
- Play opens the system player via JNI `ACTION_VIEW`.
- Patches live in `vendor/iced` + `vendor/iced_winit` (`[patch.crates-io]`).
