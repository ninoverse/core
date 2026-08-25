use std::collections::BTreeMap;

use serde_json::Value;

use configuration::{Service, ServiceProtectionMode, ServicesConfiguration};
use http_client::{HttpClient, Request};
use logger::info;

use crate::error::{ApisixError, ApisixResult};
use crate::route::build_route;

pub struct AdminClient<Client> {
    http: Client,
    base_url: String,
    api_key: String,
}

impl<Client: HttpClient> AdminClient<Client> {
    pub fn new(http: Client, configuration: &ServicesConfiguration, api_key: String) -> Self {
        Self {
            http,
            base_url: configuration
                .apisix_admin_url
                .trim_end_matches('/')
                .to_string(),
            api_key,
        }
    }

    async fn call(
        &self,
        method: &'static str,
        path: &str,
        body: Option<&str>,
    ) -> ApisixResult<String> {
        let url = format!("{}{}", self.base_url, path);
        let headers = [
            ("X-API-KEY".to_string(), self.api_key.clone()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];

        let response = self
            .http
            .send(Request {
                method,
                url: &url,
                headers: &headers,
                body,
            })
            .await?;

        if !response.is_success() {
            return Err(ApisixError::AdminApi {
                method,
                path: path.to_string(),
                status: response.status,
                body: response.body.chars().take(400).collect(),
            });
        }

        Ok(response.body)
    }

    pub async fn preflight(&self) -> ApisixResult<()> {
        self.call("GET", "/apisix/admin/routes", None).await?;
        Ok(())
    }

    pub async fn put_route(&self, service: &Service, payload: &Value) -> ApisixResult<()> {
        let path = format!("/apisix/admin/routes/{}", service.client_id);
        self.call("PUT", &path, Some(&serde_json::to_string(payload)?))
            .await?;
        Ok(())
    }

    pub async fn provision_all(
        &self,
        configuration: &ServicesConfiguration,
        secrets: &BTreeMap<String, String>,
        session_secret: &str,
    ) -> ApisixResult<usize> {
        let mut count = 0;

        for service in configuration
            .services
            .iter()
            .filter(|service| service.mode.needs_apisix_route())
        {
            // A proxy route carries no openid-connect plugin, so no kanidm
            // client — and therefore no secret — is ever minted for it.
            let secret: &str = match service.mode {
                ServiceProtectionMode::Proxy => "",
                _ => secrets
                    .get(&service.secret_key())
                    .map(String::as_str)
                    .ok_or_else(|| ApisixError::MissingSecret(service.client_id.clone()))?,
            };

            info!(
                ["APISIX"],
                "configuring route {} -> {} ({})",
                service.host(configuration),
                service.upstream.as_deref().unwrap_or("-"),
                service.mode
            );

            let payload = build_route(service, configuration, secret, session_secret)?;
            self.put_route(service, &payload).await?;

            info!(["APISIX"], "applied route {}", service.client_id);
            count += 1;
        }

        Ok(count)
    }
}
