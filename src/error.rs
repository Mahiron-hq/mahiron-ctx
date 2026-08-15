use std::io;
use std::path::PathBuf;

/// Every failure that can abort a run.
///
/// Per-file problems are not represented here: they are recorded as warnings and the
/// run continues, unless strict mode is active, in which case they surface as
/// [`Error::Strict`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error(transparent)]
    PlainIo(#[from] io::Error),

    #[error("invalid pattern `{pattern}`: {message}")]
    Pattern { pattern: String, message: String },

    #[error("source not found: {0}")]
    SourceNotFound(PathBuf),

    #[error("{0}")]
    Config(String),

    #[error("remote retrieval failed: {0}")]
    Remote(String),

    #[error("`{0}` already exists; pass --force to overwrite it, or choose another --output path")]
    RefuseOverwrite(PathBuf),

    #[error("strict mode aborted the run: {0}")]
    Strict(String),

    #[error("token estimation unavailable: {0}")]
    Tokenizer(String),

    #[error("clipboard delivery unavailable: {0}")]
    Clipboard(String),

    #[error("{0}")]
    Mcp(String),
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::Config(message.into())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
