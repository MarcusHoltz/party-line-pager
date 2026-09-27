//! Error type shared by the core crate.

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("could not parse {path}: {source}")]
    Toml {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("could not parse {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("invalid policy: {0}")]
    Policy(String),

    #[error("invalid room address: {0}")]
    Address(String),

    #[error("invalid endpoint id: {0}")]
    Endpoint(String),

    #[error("invalid quiet window: {0}")]
    QuietWindow(String),

    #[error("unknown timezone: {0}")]
    Timezone(String),

    #[error("could not gather randomness: {0}")]
    Random(#[from] getrandom::Error),
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}
