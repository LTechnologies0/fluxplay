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
Also packs `libmediakitandroidhelper.so` (MediaCodec JNI/JavaVM bridge) from the same ABI dirs.
A copy remains under `vendor/android-native-optional/` for reference.

## Android session (rotation + Surface)

- Activity: `screenOrientation=fullSensor` + `configChanges` (no Activity recreate on rotate).
- Java `onConfigurationChanged` / `onResume` / Surface destroy → `stabilizeAndroidSession` + delayed Surface reattach.
- Rust `maintain_android_surface_session` rebinds mpv `wid` when Surface `gen` or size changes.

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

# Default: INFO+ with targets fluxplay::*, iced_winit, profiler
adb logcat -s FluxPlay:V *:S

# Verbose (debug icons/images/player) — rebuild with env, or:
adb shell setprop debug.fluxplay.verbose 1   # reserved; prefer rebuild:
# FLUXPLAY_VERBOSE=1 ./scripts/build-android-apk.sh
# Or wrap:
adb shell "run-as app.fluxplay.android sh -c 'export FLUXPLAY_VERBOSE=1; …'"  # debuggable only

# Profiler lines (target profiler):
# FLUXPLAY_PROFILE=1 RUST_LOG=profiler=trace,fluxplay=debug ./scripts/build-android-apk.sh
```

Severity is preserved in logcat (E/W/I/D/V). Useful filters:

```bash
adb logcat -s FluxPlay:V | rg 'ERROR|WARN|fluxplay::icons|fluxplay::images|profiler|iced_winit|surface'
```

## Notes

- Desktop binary unchanged: `cargo run -p fluxplay` (iced daemon + libmpv).
- Android uses `iced::application` (single window); player UI embeds in-place.
- **Present paths (Phases A–D)**:
  - Prefer `vo=mediacodec_embed` + `hwdec=mediacodec` on a `SurfaceView` under translucent iced (1080p/4K/HDR when the SoC supports it).
  - Fallback: `vo=libmpv` soft RGBA + `mediacodec-copy` (capped ~720p).
  - Optional Phase D: drop-in Vulkan libmpv via `scripts/fetch-libmpv-vulkan.sh`, then `FLUXPLAY_ANDROID_VO=gpu`.
  - Overrides: `FLUXPLAY_ANDROID_PRESENT=surface|soft|gpu`.
- **Lifecycle**: iced_winit drops wgpu surfaces on `Suspended` and recreates them on `Resumed` (fixes black screen after Home / app switch). Requires vendored `vendor/iced_winit` patch.
- Playback: dynamic link to vendored `libmpv.so` (media-kit). ABIs: `arm64-v8a` (phone) + `x86_64` (Waydroid). Backend « FFmpeg » mappe vers libmpv/lavc. Audio: `ao=opensles`. Fallback: `ACTION_VIEW`.
- Device caps (HDR types, refresh Hz, MediaCodec 4K/HDR, surface size) → `files/saf_inbox/device_caps.json`.
- Default `./scripts/build-android-apk.sh` builds a **fat APK** (both ABIs). Use `--target` for single-ABI.
- Mosaic / listes: overlay drag + `on_release` titres; channel thumbs + letter avatars; tinted white PNG chrome icons.
- Phone UI: Fira Sans named font, real system insets via JNI, immersive during playback.
- Waydroid: `adb connect 192.168.240.112:5555`. Prefer fat APK or `--target x86_64-linux-android`.
- Patches live in `vendor/iced` + `vendor/iced_winit` (`[patch.crates-io]`).
- **armeabi-v7a / x86**: not shipped (no vendored libmpv). Add libs under `vendor/android-native/{armeabi-v7a,x86}/` and extend `build_targets` if needed.
