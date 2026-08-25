use bollard::Docker;
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecResults};
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

use crate::error::{ExecError, ExecResult};
use crate::{CommandRunner, Output};

pub struct DockerExec {
    docker: Docker,
    container: String,
}

impl DockerExec {
    pub fn connect(container: impl Into<String>) -> ExecResult<Self> {
        Ok(Self {
            docker: Docker::connect_with_defaults()?,
            container: container.into(),
        })
    }

    pub fn new(docker: Docker, container: impl Into<String>) -> Self {
        Self {
            docker,
            container: container.into(),
        }
    }

    pub fn container(&self) -> &str {
        &self.container
    }

    async fn exec(&self, argv: &[String], stdin: Option<&str>) -> ExecResult<Output> {
        if argv.is_empty() {
            return Err(ExecError::EmptyCommand);
        }

        let options = CreateExecOptions {
            cmd: Some(argv.to_vec()),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            attach_stdin: Some(stdin.is_some()),
            tty: Some(false),
            ..Default::default()
        };

        let created = self.docker.create_exec(&self.container, options).await?;

        let StartExecResults::Attached {
            mut output,
            mut input,
        } = self.docker.start_exec(&created.id, None).await?
        else {
            return Err(ExecError::Detached);
        };

        if let Some(body) = stdin {
            input.write_all(body.as_bytes()).await?;
            input.shutdown().await?;
        }

        drop(input);

        let mut stdout = String::new();
        let mut stderr = String::new();
        while let Some(chunk) = output.next().await {
            match chunk? {
                LogOutput::StdOut { message } => {
                    stdout.push_str(&String::from_utf8_lossy(&message))
                }
                LogOutput::StdErr { message } => {
                    stderr.push_str(&String::from_utf8_lossy(&message))
                }
                _ => {}
            }
        }

        let code = self
            .docker
            .inspect_exec(&created.id)
            .await?
            .exit_code
            .ok_or_else(|| ExecError::NoExitCode(argv.join(" ")))?;

        Ok(Output {
            code,
            stdout,
            stderr,
        })
    }
}

impl CommandRunner for DockerExec {
    async fn run(&self, argv: &[String]) -> ExecResult<Output> {
        self.exec(argv, None).await
    }

    async fn run_with_stdin(&self, argv: &[String], stdin: &str) -> ExecResult<Output> {
        self.exec(argv, Some(stdin)).await
    }
}
