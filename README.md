# FluxPlay

Lecteur IPTV cross-platform en **Rust** — UI Material Expressive (jour/nuit), parsers + providers purs Rust, lecture native **mpv / FFmpeg** (desktop) et **libmpv / Intent** (Android iced).

Inspiré de **IPTVnator** (M3U/Xtream/Stalker, EPG, favoris, User-Agent, catch-up) et **Kodi** (pipeline FFmpeg, HW accel, buffers IPTV, reconnect).

## Plateformes

| Cible | UI | Decode | Statut |
|---|---|---|---|
| **Linux** | iced | libmpv / libav* → CLI → externe | ✅ app desktop |
| **macOS** | iced | libmpv / libav* (VideoToolbox) → CLI → IINA | ✅ |
| **Windows** | iced | libmpv / libav* (D3D11VA) → CLI → VLC | ✅ |
| **Android** | iced NativeActivity (`fluxplay-android`) | vendored libmpv RGBA + ACTION_VIEW | ✅ APK via `scripts/build-android-apk.sh` |
| **iOS** | SwiftUI shell | AVPlayer | 🧪 FFI expérimental |

Voir [`crates/fluxplay-android/README.md`](crates/fluxplay-android/README.md) pour l’APK iced. `mobile/` + `fluxplay-ffi` restent expérimentaux.

## Lancer (desktop)

```bash
# libmpv + FFmpeg (recommandé) — linkage natif, vidéo embarquée dans iced
# Fedora: sudo dnf install mpv-libs-devel ffmpeg-devel
# Debian: sudo apt install libmpv-dev libavcodec-dev libavformat-dev libswscale-dev
# macOS:  brew install mpv ffmpeg

# Build natif + rpath standalone
./scripts/build-native.sh

# Ou classique
cargo run -p fluxplay

# Debug verbeux (mpv + FFmpeg + iced + crates FluxPlay)
FLUXPLAY_VERBOSE=1 cargo run -p fluxplay 2>&1 | tee /tmp/fluxplay-verbose.log
# Affiner :
#   FLUXPLAY_MPV_LOG=debug|trace
#   FLUXPLAY_FFMPEG_LOG=debug|trace
#   RUST_LOG=fluxplay=trace,fluxplay_player=debug,iced=info,iced_winit=debug
# Fichier mpv : /tmp/fluxplay-mpv-verbose.log
```

Réglages → **Auto (libmpv → FFmpeg)**, HW decode, low-latency.

### Standalone / static

| Mode | Commande |
|---|---|
| libmpv partagé + `lib/` à côté du binaire | `./scripts/build-native.sh` puis `scripts/bundle-libmpv.sh` |
| libmpv **statique** (`libmpv.a`) | `FLUXPLAY_STATIC_MPV=1 FLUXPLAY_REQUIRE_LIBMPV=1 cargo build -p fluxplay --release --features static-mpv` |
| Déps statiques manquantes | `FLUXPLAY_MPV_STATIC_DEPS=ass:avcodec:avformat:avutil:...` |

Features Cargo : `native-mpv`, `native-ffmpeg` (libav* → RGBA iced), `static-link`, `bundle-rpath`, `cli-player` (fallback `mpv`/`ffplay`).

`fluxplay-ffi` produit une **`staticlib`** pour Android/iOS (sans libmpv desktop — ExoPlayer/AVPlayer).

## Crates

| Crate | Rôle |
|---|---|
| `fluxplay-core` | M3U/M3U+/XMLTV, catch-up, favoris, prefs player |
| `fluxplay-providers` | M3U fetch, Xtream Codes, Stalker Portal |
| `fluxplay-player` | Routage + **libmpv / libav* FFI** (RGBA embarqué) + fallback CLI |
| `fluxplay-ffi` | `staticlib`/`cdylib` pour Android & iOS |
| `fluxplay` | App iced desktop |

## Qualité IPTV (desktop)

- **libmpv in-process** : HLS/DASH/RTSP/RTMP/SRT, HW accel, cache, reconnect lavf → frames RGBA dans iced
- **FFmpeg natif (libav*)** : demux/decode embarqué dans la fenêtre lecteur (même modèle que libmpv SW)
- **CLI mpv/ffplay** : fallback d’urgence seulement (`cli-player`)
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
git tag v0.2.1 && git push origin v0.2.1
```

## Builds release

```bash
# Production (fat LTO, strip, panic=abort) — défaut `--release`
cargo build -p fluxplay --release
# ou: cargo rel

# Binaire plus compact (opt-level=z)
cargo build -p fluxplay --profile release-size

# Compile plus rapide (thin LTO) — utilisé pour CI
cargo build -p fluxplay --profile release-ci
```

Profils dans le `Cargo.toml` racine : `lto=fat`, `codegen-units=1`, `panic=abort`, `strip=symbols`.

## Logging

Niveaux `tracing` : **TRACE** / **DEBUG** / **INFO** / **WARN** / **ERROR**.

Par défaut (sans `RUST_LOG`) : profilage d’interactions **ON**, logs réseau `fluxplay::net`, profiler `info`.

```bash
cargo run -p fluxplay
# → net.request / net.response, fluxplay::profile interactions, player backend

# Couper le profilage
FLUXPLAY_PROFILE=0 cargo run -p fluxplay

# Overlay FPS dans la barre de statut
FLUXPLAY_FPS=1 cargo run -p fluxplay

# TRACE complet
RUST_LOG=profiler=trace,fluxplay=trace,fluxplay_providers=trace,fluxplay_player=trace cargo run -p fluxplay
```

Chaque `update`/`view`/`Stopwatch` : `wall_ns`/`wall_ms`, `% CPU/cœur`, RSS. Les appels HTTP Xtream/images sortent sous `fluxplay::net`.

## Catalogue local

- SQLite : `~/.local/share/fluxplay/catalog.sqlite3` (live / VOD / séries)
- Au démarrage : hydrate depuis la DB, puis sync Xtream **seulement si le cache a >12h**
- Recherche VOD/Séries = FTS5 locale (pas d’API portal)
- Images : cache disque `~/.cache/fluxplay/images/` + LRU RAM (96)
- Métadonnées : TVMaze + OMDb (`OMDB_API_KEY`), une seule fois (`meta_ok`)
- UI mosaïque (24 tuiles/page) type MYTV Online
- La démo n’est chargée que s’il n’y a **aucune** vraie source

### Optimisations appliquées (recherche EN)

| Domaine | Techniques |
|---|---|
| **SQLite** | WAL, `synchronous=NORMAL`, `temp_store=MEMORY`, `busy_timeout`, page `cache_size`, `mmap_size`, `PRAGMA optimize`, transactions bulk, FTS5, indexes catégorie/meta |
| **Offline-first** | DB = source of truth, stale-while-revalidate (12h), NetworkBoundResource-style boot |
| **HTTP** | Client partagé + pool, sémaphore portal=1, TTL API longs (12–24h), stale-on-429 |
| **Images** | Shared client, disk content-addressed, RAM LRU, cap taille, skip HTML |
| **UI** | Pages courtes (24), prefetch art ≤8, enrich throttle, pas de rebuild catalogue chaud |

```bash
cp .env.example .env
# fill FLUXPLAY_XTREAM_* then run examples
```

App state with portals lives under `~/.config/fluxplay/` (gitignored).

