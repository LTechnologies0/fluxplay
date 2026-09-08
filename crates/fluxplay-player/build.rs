//! Locate libmpv / FFmpeg and emit link flags + compile native embed sources.
//!
//! If a library is missing, the matching FFI is disabled via cfg
//! (`fluxplay_has_libmpv` / `fluxplay_has_ffmpeg`) and CLI fallback remains available.
//!
//! Env (mpv):
//! - `MPV_PREFIX` / `MPV_LIB_DIR` / `MPV_INCLUDE_DIR`
//! - `FLUXPLAY_STATIC_MPV=1` or feature `static-link`
//! - `FLUXPLAY_BUNDLE_RPATH=1`
//! - `FLUXPLAY_REQUIRE_LIBMPV=1`
//!
//! Env (ffmpeg):
//! - `FFMPEG_PREFIX` / `FFMPEG_LIB_DIR` / `FFMPEG_INCLUDE_DIR`
//! - `FLUXPLAY_REQUIRE_FFMPEG=1`

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rustc-check-cfg=cfg(fluxplay_has_libmpv)");
    println!("cargo:rustc-check-cfg=cfg(fluxplay_has_ffmpeg)");

    let target = env::var("TARGET").unwrap_or_default();
    let is_android = target.contains("android");

    if env::var_os("CARGO_FEATURE_NATIVE_MPV").is_some() {
        setup_libmpv();
    }
    // FFmpeg C embed needs separate libav* — not shipped in media-kit libmpv.so.
    // On Android we rely on libmpv (which embeds codecs) for soft RGBA.
    if env::var_os("CARGO_FEATURE_NATIVE_FFMPEG").is_some() && !is_android {
        setup_ffmpeg();
    } else if is_android && env::var_os("CARGO_FEATURE_NATIVE_FFMPEG").is_some() {
        println!("cargo:warning=Android: native-ffmpeg skipped (use libmpv embed)");
    }
}

fn setup_libmpv() {
    println!("cargo:rerun-if-env-changed=MPV_PREFIX");
    println!("cargo:rerun-if-env-changed=MPV_LIB_DIR");
    println!("cargo:rerun-if-env-changed=MPV_INCLUDE_DIR");
    println!("cargo:rerun-if-env-changed=FLUXPLAY_STATIC_MPV");
    println!("cargo:rerun-if-env-changed=FLUXPLAY_BUNDLE_RPATH");
    println!("cargo:rerun-if-env-changed=FLUXPLAY_REQUIRE_LIBMPV");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");

    let want_static = env::var_os("CARGO_FEATURE_STATIC_LINK").is_some()
        || env::var("FLUXPLAY_STATIC_MPV").ok().as_deref() == Some("1");
    let require = env::var("FLUXPLAY_REQUIRE_LIBMPV").ok().as_deref() == Some("1")
        || env::var_os("CARGO_FEATURE_STATIC_LINK").is_some()
        || env::var("TARGET").unwrap_or_default().contains("android");

    let lib_dir = discover_mpv_lib_dir();
    let include_dir = discover_mpv_include_dir(&lib_dir);

    let header_ok = include_dir
        .as_ref()
        .map(|inc| Path::new(inc).join("mpv").join("client.h").is_file())
        .unwrap_or(false);
    let lib_ok = lib_dir.is_some();

    if !lib_ok || !header_ok {
        let msg = format!(
            "libmpv not found (lib_dir={lib_dir:?}, include={include_dir:?}). \
             Native FFI disabled — install libmpv-dev / brew mpv, or set MPV_LIB_DIR."
        );
        if require {
            panic!("{msg}");
        }
        println!("cargo:warning={msg}");
        return;
    }

    let lib_dir = lib_dir.expect("lib_ok");
    let include_dir = include_dir.expect("header_ok");
    println!(
        "cargo:rerun-if-changed={}",
        Path::new(&include_dir).join("mpv").join("client.h").display()
    );

    println!("cargo:rustc-link-search=native={}", lib_dir.display());

    let static_archive = lib_dir.join("libmpv.a");
    if want_static && static_archive.is_file() {
        println!("cargo:rustc-link-lib=static=mpv");
        println!(
            "cargo:warning=linking static libmpv ({})",
            static_archive.display()
        );
        if let Ok(deps) = env::var("FLUXPLAY_MPV_STATIC_DEPS") {
            for dep in deps.split(|c| c == ':' || c == ',' || c == ' ') {
                let dep = dep.trim();
                if !dep.is_empty() {
                    println!("cargo:rustc-link-lib=static={dep}");
                }
            }
        } else {
            println!(
                "cargo:warning=static libmpv: set FLUXPLAY_MPV_STATIC_DEPS if the linker misses codecs"
            );
        }
        for sys in ["m", "pthread", "dl"] {
            println!("cargo:rustc-link-lib={sys}");
        }
        #[cfg(target_os = "linux")]
        println!("cargo:rustc-link-lib=atomic");
    } else {
        if want_static {
            println!(
                "cargo:warning=static-link demandé mais {} absent — linkage dynamique",
                static_archive.display()
            );
        }
        println!("cargo:rustc-link-lib=dylib=mpv");
    }

    let bundle_rpath = env::var("FLUXPLAY_BUNDLE_RPATH").ok().as_deref() == Some("1")
        || env::var_os("CARGO_FEATURE_BUNDLE_RPATH").is_some();
    let target = env::var("TARGET").unwrap_or_default();
    let cross_android = target.contains("android");
    if bundle_rpath && !cross_android {
        if cfg!(target_os = "linux") {
            println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/lib");
        } else if cfg!(target_os = "macos") {
            println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/lib");
        }
    }
    // Never inject host rpath when cross-compiling for Android.
    if !cross_android && (cfg!(target_os = "linux") || cfg!(target_os = "macos")) {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir.display());
    }

    println!("cargo:rustc-cfg=fluxplay_has_libmpv");
}

