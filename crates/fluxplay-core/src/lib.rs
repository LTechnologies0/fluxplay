//! FluxPlay core: models, playlist formats, stream URL schemes, and EPG types.

pub mod error;
pub mod m3u;
pub mod models;
pub mod protocol;
pub mod xmltv;

pub use error::{Error, Result};
pub use models::*;
pub use protocol::{DeliveryKind, IngestKind, StreamScheme, StreamUrl};
