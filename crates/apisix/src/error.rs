use thiserror::Error;

use http_client::HttpError;

#[derive(Debug, Error)]
pub enum ApisixError {
    #[error("APISIX: json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("APISIX: config: {0}")]
    Config(String),

    #[error("APISIX: {0}")]
    Http(#[from] HttpError),

    #[error("APISIX: admin api {method} {path} -> {status}: {body}")]
    AdminApi {
        method: &'static str,
        path: String,
        status: u16,
        body: String,
    },

    #[error("APISIX: no client secret recorded for '{0}' — run the kanidm stage first")]
    MissingSecret(String),
}

pub type ApisixResult<T> = std::result::Result<T, ApisixError>;