fn setup_ffmpeg() {
    println!("cargo:rerun-if-env-changed=FFMPEG_PREFIX");
    println!("cargo:rerun-if-env-changed=FFMPEG_LIB_DIR");
    println!("cargo:rerun-if-env-changed=FFMPEG_INCLUDE_DIR");
    println!("cargo:rerun-if-env-changed=FLUXPLAY_REQUIRE_FFMPEG");
    println!("cargo:rerun-if-changed=native/ffmpeg_embed.c");
    println!("cargo:rerun-if-changed=native/ffmpeg_embed.h");

    let require = env::var("FLUXPLAY_REQUIRE_FFMPEG").ok().as_deref() == Some("1");
    let include_dir = discover_ffmpeg_include_dir();
    let lib_dir = discover_ffmpeg_lib_dir();

    let header_ok = include_dir
        .as_ref()
        .is_some_and(|inc| ffmpeg_headers_ok(Path::new(inc)));

    let link_specs = lib_dir
        .as_ref()
        .and_then(|d| ffmpeg_link_specs(d));

    if !header_ok || link_specs.is_none() {
        let msg = format!(
            "FFmpeg not found (include={include_dir:?}, lib={lib_dir:?}). \
             Native embed disabled — install ffmpeg-devel / ffmpeg-libs, or set FFMPEG_*."
        );
        if require {
            panic!("{msg}");
        }
        println!("cargo:warning={msg}");
        return;
    }

    let include_dir = include_dir.expect("header_ok");
    let lib_dir = lib_dir.expect("link_specs");
    let link_specs = link_specs.expect("link_specs");

    let native = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("native");
    let mut build = cc::Build::new();
    build
        .file(native.join("ffmpeg_embed.c"))
        .include(&native)
        .include(&include_dir)
        .warnings(false)
        .flag_if_supported("-std=c11");
    if cfg!(target_os = "linux") {
        build.define("_GNU_SOURCE", None);
    }
    build.compile("flux_ffmpeg_embed");

    // rust-lld often fails on `-l:libavcodec.so.62`; expose unversioned names via OUT_DIR.
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let link_dir = out.join("ffmpeg_link");
    let _ = std::fs::create_dir_all(&link_dir);
    for spec in &link_specs {
        if let Some(short) = spec
            .strip_prefix("lib")
            .and_then(|s| s.split(".so").next())
        {
            // versioned: libavcodec.so.62 → symlink libavcodec.so
            let dest = link_dir.join(format!("lib{short}.so"));
            let _ = std::fs::remove_file(&dest);
            let src = lib_dir.join(spec);
            if src.is_file() {
                #[cfg(unix)]
                {
                    let _ = std::os::unix::fs::symlink(&src, &dest);
                }
                #[cfg(not(unix))]
                {
                    let _ = std::fs::copy(&src, &dest);
                }
            }
            println!("cargo:rustc-link-lib=dylib={short}");
        } else {
            // already a short name (avcodec)
            println!("cargo:rustc-link-lib=dylib={spec}");
        }
    }
    println!("cargo:rustc-link-search=native={}", link_dir.display());
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=pthread");
    println!("cargo:rustc-link-lib=m");
    if cfg!(target_os = "linux") || cfg!(target_os = "macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir.display());
    }

    println!("cargo:rustc-cfg=fluxplay_has_ffmpeg");
    println!(
        "cargo:warning=native FFmpeg embed enabled (include={}, lib={})",
        include_dir.display(),
        lib_dir.display()
    );
}

