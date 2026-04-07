use thiserror::Error;

#[derive(Error, Debug)]
pub enum CdnError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("upstream error: {0}")]
    Upstream(String),

    #[error("cache error: {0}")]
    Cache(String),

    #[error("middleware error: {0}")]
    Middleware(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type CdnResult<T> = Result<T, CdnError>;
