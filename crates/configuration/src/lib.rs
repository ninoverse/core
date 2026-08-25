use std::{collections::HashSet, fmt, path::Path};

use config::{Config, Environment, File};
use garde::Validate;
use serde::{Deserialize, Deserializer};

fn parse_topic<E>(topic_string: &str) -> Result<KafkaTopic, E>
where
    E: serde::de::Error,
{
    let mut parts = topic_string.splitn(3, ':');
    let (name, num_partition, offset) = match (parts.next(), parts.next(), parts.next()) {
        (Some(name), Some(num_partition), Some(offset)) => {
            (name.trim(), num_partition.trim(), offset.trim())
        }
        _ => {
            return Err(E::custom(format!(
                "invalid topic '{topic_string}': expected format 'name:num_partition:offset'"
            )));
        }
    };

    if name.is_empty() {
        return Err(E::custom(format!(
            "invalid topic '{topic_string}': empty topic name"
        )));
    }

    let num_partition = num_partition.parse().map_err(|e| {
        E::custom(format!(
            "invalid num_partition '{num_partition}' in topic '{topic_string}': {e}"
        ))
    })?;

    let offset = offset.parse().map_err(|e| {
        E::custom(format!(
            "invalid offset '{offset}' in topic '{topic_string}': {e}"
        ))
    })?;

    Ok(KafkaTopic {
        name: name.to_string(),
        num_partition,
        offset,
    })
}

fn deserialize_kafka_topics<'de, D>(deserializer: D) -> Result<Vec<KafkaTopic>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw_list: Vec<String> = Vec::deserialize(deserializer)?;
    raw_list
        .iter()
        .map(|topic_string| parse_topic::<D::Error>(topic_string))
        .collect()
}

#[derive(Deserialize)]
pub struct PostgresDatabaseConfiguration {
    pub host: String,
    pub port: u16,
    pub name: String,
    pub user: String,
    pub password: String,
    pub pool_size: u32,
    pub connection_attempt: u32,
}

impl fmt::Debug for PostgresDatabaseConfiguration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PostgresDatabaseConfiguration")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("name", &self.name)
            .field("user", &self.user)
            .field("password", &"***")
            .field("pool_size", &self.pool_size)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
pub struct KafkaBrokerConfiguration {
    pub broker: String,
    #[serde(deserialize_with = "deserialize_kafka_topics")]
    pub topics: Vec<KafkaTopic>,
    pub group_id: String,
}

#[derive(Debug, Deserialize)]
pub struct KafkaTopic {
    pub name: String,
    pub num_partition: i32,
    #[allow(dead_code)]
    pub offset: u64,
}

#[derive(Debug, Deserialize)]
pub struct DockerConfiguration {
    pub remove_containers_on_shutdown: bool,
    pub remove_volumes_on_shutdown: bool,
    pub remove_networks_on_shutdown: bool,
}

#[derive(Debug, Deserialize)]
pub struct NinoverseCoreConfiguration {
    #[allow(dead_code)]
    pub host: String,
    pub port: u16,
    pub database: PostgresDatabaseConfiguration,
    pub kafka: KafkaBrokerConfiguration,
    pub docker: DockerConfiguration,
}

impl NinoverseCoreConfiguration {
    pub fn build(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let builder = Config::builder()
            .add_source(File::from(path).required(true))
            .add_source(
                Environment::with_prefix("APP")
                    .try_parsing(true)
                    .separator("__")
                    .list_separator(",")
                    .with_list_parse_key("kafka.topics"),
            );

        let configuration = builder.build()?.try_deserialize::<Self>()?;
        // configuration.clone().validate()?;
        Ok(configuration)
    }
}

fn no_trailing_slash(value: &str, _: &()) -> garde::Result {
    if value.ends_with('/') {
        return Err(garde::Error::new("must not have a trailing slash"));
    }
    Ok(())
}

fn unique_client_ids(services: &[Service], _: &()) -> garde::Result {
    let mut seen = HashSet::new();
    for s in services {
        if !seen.insert(s.client_id.as_str()) {
            return Err(garde::Error::new(format!(
                "duplicate client_id '{}'",
                s.client_id
            )));
        }
    }
    Ok(())
}

fn unique_person_names(persons: &[Person], _: &()) -> garde::Result {
    let mut seen = HashSet::new();
    for p in persons {
        if !seen.insert(p.name.as_str()) {
            return Err(garde::Error::new(format!("duplicate name '{}'", p.name)));
        }
    }
    Ok(())
}

fn default_kanidm_container() -> String {
    "kanidm-cli".into()
}

fn default_kanidm_admin() -> String {
    "idm_admin".into()
}

fn default_curl_container() -> String {
    "apisix-curl".into()
}

fn default_admin_url() -> String {
    "http://apisix:9180".into()
}

fn default_secrets_dir() -> String {
    "./secrets".into()
}

fn default_true() -> bool {
    true
}

fn default_uris() -> Vec<String> {
    vec!["/*".into()]
}

fn paths_absolute(uris: &[String], _: &()) -> garde::Result {
    if uris.is_empty() {
        return Err(garde::Error::new("must list at least one path"));
    }
    for uri in uris {
        if !uri.starts_with('/') {
            return Err(garde::Error::new(format!(
                "path '{uri}' must start with '/'"
            )));
        }
    }
    Ok(())
}

