//! FluxPlay core: models, playlist formats, stream URL schemes, and EPG types.

pub mod codes;
pub mod error;
pub mod log;
pub mod m3u;
pub mod models;
pub mod profiler;
pub mod protocol;
pub mod redact;
pub mod xmltv;
#[cfg(test)]
mod prop_xmltv;
#[cfg(test)]
mod prop_models;

pub use codes::{
    classify_io, classify_message, ensure_status, FpCode, FpDomain, HasFpCode,
};
pub use error::{Error, Result};
pub use redact::redact_urls;
pub use log::{Stopwatch, DEFAULT_ENV_FILTER, FULL_ENV_FILTER, full_logs_enabled};
pub use models::*;
pub use profiler::{
    format_bytes, format_bytes_signed, overlay_enabled, overlay_status_line, profiling_enabled,
    FpsCounter, GuiProfiler, InteractionGuard, InteractionSample, ProcSnapshot, ResourceStopwatch,
};
pub use protocol::{DeliveryKind, IngestKind, StreamScheme, StreamUrl};
