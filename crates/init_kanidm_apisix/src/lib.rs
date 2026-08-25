pub mod error;
pub mod secrets;

use std::collections::BTreeMap;

use apisix::AdminClient;
use configuration::{ServiceProtectionMode, ServicesConfiguration};
use exec::DockerExec;
use http_client::{CurlExecClient, HttpClient, ReqwestClient};
use kanidm::{Kanidm, ProvisionedPerson};
use logger::{info, warn};

use crate::error::Result;
use crate::secrets::SecretStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reach {
    Direct,
    #[default]
    ViaDocker,
}

#[derive(Debug, Clone)]
pub struct RunSummary {
    pub routes: usize,
    pub persons: Vec<ProvisionedPerson>,
}

#[derive(Debug, Clone)]
pub struct NativeSummary {
    pub display_name: String,
    pub client_id: String,
    pub discovery_url: String,
    pub secret_key: String,
    pub group: String,
    pub redirect_url_missing: bool,
}

pub async fn run_kanidm_stage(
    configuration: &ServicesConfiguration,
) -> Result<BTreeMap<String, String>> {
    let runner = DockerExec::connect(&configuration.kanidm_cli_container)?;
    let kanidm = Kanidm::new(runner, configuration);

    info!(["INIT"], "checking kanidm session");
    kanidm.check_session().await?;
    info!(["INIT"], "authenticated as {}", configuration.kanidm_admin);

    let secrets = kanidm.provision_all(configuration).await?;

    let store = SecretStore::new(&configuration.secrets_dir)?;
    store.write_client_secrets(&secrets)?;
    info!(["INIT"], "wrote {} client secrets", secrets.len());

    let (_, created) = store.session_secret()?;
    if created {
        info!(["INIT"], "generated APISIX session secret");
    } else {
        info!(
            ["INIT"],
            "APISIX session secret already present, keeping it"
        );
    }

    Ok(secrets)
}

pub async fn run_persons_stage(
    configuration: &ServicesConfiguration,
) -> Result<Vec<ProvisionedPerson>> {
    if configuration.persons.is_empty() {
        return Ok(Vec::new());
    }

    let runner = DockerExec::connect(&configuration.kanidm_cli_container)?;
    let kanidm = Kanidm::new(runner, configuration);

    let persons = kanidm.provision_persons(configuration).await?;
    info!(["INIT"], "provisioned {} persons", persons.len());

    Ok(persons)
}

pub async fn run_apisix_stage(
    configuration: &ServicesConfiguration,
    reach: Reach,
) -> Result<usize> {
    let store = SecretStore::new(&configuration.secrets_dir)?;
    let secrets = store.read_client_secrets()?;
    let (session_secret, _) = store.session_secret()?;
    let api_key = store.apisix_admin_key()?;

    match reach {
        Reach::Direct => {
            let http = ReqwestClient::new()?;
            apply_routes(http, configuration, &secrets, &session_secret, api_key).await
        }
        Reach::ViaDocker => {
            let runner = DockerExec::connect(&configuration.curl_container)?;
            let http = CurlExecClient::new(runner);
            apply_routes(http, configuration, &secrets, &session_secret, api_key).await
        }
    }
}

async fn apply_routes<C: HttpClient>(
    http: C,
    configuration: &ServicesConfiguration,
    secrets: &BTreeMap<String, String>,
    session_secret: &str,
    api_key: String,
) -> Result<usize> {
    let admin = AdminClient::new(http, configuration, api_key);

    info!(["INIT"], "apisix preflight");
    admin.preflight().await?;
    info!(["INIT"], "admin api reachable, key accepted");

    Ok(admin
        .provision_all(configuration, secrets, session_secret)
        .await?)
}

pub async fn run_all(configuration: &ServicesConfiguration, reach: Reach) -> Result<RunSummary> {
    run_kanidm_stage(configuration).await?;
    let persons = run_persons_stage(configuration).await?;
    let routes = run_apisix_stage(configuration, reach).await?;

    Ok(RunSummary { routes, persons })
}

pub fn native_summary(configuration: &ServicesConfiguration) -> Vec<NativeSummary> {
    configuration
        .services
        .iter()
        .filter(|service| service.mode == ServiceProtectionMode::Native)
        .map(|service| {
            if service.native_redirect_url.is_none() {
                warn!(
                    ["INIT"],
                    "{}: native_redirect_url is unset — logins will fail until it is registered",
                    service.client_id
                );
            }

            NativeSummary {
                display_name: service.display_name.clone(),
                client_id: service.client_id.clone(),
                discovery_url: service.discovery_url(configuration),
                secret_key: service.secret_key(),
                group: service.group.clone(),
                redirect_url_missing: service.native_redirect_url.is_none(),
            }
        })
        .collect()
}