fn upstream_for(
    service_protection_mode: &ServiceProtectionMode,
) -> impl FnOnce(&Option<String>, &()) -> garde::Result + '_ {
    move |upstream, _| match upstream {
        None if service_protection_mode.needs_apisix_route() => Err(garde::Error::new(format!(
            "mode {service_protection_mode} needs an upstream"
        ))),
        Some(up) if !up.contains(':') => Err(garde::Error::new(format!(
            "upstream '{up}' must be host:port"
        ))),
        _ => Ok(()),
    }
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceProtectionMode {
    /// APISIX runs the OIDC authorization-code flow in front of an app that
    /// has no usable auth of its own. Creates a route and an upstream.
    Gate,
    /// The app speaks OIDC itself. A kanidm client is created and the secret
    /// emitted, but APISIX is not placed in the request path.
    Native,
    /// APISIX validates a bearer token. No browser flow, no session cookie.
    Api,
    /// APISIX routes the request but adds no authentication. The upstream owns
    /// its own auth — used for CLI surfaces presenting the upstream's own
    /// tokens, which a gateway-level OIDC check would reject.
    Proxy,
    /// No validation is requested
    Unprotected,
}

impl ServiceProtectionMode {
    pub fn needs_apisix_route(self) -> bool {
        matches!(
            self,
            ServiceProtectionMode::Gate | ServiceProtectionMode::Api | ServiceProtectionMode::Proxy
        )
    }

    pub fn is_browser_flow(self) -> bool {
        matches!(self, ServiceProtectionMode::Gate)
    }
}

impl fmt::Display for ServiceProtectionMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            ServiceProtectionMode::Gate => "gate",
            ServiceProtectionMode::Native => "native",
            ServiceProtectionMode::Api => "api",
            ServiceProtectionMode::Proxy => "proxy",
            ServiceProtectionMode::Unprotected => "unprotected",
        })
    }
}

#[derive(Debug, Deserialize, Clone, Validate)]
#[allow(dead_code)]
pub struct Service {
    #[garde(length(min = 1))]
    pub client_id: String,
    #[garde(skip)]
    pub display_name: String,
    #[garde(skip)]
    pub subdomain: String,
    /// Paths this service claims on its host. Several services may share a
    /// host as long as their paths do not overlap.
    #[serde(default = "default_uris")]
    #[garde(custom(paths_absolute))]
    pub uris: Vec<String>,
    /// Tie-break when more than one route matches a request; higher wins.
    /// APISIX defaults to 0.
    #[serde(default)]
    #[garde(skip)]
    pub priority: i32,
    #[serde(default)]
    #[garde(custom(upstream_for(&self.mode)))]
    pub upstream: Option<String>,
    #[garde(skip)]
    pub group: String,
    #[garde(skip)]
    pub mode: ServiceProtectionMode,
    #[serde(default)]
    #[garde(skip)]
    pub native_redirect_url: Option<String>,
}

impl Service {
    pub fn host(&self, configuration: &ServicesConfiguration) -> String {
        format!("{}.{}", self.subdomain, configuration.domain)
    }

    pub fn base_url(&self, configuration: &ServicesConfiguration) -> String {
        format!("https://{}", self.host(configuration))
    }

    pub fn redirect_url(&self, configuration: &ServicesConfiguration) -> Option<String> {
        match self.mode {
            ServiceProtectionMode::Gate => {
                Some(format!("{}/oidc/callback", self.base_url(configuration)))
            }
            ServiceProtectionMode::Native => self.native_redirect_url.clone(),
            ServiceProtectionMode::Api => None,
            ServiceProtectionMode::Proxy => None,
            ServiceProtectionMode::Unprotected => None,
        }
    }

    pub fn discovery_url(&self, configuration: &ServicesConfiguration) -> String {
        format!(
            "{}/oauth2/openid/{}/.well-known/openid-configuration",
            configuration.kanidm_origin, self.client_id
        )
    }

    pub fn secret_key(&self) -> String {
        let uppercase_secret: String = self
            .client_id
            .chars()
            .map(|c| {
                if c == '-' {
                    '_'
                } else {
                    c.to_ascii_uppercase()
                }
            })
            .collect();
        format!("OIDC_SECRET_{uppercase_secret}")
    }
}

#[derive(Debug, Deserialize, Clone, Validate)]
#[allow(dead_code)]
pub struct Person {
    #[garde(length(min = 1))]
    pub name: String,
    #[garde(skip)]
    pub display_name: String,
    #[serde(default)]
    #[garde(skip)]
    pub mail: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    pub legal_name: Option<String>,
    #[serde(default)]
    #[garde(inner(length(min = 1)))]
    pub groups: Vec<String>,
}

#[derive(Debug, Deserialize, Validate, Clone)]
#[allow(dead_code)]
pub struct ServicesConfiguration {
    #[garde(length(min = 1))]
    pub domain: String,
    #[garde(custom(no_trailing_slash))]
    pub kanidm_origin: String,
    #[serde(default = "default_kanidm_container")]
    #[garde(skip)]
    pub kanidm_cli_container: String,
    #[serde(default = "default_kanidm_admin")]
    #[garde(skip)]
    pub kanidm_admin: String,
    #[serde(default = "default_curl_container")]
    #[garde(skip)]
    pub curl_container: String,
    #[serde(default = "default_admin_url")]
    #[garde(skip)]
    pub apisix_admin_url: String,
    #[serde(default = "default_secrets_dir")]
    #[garde(skip)]
    pub secrets_dir: String,
    #[serde(default = "default_true")]
    #[garde(skip)]
    pub set_userinfo_header: bool,
    #[serde(default = "default_true")]
    #[garde(skip)]
    pub tls_verify: bool,
    #[garde(custom(unique_client_ids), dive)]
    pub services: Vec<Service>,
    #[serde(default)]
    #[garde(custom(unique_person_names), dive)]
    pub persons: Vec<Person>,
}

impl ServicesConfiguration {
    pub fn build(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let builder = Config::builder().add_source(File::from(path).required(true));

        let configuration = builder.build()?.try_deserialize::<Self>()?;
        configuration.clone().validate()?;
        Ok(configuration)
    }
}
