# FluxPlay mobile (Android / iOS)

Le cœur IPTV (playlists, Xtream, Stalker, EPG, routage) est en Rust et exposé via **`fluxplay-ffi`** (`cdylib` / `staticlib`).

| OS | UI shell | Decode vidéo | Lien Rust |
|---|---|---|---|
| **Android** | Kotlin + Jetpack Compose | **Media3 ExoPlayer** (HLS/DASH, MediaCodec) | JNI → `libfluxplay_ffi.so` |
| **iOS** | SwiftUI | **AVPlayer** (VideoToolbox) | Swift bridging → `libfluxplay_ffi.a` |

Desktop (Windows / macOS / Linux) utilise l’app **iced** + **mpv** / **FFmpeg** — voir README racine.

## Build bibliothèque

```bash
# Android (exemples)
cargo build -p fluxplay-ffi --target aarch64-linux-android --release
cargo build -p fluxplay-ffi --target armv7-linux-androideabi --release
cargo build -p fluxplay-ffi --target x86_64-linux-android --release

# iOS (sur macOS + Xcode)
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
cargo build -p fluxplay-ffi --target aarch64-apple-ios --release
```

## API C minimale

```c
int32_t fluxplay_parse_m3u(const char *body);
const char *fluxplay_playlist_json(void);
int32_t fluxplay_channel_count(void);
uint32_t fluxplay_platform_id(void);
const char *fluxplay_recommended_decoder(void);
const char *fluxplay_last_error(void);
```

## Intégration lecture

1. Parser / charger la playlist via FFI (ou Xtream/Stalker côté Rust étendu).
2. Passer `stream_url` + headers (`User-Agent`) au player natif.
3. Ne **pas** embarquer mpv sur mobile store — ExoPlayer / AVPlayer sont la voie officielle (comme TiviMate / apps App Store).

## Prochaines étapes shell

- `android/`: module Gradle `fluxplay` + `System.loadLibrary("fluxplay_ffi")`
- `ios/`: Xcode package + `module.modulemap` pour les symboles C
