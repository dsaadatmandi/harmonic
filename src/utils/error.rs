use std::path::PathBuf;

use thiserror::Error;
use walkdir::DirEntry;

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

    #[error("String conversion error: path contains invalid UTF-8 characters")]
    StringInvalid,

    #[error("System Time Error: {0}")]
    SystemTime(#[from] std::time::SystemTimeError),

    #[error("File metadata error: Unable to get metadata for {dir:?}")]
    FileMetadataError { dir: DirEntry },

    #[error("Invalid input parameters")]
    InvalidInputError,

    #[error("gRPC send error")]
    SendError(String),
}

pub type Result<T> = std::result::Result<T, HarmonicError>;