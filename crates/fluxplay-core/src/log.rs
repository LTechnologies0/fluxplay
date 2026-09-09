//! Shared logging helpers for all FluxPlay crates.
//!
//! Levels (via `tracing`):
//! - **TRACE** — fine-grained flow (`RUST_LOG=…=trace`)
//! - **PROFILER** — timing / CPU / RSS / FPS (`target: "profiler"`, or `FLUXPLAY_PROFILE=1`)
//! - **DEBUG** — operational detail
//! - **INFO** — milestones
//! - **WARN** — recoverable failures
//! - **ERROR** — hard failures

/// EnvFilter default when `RUST_LOG` is unset.
/// Keep hot paths quiet — enable `FLUXPLAY_PROFILE=1` / `profiler=info` when measuring.
///
/// Verbose native backends (libmpv / libav*):
/// ```text
/// FLUXPLAY_VERBOSE=1 cargo run -p fluxplay
/// # or fine-grained:
/// FLUXPLAY_MPV_LOG=debug FLUXPLAY_FFMPEG_LOG=debug \
///   RUST_LOG=fluxplay=debug,fluxplay_player=debug,iced=info,iced_winit=info \
///   cargo run -p fluxplay
/// ```
/// Logs: stderr + `/tmp/fluxplay-mpv-verbose.log` (libmpv/CLI when verbose).
///
/// Quiet production-ish default (`FLUXPLAY_QUIET=1` forces this even if FULL would apply).
pub const DEFAULT_ENV_FILTER: &str = concat!(
    "fluxplay=info,",
    "fluxplay_core=info,",
    "fluxplay_providers=info,",
    "fluxplay_player=info,",
    "fluxplay_ffi=info,",
    "fluxplay::net=info"
);

/// Full diagnostic filter: every FluxPlay crate + major deps (deps capped to avoid naga flood).
/// Used when `RUST_LOG` is unset and `FLUXPLAY_QUIET` is not set.
pub const FULL_ENV_FILTER: &str = concat!(
    "fluxplay=debug,",
    "fluxplay_core=debug,",
    "fluxplay_providers=debug,",
    "fluxplay_player=debug,",
    "fluxplay_ffi=debug,",
    "fluxplay::ui=debug,",
    "fluxplay::net=debug,",
    "fluxplay::icons=debug,",
    "fluxplay::images=debug,",
    "fluxplay::android=debug,",
    "fluxplay::boot=debug,",
    "iced=info,",
    "iced_winit=info,",
    "iced_wgpu=info,",
    "iced_runtime=info,",
    "iced_graphics=info,",
    "wgpu=info,",
    "wgpu_core=warn,",
    "wgpu_hal=warn,",
    "naga=warn,",
    "reqwest=info,",
    "hyper=info,",
    "h2=warn,",
    "tower=info,",
    "tokio=info,",
    "mio=warn,",
    "rustls=info,",
    "calloop=warn,",
    "sctk=warn,",
    "profiler=trace"
);

/// True unless `FLUXPLAY_QUIET=1` — enables FULL_ENV_FILTER + native verbose seeding.
pub fn full_logs_enabled() -> bool {
    !matches!(
        std::env::var("FLUXPLAY_QUIET").ok().as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

/// Emit a **profiler** line (TRACE on target `profiler`).
#[macro_export]
macro_rules! profiler {
    ($($tt:tt)*) => {
        ::tracing::trace!(target: "profiler", $($tt)*)
    };
}

/// Enter a named profiler span for the rest of the scope.
#[macro_export]
macro_rules! profile_scope {
    ($name:expr) => {
        let _fluxplay_profile_guard = ::tracing::span!(
            target: "profiler",
            ::tracing::Level::TRACE,
            "profile",
            name = $name
        )
        .entered();
    };
}

/// Instant elapsed helper for ad-hoc profiler logs (wall + CPU + RSS when enabled).
pub struct Stopwatch {
    inner: crate::profiler::ResourceStopwatch,
}

impl Stopwatch {
    pub fn start(name: &'static str) -> Self {
        Self {
            inner: crate::profiler::ResourceStopwatch::start(name),
        }
    }

    pub fn lap(&self, step: &str) {
        self.inner.lap(step);
    }
}
