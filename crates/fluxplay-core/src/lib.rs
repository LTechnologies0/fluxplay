//! FluxPlay core: models, playlist formats, stream URL schemes, and EPG types.

pub mod error;
pub mod log;
pub mod m3u;
pub mod models;
pub mod profiler;
pub mod protocol;
pub mod xmltv;

pub use error::{Error, Result};
pub use log::{Stopwatch, DEFAULT_ENV_FILTER};
pub use models::*;
pub use profiler::{
    format_bytes, format_bytes_signed, overlay_enabled, overlay_status_line, profiling_enabled,
    FpsCounter, GuiProfiler, InteractionGuard, InteractionSample, ProcSnapshot, ResourceStopwatch,
};
pub use protocol::{DeliveryKind, IngestKind, StreamScheme, StreamUrl};
