use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("invalid playlist: {0}")]
    InvalidPlaylist(String),

    #[error("unsupported scheme: {0}")]
    UnsupportedScheme(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("validation: {0}")]
    Validation(String),

    #[error(transparent)]
    Url(#[from] url::ParseError),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
