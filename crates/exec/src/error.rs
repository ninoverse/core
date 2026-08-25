use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecError {
    #[error("EXEC: docker api error: {0}")]
    Bollard(#[from] bollard::errors::Error),

    #[error("EXEC: io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("EXEC: command '{0}' finished without reporting an exit code")]
    NoExitCode(String),

    #[error("EXEC: docker started the exec detached, so no output is available")]
    Detached,

    #[error("EXEC: refusing to run an empty command")]
    EmptyCommand,

    #[error("EXEC: could not open stdin on the child process")]
    NoStdin,
}

pub type ExecResult<T> = std::result::Result<T, ExecError>;
