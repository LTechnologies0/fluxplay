//! Propagate rpath onto the final `fluxplay` binary (lib crate build.rs link-args
//! are ignored when the dependency is compiled as an rlib).

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=MPV_LIB_DIR");
    println!("cargo:rerun-if-env-changed=MPV_PREFIX");
    println!("cargo:rerun-if-env-changed=FLUXPLAY_BUNDLE_RPATH");
    println!("cargo:rerun-if-env-changed=HOMEBREW_PREFIX");

    if cfg!(target_os = "linux") {
        // Pass $ORIGIN literally to the linker (Cargo/build script must not expand it).
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN/lib");
    } else if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/lib");
    }

    // Windows: delay-load libmpv so the exe starts (and falls back) without the DLL —
    // link args from fluxplay-player's build.rs never reach this binary.
    let target = env::var("TARGET").unwrap_or_default();
    if target.contains("windows-msvc") {
        // All targets, not just bins: test executables import libmpv too and would
        // fail to start (STATUS_DLL_NOT_FOUND) on a machine without the DLL.
        println!("cargo:rustc-link-arg=/DELAYLOAD:libmpv-2.dll");
        println!("cargo:rustc-link-arg=delayimp.lib");
    }
    // `winresource` is a Windows-host build-dependency (needs rc.exe): only a
    // Windows host embeds the icon; Linux/macOS hosts must not reference it.
    #[cfg(windows)]
    if target.contains("windows") {
        embed_windows_icon();
    }

    if let Some(lib) = discover_lib_dir() {
        if cfg!(target_os = "linux") || cfg!(target_os = "macos") {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
        }
    }
}

/// Embed the app icon + version info in `fluxplay.exe` (Explorer, taskbar, Alt-Tab).
#[cfg(windows)]
fn embed_windows_icon() {
    println!("cargo:rerun-if-changed=assets/fluxplay.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/fluxplay.ico")
        .set("ProductName", "FluxPlay")
        .set("FileDescription", "FluxPlay — lecteur IPTV");
    if let Err(e) = res.compile() {
        println!("cargo:warning=icon resource not embedded: {e}");
    }
}

fn discover_lib_dir() -> Option<PathBuf> {
    if let Ok(d) = env::var("MPV_LIB_DIR") {
        return Some(PathBuf::from(d));
    }
    if let Ok(prefix) = env::var("MPV_PREFIX") {
        return Some(PathBuf::from(prefix).join("lib"));
    }
    if let Ok(p) = env::var("HOMEBREW_PREFIX") {
        let lib = PathBuf::from(p).join("lib");
        if lib.is_dir() {
            return Some(lib);
        }
    }
    for c in [
        "/home/linuxbrew/.linuxbrew/lib",
        "/opt/homebrew/lib",
        "/usr/local/lib",
        "/usr/lib64",
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib",
    ] {
        let p = PathBuf::from(c);
        if p.join("libmpv.so").exists()
            || std::fs::read_dir(&p).ok().is_some_and(|rd| {
                rd.flatten().any(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with("libmpv.so")
                })
            })
        {
            return Some(p);
        }
    }
    None
}
