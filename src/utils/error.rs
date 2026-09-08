use std::path::PathBuf;

use thiserror::Error;
use tonic::transport;
use http::uri::InvalidUri;

#[derive(Error, Debug)]
pub enum HarmonicError {
    #[error("Path related error: {path:?}")]
    PathError { path: PathBuf },

    #[error("Path error: {path:?} is not in sync dir: {sync_path:?}")]
    PathIntegrityError { path: PathBuf, sync_path: PathBuf },

    #[error("UUID parsing error: {0}")]
    Uuid(#[from] uuid::Error),

    #[error("JSON de/serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("TOML deserialization error: {0}")]
    TomlDeserError(#[from] toml::de::Error),

    #[error("TOML serialization error: {0}")]
    TomlSerError(#[from] toml::ser::Error),

    #[error("Config parsing error")]
    ConfigError,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("System Time Error: {0}")]
    SystemTime(#[from] std::time::SystemTimeError),

    #[error("Invalid input parameters")]
    InvalidInputError,

    #[error("Input error: {0}")]
    Input(String),

    #[error("gRPC send error: {0}")]
    SendError(String),

    #[error("Crypto error: {0}")]
    CryptoError(String),

    #[error("Invalid Uri Error: {0}")]
    UriError(#[from] InvalidUri),

    #[error("Transport Error: {0}")]
    TranksportError(#[from] transport::Error),

    #[error("gRPC Status error: {0}")]
    GrpcStatus(#[from] tonic::Status),

    #[error("Not found: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, HarmonicError>;