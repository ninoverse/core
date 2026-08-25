use thiserror::Error;

use exec::ExecError;

#[derive(Debug, Error)]
pub enum HttpError {
    #[error("HTTP: transport failure: {0}")]
    Transport(String),

    #[error("HTTP: {0}")]
    Exec(#[from] ExecError),

    #[error("HTTP: reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error("HTTP: could not parse a status code out of curl's output: {0:?}")]
    UnparseableStatus(String),
}

pub type HttpResult<T> = std::result::Result<T, HttpError>;
