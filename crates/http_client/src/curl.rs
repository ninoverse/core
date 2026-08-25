use exec::CommandRunner;

use crate::error::{HttpError, HttpResult};
use crate::{HttpClient, Request, Response};

const WRITE_OUT: &str = "\\n%{http_code}";

pub struct CurlExecClient<R> {
    runner: R,
}

impl<R: CommandRunner> CurlExecClient<R> {
    pub fn new(runner: R) -> Self {
        Self { runner }
    }

    pub fn runner(&self) -> &R {
        &self.runner
    }

    fn argv(request: &Request<'_>) -> Vec<String> {
        let mut argv: Vec<String> = vec![
            "curl".into(),
            "-s".into(),
            "-w".into(),
            WRITE_OUT.into(),
            "-X".into(),
            request.method.into(),
            request.url.into(),
        ];

        for (name, value) in request.headers {
            argv.push("-H".into());
            argv.push(format!("{name}: {value}"));
        }

        if request.body.is_some() {
            argv.push("--data-binary".into());
            argv.push("@-".into());
        }

        argv
    }
}

fn split_curl_output(code: i64, stdout: &str, stderr: &str) -> HttpResult<Response> {
    if code != 0 {
        return Err(HttpError::Transport(format!(
            "curl exited {code}: {}",
            stderr.trim()
        )));
    }

    let (body, status) = match stdout.rfind('\n') {
        Some(index) => (&stdout[..index], stdout[index + 1..].trim()),
        None => ("", stdout.trim()),
    };

    let status: u16 = status
        .parse()
        .map_err(|_| HttpError::UnparseableStatus(status.to_string()))?;

    Ok(Response {
        status,
        body: body.to_string(),
    })
}

impl<R: CommandRunner + Sync> HttpClient for CurlExecClient<R> {
    async fn send(&self, request: Request<'_>) -> HttpResult<Response> {
        let argv = Self::argv(&request);

        let output = match request.body {
            Some(body) => self.runner.run_with_stdin(&argv, body).await?,
            None => self.runner.run(&argv).await?,
        };

        split_curl_output(output.code, &output.stdout, &output.stderr)
    }
}
