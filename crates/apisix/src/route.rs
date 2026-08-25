use serde_json::{Map, Value, json};

use configuration::{Service, ServiceProtectionMode, ServicesConfiguration};

use crate::error::{ApisixError, ApisixResult};

/// Paths the `openid-connect` plugin serves itself on a gated route.
const OIDC_URIS: &str = "/oidc/*";

/// A gated route has to match the callback and logout paths too: the plugin
/// only runs on requests *this* route matches, so a callback landing on a
/// sibling route would be forwarded upstream and the login would never finish.
fn route_uris(service: &Service) -> Vec<String> {
    let mut uris = service.uris.clone();
    if service.mode.is_browser_flow()
        && !uris
            .iter()
            .any(|uri| uri == "/*" || uri.starts_with("/oidc"))
    {
        uris.push(OIDC_URIS.to_string());
    }
    uris
}

fn build_oidc(
    service: &Service,
    configuration: &ServicesConfiguration,
    client_secret: &str,
    session_secret: &str,
) -> ApisixResult<Value> {
    let mut oidc = Map::new();
    oidc.insert("client_id".into(), json!(service.client_id));
    oidc.insert("client_secret".into(), json!(client_secret));
    oidc.insert(
        "discovery".into(),
        json!(service.discovery_url(configuration)),
    );
    oidc.insert("scope".into(), json!("openid profile email groups"));
    oidc.insert("realm".into(), json!(configuration.domain));
    oidc.insert("ssl_verify".into(), json!(configuration.tls_verify));
    oidc.insert("token_signing_alg_values_expected".into(), json!("ES256"));
    oidc.insert(
        "set_userinfo_header".into(),
        json!(configuration.set_userinfo_header),
    );
    oidc.insert("set_id_token_header".into(), json!(false));
    oidc.insert("set_refresh_token_header".into(), json!(false));

    match service.mode {
        ServiceProtectionMode::Api => {
            oidc.insert("bearer_only".into(), json!(true));
            oidc.insert("use_jwks".into(), json!(true));
        }
        ServiceProtectionMode::Gate => {
            oidc.insert("bearer_only".into(), json!(false));
            oidc.insert("use_pkce".into(), json!(true));
            oidc.insert(
                "redirect_uri".into(),
                json!(service.redirect_url(configuration).ok_or_else(|| {
                    ApisixError::Config(format!("'{}' has no redirect url", service.client_id))
                })?),
            );
            oidc.insert("logout_path".into(), json!("/oidc/logout"));
            oidc.insert(
                "post_logout_redirect_uri".into(),
                json!(format!("{}/", service.base_url(configuration))),
            );
            oidc.insert("session".into(), json!({ "secret": session_secret }));
        }
        ServiceProtectionMode::Proxy
        | ServiceProtectionMode::Native
        | ServiceProtectionMode::Unprotected => {
            return Err(ApisixError::Config(format!(
                "'{}' is {} and must not get an openid-connect plugin",
                service.client_id, service.mode
            )));
        }
    }

    Ok(Value::Object(oidc))
}

pub fn build_route(
    service: &Service,
    configuration: &ServicesConfiguration,
    client_secret: &str,
    session_secret: &str,
) -> ApisixResult<Value> {
    let upstream = service
        .upstream
        .as_deref()
        .ok_or_else(|| ApisixError::Config(format!("'{}' has no upstream", service.client_id)))?;

    let (plugins, desc) = match service.mode {
        ServiceProtectionMode::Proxy => (
            json!({}),
            "Unauthenticated passthrough; the upstream owns its auth. \
             Managed by ninoverse-init; edits will be overwritten.",
        ),
        ServiceProtectionMode::Gate | ServiceProtectionMode::Api => (
            json!({
                "openid-connect":
                    build_oidc(service, configuration, client_secret, session_secret)?
            }),
            "OIDC-gated by kanidm. Managed by ninoverse-init; edits will be overwritten.",
        ),
        ServiceProtectionMode::Native | ServiceProtectionMode::Unprotected => {
            return Err(ApisixError::Config(format!(
                "'{}' is {} and must not get an APISIX route",
                service.client_id, service.mode
            )));
        }
    };

    Ok(json!({
        "name": service.client_id,
        "desc": desc,
        "uris": route_uris(service),
        "priority": service.priority,
        "host": service.host(configuration),
        "plugins": plugins,
        "upstream": {
            "type": "roundrobin",
            "scheme": "http",
            "nodes": { upstream: 1 },
            "checks": {
                "active": {
                    "type": "http",
                    "http_path": "/",
                    "healthy":   { "interval": 5, "successes": 1 },
                    "unhealthy": { "interval": 5, "http_failures": 3 }
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configuration() -> ServicesConfiguration {
        ServicesConfiguration {
            domain: "example.test".into(),
            kanidm_origin: "https://auth.example.test".into(),
            kanidm_cli_container: "kanidm-cli".into(),
            kanidm_admin: "idm_admin".into(),
            curl_container: "apisix-curl".into(),
            apisix_admin_url: "http://apisix:9180".into(),
            secrets_dir: "./secrets".into(),
            set_userinfo_header: true,
            tls_verify: true,
            services: Vec::new(),
            persons: Vec::new(),
        }
    }

    fn service(mode: ServiceProtectionMode, uris: &[&str]) -> Service {
        Service {
            client_id: "thing".into(),
            display_name: "Thing".into(),
            subdomain: "thing".into(),
            uris: uris.iter().map(|uri| (*uri).to_string()).collect(),
            priority: 0,
            upstream: Some("thing:8080".into()),
            group: "thing_users".into(),
            mode,
            native_redirect_url: None,
        }
    }

    #[test]
    fn gate_route_matches_the_oidc_callback() {
        let service = service(ServiceProtectionMode::Gate, &["/ui/*"]);
        let route = build_route(&service, &configuration(), "secret", "session").unwrap();

        let uris: Vec<&str> = route["uris"]
            .as_array()
            .unwrap()
            .iter()
            .map(|uri| uri.as_str().unwrap())
            .collect();

        // Without this the callback falls through to a sibling route and the
        // login never completes.
        assert!(uris.contains(&OIDC_URIS), "gate route missing {OIDC_URIS}");
    }

    #[test]
    fn gate_route_keeps_a_catch_all_as_is() {
        let service = service(ServiceProtectionMode::Gate, &["/*"]);
        let route = build_route(&service, &configuration(), "secret", "session").unwrap();

        assert_eq!(route["uris"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn proxy_route_carries_no_auth_plugin() {
        let service = service(ServiceProtectionMode::Proxy, &["/cargo/*"]);
        let route = build_route(&service, &configuration(), "", "session").unwrap();

        // A proxy route exists precisely so the upstream sees the request
        // untouched; an auth plugin here would reject the upstream's own tokens.
        assert!(
            route["plugins"].as_object().unwrap().is_empty(),
            "proxy route must not carry plugins"
        );
        assert_eq!(route["uris"], json!(["/cargo/*"]));
    }

    #[test]
    fn priority_defaults_to_zero_and_is_emitted() {
        let service = service(ServiceProtectionMode::Proxy, &["/cargo/*"]);
        let route = build_route(&service, &configuration(), "", "session").unwrap();

        assert_eq!(route["priority"], json!(0));
    }

    #[test]
    fn native_is_refused_a_route() {
        let service = service(ServiceProtectionMode::Native, &["/*"]);

        assert!(build_route(&service, &configuration(), "secret", "session").is_err());
    }
}
