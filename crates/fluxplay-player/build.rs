//! Locate libmpv and emit link flags (shared by default, static when requested).
//!
//! If libmpv is missing, the crate still builds: native FFI is disabled via
//! `cfg(fluxplay_has_libmpv)` and CLI fallback (if enabled) remains available.
//!
//! Env:
//! - `MPV_PREFIX` / `MPV_LIB_DIR` / `MPV_INCLUDE_DIR` — override discovery
//! - `FLUXPLAY_STATIC_MPV=1` or feature `static-link` — prefer `libmpv.a`
//! - `FLUXPLAY_BUNDLE_RPATH=1` — add `$ORIGIN/lib` rpath for portable bundles
//! - `FLUXPLAY_REQUIRE_LIBMPV=1` — fail the build if libmpv cannot be found

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    if env::var_os("CARGO_FEATURE_NATIVE_MPV").is_none() {
        return;
    }

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
        || env::var_os("CARGO_FEATURE_STATIC_LINK").is_some();

    let lib_dir = discover_lib_dir();
    let include_dir = discover_include_dir(&lib_dir);

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
        println!("cargo:warning=linking static libmpv ({})", static_archive.display());
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
    if bundle_rpath {
        if cfg!(target_os = "linux") {
            println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/lib");
        } else if cfg!(target_os = "macos") {
            println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/lib");
        }
    }
    if cfg!(target_os = "linux") || cfg!(target_os = "macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir.display());
    }

    println!("cargo:rustc-cfg=fluxplay_has_libmpv");
    // Allow `#[cfg(fluxplay_has_libmpv)]` in this crate and dependents that opt in.
    println!("cargo:rustc-check-cfg=cfg(fluxplay_has_libmpv)");
}

fn discover_lib_dir() -> Option<PathBuf> {
    if let Ok(d) = env::var("MPV_LIB_DIR") {
        return Some(PathBuf::from(d));
    }
    if let Ok(prefix) = env::var("MPV_PREFIX") {
        let lib = PathBuf::from(prefix).join("lib");
        if lib.is_dir() {
            return Some(lib);
        }
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

fn discover_include_dir(lib_dir: &Option<PathBuf>) -> Option<PathBuf> {
    if let Ok(d) = env::var("MPV_INCLUDE_DIR") {
        return Some(PathBuf::from(d));
    }
    if let Ok(prefix) = env::var("MPV_PREFIX") {
        let inc = PathBuf::from(prefix).join("include");
        if inc.join("mpv").join("client.h").is_file() {
            return Some(inc);
        }
    }
    if let Some(lib) = lib_dir {
        if let Some(prefix) = lib.parent() {
            let inc = prefix.join("include");
            if inc.join("mpv").join("client.h").is_file() {
                return Some(inc);
            }
        }
        if let Some(ver) = lib.parent() {
            let inc = ver.join("include");
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

fn candidate_lib_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for key in ["HOMEBREW_PREFIX", "HOMEBREW_REPOSITORY"] {
        if let Ok(p) = env::var(key) {
            out.push(PathBuf::from(&p).join("lib"));
            out.push(PathBuf::from(&p).join("opt/mpv/lib"));
        }
    }
    out.extend([
        PathBuf::from("/home/linuxbrew/.linuxbrew/lib"),
        PathBuf::from("/home/linuxbrew/.linuxbrew/opt/mpv/lib"),
        PathBuf::from("/opt/homebrew/lib"),
        PathBuf::from("/opt/homebrew/opt/mpv/lib"),
        PathBuf::from("/usr/local/lib"),
        PathBuf::from("/usr/local/opt/mpv/lib"),
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
    }
    out.extend([
        PathBuf::from("/home/linuxbrew/.linuxbrew/include"),
        PathBuf::from("/home/linuxbrew/.linuxbrew/opt/mpv/include"),
        PathBuf::from("/opt/homebrew/include"),
        PathBuf::from("/opt/homebrew/opt/mpv/include"),
        PathBuf::from("/usr/local/include"),
        PathBuf::from("/usr/include"),
    ]);
    out
}