fn discover_mpv_lib_dir() -> Option<PathBuf> {
    if let Ok(d) = env::var("MPV_LIB_DIR") {
        return Some(PathBuf::from(d));
    }
    if let Ok(prefix) = env::var("MPV_PREFIX") {
        let lib = PathBuf::from(prefix).join("lib");
        if lib.is_dir() {
            return Some(lib);
        }
    }
    // Android NDK: vendored media-kit libmpv (self-contained .so).
    let target = env::var("TARGET").unwrap_or_default();
    if target.contains("android") {
        let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
        let abi = if target.contains("aarch64") {
            "arm64-v8a"
        } else if target.contains("armv7") {
            "armeabi-v7a"
        } else if target.contains("x86_64") {
            "x86_64"
        } else {
            "x86"
        };
        let vendored = manifest
            .join("../../vendor/android-native")
            .join(abi);
        if vendored.join("libmpv.so").is_file() {
            println!(
                "cargo:warning=using vendored Android libmpv ({})",
                vendored.display()
            );
            return Some(vendored);
        }
    }
    if is_apple_darwin_arch_cross() {
        println!(
            "cargo:warning=skipping host Homebrew libmpv (TARGET≠HOST on apple-darwin); \
             set MPV_LIB_DIR for a matching-arch build or rely on CLI mpv fallback"
        );
        return None;
    }
    for candidate in candidate_lib_dirs() {
        if candidate.join("libmpv.so").is_file()
            || candidate.join("libmpv.dylib").is_file()
            || candidate.join("libmpv.a").is_file()
            || candidate.join("mpv.lib").is_file()
            || candidate.join("libmpv.dll.a").is_file()
        {
            return Some(candidate);
        }
        if std::fs::read_dir(&candidate).ok().is_some_and(|rd| {
            rd.flatten().any(|e| {
                let n = e.file_name();
                let n = n.to_string_lossy();
                n.starts_with("libmpv.so") || n.starts_with("libmpv.dylib")
            })
        }) {
            return Some(candidate);
        }
    }
    None
}

fn is_apple_darwin_arch_cross() -> bool {
    let target = env::var("TARGET").unwrap_or_default();
    let host = env::var("HOST").unwrap_or_default();
    target.contains("apple-darwin")
        && host.contains("apple-darwin")
        && !target.is_empty()
        && !host.is_empty()
        && target != host
}

fn discover_mpv_include_dir(lib_dir: &Option<PathBuf>) -> Option<PathBuf> {
    if let Ok(d) = env::var("MPV_INCLUDE_DIR") {
        return Some(PathBuf::from(d));
    }
    if let Ok(prefix) = env::var("MPV_PREFIX") {
        let inc = PathBuf::from(prefix).join("include");
        if inc.join("mpv").join("client.h").is_file() {
            return Some(inc);
        }
    }
    let target = env::var("TARGET").unwrap_or_default();
    if target.contains("android") {
        let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
        let vendored = manifest.join("../../vendor/android-native/include");
        if vendored.join("mpv").join("client.h").is_file() {
            return Some(vendored);
        }
    }
    if let Some(lib) = lib_dir {
        if let Some(prefix) = lib.parent() {
            let inc = prefix.join("include");
            if inc.join("mpv").join("client.h").is_file() {
                return Some(inc);
            }
        }
    }
    for candidate in candidate_include_dirs() {
        if candidate.join("mpv").join("client.h").is_file() {
            return Some(candidate);
        }
    }
    None
}

