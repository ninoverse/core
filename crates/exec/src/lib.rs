pub mod error;

mod docker;
mod local;

use std::future::Future;

pub use crate::docker::DockerExec;
pub use crate::error::{ExecError, ExecResult};
pub use crate::local::LocalCommand;

#[derive(Debug, Clone)]
pub struct Output {
    pub code: i64,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == 0
    }
}

pub trait CommandRunner {
    fn run(&self, argv: &[String]) -> impl Future<Output = ExecResult<Output>> + Send;

    fn run_with_stdin(
        &self,
        argv: &[String],
        stdin: &str,
    ) -> impl Future<Output = ExecResult<Output>> + Send;
}
