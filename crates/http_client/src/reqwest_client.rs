use reqwest::{Client, Method};

use crate::error::{HttpError, HttpResult};
use crate::{HttpClient, Request, Response};

pub struct ReqwestClient {
    client: Client,
}

impl ReqwestClient {
    pub fn new() -> HttpResult<Self> {
        Ok(Self {
            client: Client::builder().build()?,
        })
    }

    pub fn with_client(client: Client) -> Self {
        Self { client }
    }
}

impl HttpClient for ReqwestClient {
    async fn send(&self, request: Request<'_>) -> HttpResult<Response> {
        let method = Method::from_bytes(request.method.as_bytes())
            .map_err(|e| HttpError::Transport(format!("invalid method: {e}")))?;

        let mut builder = self.client.request(method, request.url);
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = request.body {
            builder = builder.body(body.to_string());
        }

        let response = builder.send().await?;
        let status = response.status().as_u16();

        Ok(Response {
            status,
            body: response.text().await?,
        })
    }
}
