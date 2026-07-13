use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("invalid project config at {path}: {reason}")]
    InvalidProject { path: PathBuf, reason: String },

    #[error("project folder not found: {0}")]
    ProjectMissing(PathBuf),

    #[error("path escapes project root: {0}")]
    PathOutsideProject(PathBuf),

    #[error("extraction failed: {0}")]
    Extraction(String),

    #[error("watch error: {0}")]
    Watch(String),

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}
