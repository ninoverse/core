use thiserror::Error;

use exec::ExecError;

#[derive(Debug, Error)]
pub enum KanidmError {
    #[error("KANIDM: {0}")]
    Exec(#[from] ExecError),

    #[error("KANIDM: config: {0}")]
    Config(String),

    #[error(
        "KANIDM: no valid session for '{account}'\n\
         run once, interactively:\n    docker exec -it {container} kanidm login --name {account}"
    )]
    NoKanidmSession { container: String, account: String },

    #[error("KANIDM: cli `{command}` exited {code}: {stderr}")]
    KanidmCli {
        command: String,
        code: i64,
        stderr: String,
    },

    #[error("KANIDM: {0}")]
    Other(String),
}

pub type KanidmResult<T> = std::result::Result<T, KanidmError>;
