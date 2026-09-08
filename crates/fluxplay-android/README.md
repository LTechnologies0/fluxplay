# FluxPlay on Android / Android TV (pure Rust + iced desktop UI)

Same **iced** UI as the desktop app (`crates/fluxplay`), forced onto Android via a patched `iced` / `iced_winit` (`vendor/`) with `run_android` + deferred window create until `Resumed`.

| Layer | Choice |
|-------|--------|
| Activity | `app.fluxplay.android.FluxPlayNativeActivity` (NativeActivity subclass) |
| UI | **iced 0.14** (same widgets / themes / browser as desktop) |
| GPU | `wgpu` (GLES forced on Android: `WGPU_BACKEND=gl`) |
| Catalog | `fluxplay-core` + `fluxplay-providers` + SQLite |
| Playback | **in-process libmpv** (vendored `vendor/android-native/*/libmpv.so`) → RGBA embed like desktop; `ACTION_VIEW` Intent fallback |

## Native libs

Prebuilt **media-kit** `libmpv.so` (arm64-v8a + x86_64) lives under:

```
vendor/android-native/arm64-v8a/libmpv.so
vendor/android-native/x86_64/libmpv.so
vendor/android-native/include/mpv/*.h
```

`cargo-apk` packs them via `[package.metadata.android] runtime_libs`.
Unused `libmediakitandroidhelper.so` lives under `vendor/android-native-optional/` (not packaged).

## Build

```bash
export ANDROID_HOME=$HOME/Android/Sdk
export ANDROID_NDK_ROOT=$ANDROID_HOME/ndk/28.0.13004108   # or your NDK
unset ANDROID_SDK_ROOT

# Recommended: cargo-apk + Java inject (SAF / PiP / insets / audio focus) in one step
./scripts/build-android-apk.sh

# Or manually:
cargo apk build -p fluxplay-android --profile release-ci
./scripts/inject-android-java.sh target/release-ci/apk/FluxPlay.apk

# Single ABI only (smaller):
./scripts/build-android-apk.sh --target aarch64-linux-android
```

Signing for the inject step (optional; defaults to `~/.android/debug.keystore`):

```bash
export FLUXPLAY_ANDROID_KS=~/.android/debug.keystore
export FLUXPLAY_ANDROID_KS_PASS=android
export FLUXPLAY_ANDROID_KEY_ALIAS=androiddebugkey
export FLUXPLAY_ANDROID_KEY_PASS=android
```

Cleartext HTTP is enabled for IPTV. WireGuard uses the same userspace `wg-socks` SOCKS path as desktop (app-scoped, not VpnService).
Activity: `app.fluxplay.android.FluxPlayNativeActivity`.

## Install / logs

```bash
adb install -r -g target/release-ci/apk/FluxPlay.apk
adb shell am start -W -n app.fluxplay.android/app.fluxplay.android.FluxPlayNativeActivity
adb logcat -s FluxPlay:I *:S
```

## Notes

- Desktop binary unchanged: `cargo run -p fluxplay` (iced daemon + libmpv).
- Android uses `iced::application` (single window); player UI embeds in-place via `vo=libmpv` soft RGBA.
- **Lifecycle**: iced_winit drops wgpu surfaces on `Suspended` and recreates them on `Resumed` (fixes black screen after Home / app switch). Requires vendored `vendor/iced_winit` patch.
- Playback: dynamic link to vendored `libmpv.so` (media-kit). ABIs: `arm64-v8a` (phone) + `x86_64` (Waydroid). Backend « FFmpeg » mappe vers libmpv/lavc. Audio: `ao=opensles`. Fallback: `ACTION_VIEW`.
- Mosaic / listes: overlay drag + `on_release` titres (slop ~12px).
- Phone UI: letter/`#` posters (GLES quirk), barre nav 1 rangée `FillPortion`, real system insets via JNI (no fake 56dp), Fira Sans + `advanced-shaping`.
- Waydroid: `adb connect 192.168.240.112:5555`, build `--target x86_64-linux-android`. Pixel-like: `wm size 1080x2400` + `wm density 420`.
- Patches live in `vendor/iced` + `vendor/iced_winit` (`[patch.crates-io]`).
