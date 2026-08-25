use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::error::{ExecError, ExecResult};
use crate::{CommandRunner, Output};

pub struct LocalCommand;

impl LocalCommand {
    async fn exec(argv: &[String], stdin: Option<&str>) -> ExecResult<Output> {
        let (program, args) = argv.split_first().ok_or(ExecError::EmptyCommand)?;

        let mut child = Command::new(program)
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        if let Some(body) = stdin {
            let mut handle = child.stdin.take().ok_or(ExecError::NoStdin)?;
            handle.write_all(body.as_bytes()).await?;
            drop(handle);
        }

        let finished = child.wait_with_output().await?;
        let code = finished
            .status
            .code()
            .map(i64::from)
            .ok_or_else(|| ExecError::NoExitCode(argv.join(" ")))?;

        Ok(Output {
            code,
            stdout: String::from_utf8_lossy(&finished.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&finished.stderr).into_owned(),
        })
    }
}

impl CommandRunner for LocalCommand {
    async fn run(&self, argv: &[String]) -> ExecResult<Output> {
        Self::exec(argv, None).await
    }

    async fn run_with_stdin(&self, argv: &[String], stdin: &str) -> ExecResult<Output> {
        Self::exec(argv, Some(stdin)).await
    }
}