fn discover_ffmpeg_include_dir() -> Option<PathBuf> {
    if let Ok(d) = env::var("FFMPEG_INCLUDE_DIR") {
        return Some(PathBuf::from(d));
    }
    if let Ok(prefix) = env::var("FFMPEG_PREFIX") {
        let inc = PathBuf::from(prefix).join("include");
        if ffmpeg_headers_ok(&inc) {
            return Some(inc);
        }
    }
    // Prefer vendored FFmpeg 8.x headers when linking distro .so.62 (ABI match).
    let vendored = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("vendor")
        .join("ffmpeg-include");
    if ffmpeg_headers_ok(&vendored) {
        return Some(vendored);
    }
    for candidate in candidate_include_dirs() {
        if ffmpeg_headers_ok(&candidate) {
            return Some(candidate);
        }
    }
    for candidate in [
        PathBuf::from("/tmp/ffmpeg-8.1"),
        PathBuf::from("/tmp/ffmpeg"),
    ] {
        if ffmpeg_headers_ok(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn ffmpeg_headers_ok(inc: &Path) -> bool {
    inc.join("libavformat").join("avformat.h").is_file()
        && inc.join("libavcodec").join("avcodec.h").is_file()
        && inc.join("libswscale").join("swscale.h").is_file()
        && inc.join("libavutil").join("avconfig.h").is_file()
}

fn discover_ffmpeg_lib_dir() -> Option<PathBuf> {
    if let Ok(d) = env::var("FFMPEG_LIB_DIR") {
        return Some(PathBuf::from(d));
    }
    if let Ok(prefix) = env::var("FFMPEG_PREFIX") {
        let lib = PathBuf::from(&prefix).join("lib");
        if lib.is_dir() {
            return Some(lib);
        }
        let lib64 = PathBuf::from(&prefix).join("lib64");
        if lib64.is_dir() {
            return Some(lib64);
        }
    }
    // Prefer distro libs (often CUDA/NVDEC-enabled) over Homebrew (often SW-only).
    let mut dirs = vec![
        PathBuf::from("/usr/lib64"),
        PathBuf::from("/usr/lib"),
        PathBuf::from("/usr/lib/x86_64-linux-gnu"),
        PathBuf::from("/usr/lib/aarch64-linux-gnu"),
    ];
    dirs.extend(candidate_lib_dirs());
    for candidate in dirs {
        if ffmpeg_link_specs(&candidate).is_some() {
            return Some(candidate);
        }
    }
    None
}

/// Returns linker names: either short names (`avcodec`) or versioned (`libavcodec.so.62`).
fn ffmpeg_link_specs(lib_dir: &Path) -> Option<Vec<String>> {
    const NEEDED: &[(&str, &str)] = &[
        ("avcodec", "libavcodec"),
        ("avformat", "libavformat"),
        ("avutil", "libavutil"),
        ("swscale", "libswscale"),
    ];
    let mut out = Vec::with_capacity(NEEDED.len());
    for &(short, stem) in NEEDED {
        let unversioned = lib_dir.join(format!("lib{short}.so"));
        let dylib = lib_dir.join(format!("lib{short}.dylib"));
        let dll = lib_dir.join(format!("{short}.lib"));
        if unversioned.is_file() || dylib.is_file() || dll.is_file() {
            out.push(short.to_string());
            continue;
        }
        // Fedora/RHEL: only libavcodec.so.N
        let versioned = std::fs::read_dir(lib_dir).ok().and_then(|rd| {
            let mut best: Option<String> = None;
            for e in rd.flatten() {
                let n = e.file_name();
                let n = n.to_string_lossy();
                if n.starts_with(&format!("{stem}.so.")) && !n.contains(".debug") {
                    // Prefer shortest (libavcodec.so.62 over libavcodec.so.62.28.102)
                    let pick = match &best {
                        None => true,
                        Some(b) => n.len() < b.len(),
                    };
                    if pick {
                        best = Some(n.into_owned());
                    }
                }
            }
            best
        });
        if let Some(soname) = versioned {
            out.push(soname);
        } else {
            return None;
        }
    }
    Some(out)
}

fn candidate_lib_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for key in ["HOMEBREW_PREFIX", "HOMEBREW_REPOSITORY"] {
        if let Ok(p) = env::var(key) {
            out.push(PathBuf::from(&p).join("lib"));
            out.push(PathBuf::from(&p).join("opt/mpv/lib"));
            out.push(PathBuf::from(&p).join("opt/ffmpeg/lib"));
        }
    }
    out.extend([
        PathBuf::from("/home/linuxbrew/.linuxbrew/lib"),
        PathBuf::from("/home/linuxbrew/.linuxbrew/opt/mpv/lib"),
        PathBuf::from("/home/linuxbrew/.linuxbrew/opt/ffmpeg/lib"),
        PathBuf::from("/opt/homebrew/lib"),
        PathBuf::from("/opt/homebrew/opt/mpv/lib"),
        PathBuf::from("/opt/homebrew/opt/ffmpeg/lib"),
        PathBuf::from("/usr/local/lib"),
        PathBuf::from("/usr/local/opt/mpv/lib"),
        PathBuf::from("/usr/local/opt/ffmpeg/lib"),
        PathBuf::from("/usr/lib"),
        PathBuf::from("/usr/lib64"),
        PathBuf::from("/usr/lib/x86_64-linux-gnu"),
        PathBuf::from("/usr/lib/aarch64-linux-gnu"),
    ]);
    out
}

fn candidate_include_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = env::var("HOMEBREW_PREFIX") {
        out.push(PathBuf::from(&p).join("include"));
        out.push(PathBuf::from(&p).join("opt/mpv/include"));
        out.push(PathBuf::from(&p).join("opt/ffmpeg/include"));
    }
    out.extend([
        PathBuf::from("/home/linuxbrew/.linuxbrew/include"),
        PathBuf::from("/home/linuxbrew/.linuxbrew/opt/mpv/include"),
        PathBuf::from("/home/linuxbrew/.linuxbrew/opt/ffmpeg/include"),
        PathBuf::from("/opt/homebrew/include"),
        PathBuf::from("/opt/homebrew/opt/mpv/include"),
        PathBuf::from("/opt/homebrew/opt/ffmpeg/include"),
        PathBuf::from("/usr/local/include"),
        PathBuf::from("/usr/include"),
    ]);
    out
}
