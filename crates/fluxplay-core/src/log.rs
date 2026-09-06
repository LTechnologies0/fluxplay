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
pub const DEFAULT_ENV_FILTER: &str = concat!(
    "fluxplay=info,",
    "fluxplay_core=info,",
    "fluxplay_providers=info,",
    "fluxplay_player=info,",
    "fluxplay_ffi=info,",
    "fluxplay::net=info"
);

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
