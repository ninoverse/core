use thiserror::Error;

use apisix::error::ApisixError;
use exec::ExecError;
use http_client::HttpError;
use kanidm::error::KanidmError;

#[derive(Debug, Error)]
pub enum KanidmApisixError {
    #[error(transparent)]
    Kanidm(#[from] KanidmError),

    #[error(transparent)]
    Apisix(#[from] ApisixError),

    #[error(transparent)]
    Exec(#[from] ExecError),

    #[error(transparent)]
    Http(#[from] HttpError),

    #[error("INIT: io: {0}")]
    Io(#[from] std::io::Error),

    #[error(
        "INIT: no APISIX admin key at {path}\n\
         copy the `admin_key` value out of crates/core/config/apisix.yaml:\n    \
         printf '%s' '<key>' > {path} && chmod 600 {path}"
    )]
    MissingAdminKey { path: String },

    #[error("INIT: {0}")]
    Other(String),
}

impl KanidmApisixError {
    pub fn is_recoverable_by_operator(&self) -> bool {
        matches!(
            self,
            Self::Kanidm(KanidmError::NoKanidmSession { .. }) | Self::MissingAdminKey { .. }
        )
    }
}

pub type Result<T> = std::result::Result<T, KanidmApisixError>;
