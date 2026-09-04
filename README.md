# FluxPlay

Lecteur IPTV cross-platform en **Rust** — UI Material Expressive (jour/nuit), parsers + providers purs Rust, lecture native **mpv / FFmpeg** (desktop) et **ExoPlayer / AVPlayer** (mobile).

Inspiré de **IPTVnator** (M3U/Xtream/Stalker, EPG, favoris, User-Agent, catch-up) et **Kodi** (pipeline FFmpeg, HW accel, buffers IPTV, reconnect).

## Plateformes

| Cible | UI | Decode | Statut |
|---|---|---|---|
| **Linux** | iced | mpv → ffplay → externe | ✅ app desktop |
| **macOS** | iced | mpv (VideoToolbox) → ffplay → IINA | ✅ |
| **Windows** | iced | mpv (D3D11VA) → ffplay → VLC | ✅ |
| **Android** | Kotlin shell | Media3 ExoPlayer | ✅ cœur FFI (`fluxplay-ffi`) |
| **iOS** | SwiftUI shell | AVPlayer | ✅ cœur FFI |

Voir [`mobile/README.md`](mobile/README.md) pour le bridge JNI/Swift.

## Lancer (desktop)

```bash
# Dépendances lecture (recommandé)
# Fedora: sudo dnf install mpv ffmpeg
# Debian: sudo apt install mpv ffmpeg
# macOS:  brew install mpv ffmpeg
# Windows: winget install mpv / ffmpeg

cargo run -p fluxplay
```

Réglages → choisir **Auto (mpv → FFmpeg)**, HW decode, low-latency.

## Crates

| Crate | Rôle |
|---|---|
| `fluxplay-core` | M3U/M3U+/XMLTV, catch-up, favoris, prefs player |
| `fluxplay-providers` | M3U fetch, Xtream Codes, Stalker Portal |
| `fluxplay-player` | Routage protocoles + **NativePlayer** (mpv IPC / ffplay) |
| `fluxplay-ffi` | `cdylib`/`staticlib` pour Android & iOS |
| `fluxplay` | App iced desktop |

## Qualité IPTV (desktop)

- **mpv** (embarque FFmpeg) : HLS/DASH/RTSP/RTMP/SRT, HW accel, cache, reconnect lavf
- **ffplay** : fallback FFmpeg avec fenêtre native
- Options type Kodi : `hwdec`, cache réseau, demux readahead, low-latency, User-Agent / Referer par source

## Fonctions type IPTVnator

- Sources M3U / Xtream / Stalker / XMLTV
- Favoris ★, récents, EPG now/next sur les lignes
- Catch-up M3U Plus (`catchup`, `catchup-source`, `catchup-days`)
- User-Agent & referer par playlist
- Auto-refresh sources

## CI

GitHub Actions builds desktop binaries for:

| Artifact | Runner |
|---|---|
| `fluxplay-linux-x86_64` | Ubuntu |
| `fluxplay-linux-aarch64` | Ubuntu ARM |
| `fluxplay-macos-x86_64` | macOS 13 Intel |
| `fluxplay-macos-aarch64` | macOS Apple Silicon |
| `fluxplay-windows-x86_64` | Windows |

- **CI** (push/PR): compile + tests on the matrix  
- **Release** (tag `v*` or manual dispatch): upload archives + checksums  

```bash
git tag v0.1.0 && git push origin v0.1.0
```

## Secrets

Never commit IPTV credentials. Use `.env.example` locally:

```bash
cp .env.example .env
# fill FLUXPLAY_XTREAM_* then run examples
```

App state with portals lives under `~/.config/fluxplay/` (gitignored).

