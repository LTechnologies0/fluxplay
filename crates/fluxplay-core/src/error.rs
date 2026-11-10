use thiserror::Error;

use crate::codes::{classify_message, FpCode, HasFpCode};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{}", display_invalid_playlist(.0))]
    InvalidPlaylist(String),

    #[error("{}", display_unsupported_scheme(.0))]
    UnsupportedScheme(String),

    #[error("{}", display_parse(.0))]
    Parse(String),

    #[error("{}", display_validation(.0))]
    Validation(String),

    #[error("{}", display_url(.0))]
    Url(#[from] url::ParseError),

    #[error("{}", display_other(.0))]
    Other(#[from] anyhow::Error),
}

fn playlist_code(msg: &str) -> FpCode {
    let s = msg.to_ascii_lowercase();
    if s.contains("oversize") || s.contains("too large") || s.contains("cap") {
        FpCode::CatM3uOversize
    } else if s.contains("empty") {
        FpCode::CatM3uEmpty
    } else if s.contains("line") {
        FpCode::CatM3uLine
    } else {
        FpCode::CatM3uParse
    }
}

fn parse_code(msg: &str) -> FpCode {
    let s = msg.to_ascii_lowercase();
    let xmltv = s.contains("xmltv") || s.contains("epg");
    let oversize = s.contains("oversize")
        || s.contains("too large")
        || s.contains("volumineu")
        || s.contains("cap");
    if xmltv && oversize {
        FpCode::CatXmltvOversize
    } else if xmltv {
        FpCode::CatXmltvParse
    } else if s.contains("m3u") {
        FpCode::CatM3uParse
    } else {
        FpCode::SysParse
    }
}

fn display_invalid_playlist(msg: &str) -> String {
    playlist_code(msg).ensure_coded(format!("invalid playlist: {msg}"))
}

fn display_unsupported_scheme(msg: &str) -> String {
    FpCode::AvUnsupportedScheme.ensure_coded(format!("unsupported scheme: {msg}"))
}

fn display_parse(msg: &str) -> String {
    parse_code(msg).ensure_coded(format!("parse error: {msg}"))
}

fn display_validation(msg: &str) -> String {
    FpCode::SysValidation.ensure_coded(format!("validation: {msg}"))
}

fn display_url(e: &url::ParseError) -> String {
    FpCode::AvInvalidUrl.ensure_coded(format!("url: {e}"))
}

fn display_other(e: &anyhow::Error) -> String {
    classify_message(&e.to_string()).ensure_coded(e)
}

impl HasFpCode for Error {
    fn fp_code(&self) -> FpCode {
        match self {
            Self::InvalidPlaylist(m) => playlist_code(m),
            Self::UnsupportedScheme(_) => FpCode::AvUnsupportedScheme,
            Self::Parse(m) => parse_code(m),
            Self::Validation(_) => FpCode::SysValidation,
            Self::Url(_) => FpCode::AvInvalidUrl,
            Self::Other(e) => classify_message(&e.to_string()),
        }
    }
}

impl Error {
    /// Display already includes `[FP-…]`.
    pub fn coded(&self) -> String {
        self.to_string()
    }

    pub fn with_code(code: FpCode, msg: impl Into<String>) -> Self {
        let m = code.ensure_coded(msg.into());
        match code.domain() {
            crate::codes::FpDomain::Cat => Self::InvalidPlaylist(m),
            crate::codes::FpDomain::Av if matches!(code, FpCode::AvUnsupportedScheme) => {
                Self::UnsupportedScheme(m)
            }
            crate::codes::FpDomain::Av if matches!(code, FpCode::AvInvalidUrl) => {
                Self::Validation(m)
            }
            _ => Self::Parse(m),
        }
    }
}
