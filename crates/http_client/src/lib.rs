pub mod error;

mod curl;
mod reqwest_client;

use std::future::Future;

pub use crate::curl::CurlExecClient;
pub use crate::error::{HttpError, HttpResult};
pub use crate::reqwest_client::ReqwestClient;

pub struct Request<'a> {
    // TODO: use the reqwest::Method struct
    pub method: &'a str,
    pub url: &'a str,
    pub headers: &'a [(String, String)],
    pub body: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

impl Response {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

pub trait HttpClient {
    fn send(&self, request: Request<'_>) -> impl Future<Output = HttpResult<Response>> + Send;
}
