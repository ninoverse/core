use bollard::{
    Docker,
    models::{
        ContainerStateStatusEnum, HealthStatusEnum, Mount, MountBindOptions, MountType,
        NetworkCreateRequest, RestartPolicy, RestartPolicyNameEnum, VolumeCreateRequest,
    },
    plugin::{ContainerCreateBody, EndpointSettings, HostConfig, NetworkingConfig, PortBinding},
    query_parameters::{
        CreateContainerOptions, CreateImageOptions, InspectContainerOptions,
        RemoveContainerOptionsBuilder, StartContainerOptions, StopContainerOptions,
    },
};
use futures::StreamExt;
use serde::Deserialize;
use serde_yaml::Value;
use std::sync::Arc;
use std::{
    collections::{BTreeMap, HashMap},
    env, fs,
    path::{Component, Path, PathBuf},
};
use thiserror::Error;
use tokio::{
    sync::{Barrier, broadcast::Sender, mpsc, watch},
    task::JoinSet,
    time::{Duration, sleep},
};

use logger::{error, info, warn};

#[derive(Default)]
pub struct DockerDefinitions {
    pub services: Vec<(String, ServiceConfig)>,
    pub networks: Vec<(String, Option<NetworkConfig>)>,
    pub volumes: Vec<(String, Option<VolumeConfig>)>,
}

#[derive(Deserialize)]
pub struct ComposeFile {
    pub services: Option<HashMap<String, ServiceConfig>>,
    pub networks: Option<HashMap<String, Option<NetworkConfig>>>,
    pub volumes: Option<HashMap<String, Option<VolumeConfig>>>,
}

#[derive(Deserialize)]
pub struct ServiceConfig {
    pub image: Option<String>,
    pub ports: Option<Vec<String>>,
    pub networks: Option<Vec<String>>,
    pub volumes: Option<Vec<VolumeSpec>>,
    #[serde(skip)]
    pub mounts: Vec<ResolvedMount>,
    pub environment: Option<Environment>,
    pub env_file: Option<EnvFile>,
    #[serde(skip)]
    pub env: Vec<String>,
    pub container_name: Option<String>,
    pub command: Option<String>,
    pub user: Option<String>,
    pub depends_on: Option<DependsOn>,
    pub restart: Option<String>,
    #[serde(skip)]
    pub restart_policy: Option<RestartPolicy>,
    #[serde(skip)]
    pub command_argv: Option<Vec<String>>,
}

impl ServiceConfig {
    /// Normalize `depends_on` (both the short list form and the long map form)
    /// into a flat list of `(dependency name, condition)`. List entries default
    /// to `service_started`.
    pub fn dependencies(&self) -> Vec<(String, DependencyCondition)> {
        match &self.depends_on {
            None => Vec::new(),
            Some(DependsOn::List(names)) => names
                .iter()
                .map(|name| (name.clone(), DependencyCondition::ServiceStarted))
                .collect(),
            Some(DependsOn::Map(entries)) => entries
                .iter()
                .map(|(name, entry)| (name.clone(), entry.condition))
                .collect(),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum DependsOn {
    /// `depends_on: [db, redis]`
    List(Vec<String>),
    /// `depends_on: { db: { condition: service_healthy } }`
    Map(HashMap<String, DependsOnEntry>),
}

#[derive(Deserialize)]
pub struct DependsOnEntry {
    #[serde(default)]
    pub condition: DependencyCondition,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Default, Debug)]
// Variant names intentionally mirror the Compose `service_*` condition strings.
#[allow(clippy::enum_variant_names)]
pub enum DependencyCondition {
    #[serde(rename = "service_started")]
    #[default]
    ServiceStarted,
    #[serde(rename = "service_healthy")]
    ServiceHealthy,
    #[serde(rename = "service_completed_successfully")]
    ServiceCompletedSuccessfully,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum VolumeSpec {
    Short(String),
    Long(LongVolumeSpec),
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum Environment {
    /// `environment: [KEY=VALUE, KEY]`
    List(Vec<String>),
    /// `environment: { KEY: value, KEY: }` — the values are `serde_yaml::Value`
    /// rather than `String` because a compose file may write `REPLICAS: 3` or
    /// `DEBUG: true`, and serde will not coerce a YAML integer or bool into a
    /// `String`. `Value::Null` is the bare `KEY:` inherit-from-host form.
    Map(HashMap<String, Value>),
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum EnvFile {
    /// `env_file: ./db.env`
    Single(String),
    /// `env_file: [./db.env, ./shared.env]`
    Many(Vec<String>),
}

#[derive(Deserialize)]
pub struct LongVolumeSpec {
    #[serde(rename = "type")]
    pub mount_type: String,
    pub source: Option<String>,
    pub target: String,
    #[serde(default)]
    pub read_only: bool,
}

pub struct ResolvedMount {
    pub mount_type: MountType,
    pub source: Option<String>,
    pub target: String,
    pub read_only: bool,
}

#[derive(Deserialize, Default)]
pub struct NetworkConfig {
    pub driver: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct VolumeConfig {
    pub driver: Option<String>,
}

#[derive(Error, Debug)]
pub enum DockerModuleError {
    #[error("Compose file directory not found: {0}")]
    ComposeFileDirectoryNotFound(String),

    #[error("Failed to read the file: {0}")]
    Read(#[from] std::io::Error),

    #[error("Failed to parse the yaml content: {0}")]
    Parse(#[from] serde_yaml::Error),

    #[error("Bollard(docker) error: {0}")]
    Bollard(#[from] bollard::errors::Error),

    #[error("Invalid volume specification: {0}")]
    InvalidVolumeSpec(String),

    #[error("Invalid restart policy: {0}")]
    InvalidRestartPolicy(String),

    /// A service's `command:` could not be split into arguments — an
    /// unterminated quote or a trailing backslash.
    #[error("Service [{service}] has a malformed command: {command}")]
    InvalidCommand { service: String, command: String },

    /// A service's environment could not be resolved — a non-scalar mapping
    /// value, or an unreadable or malformed `env_file`.
    #[error("Service [{service}] has an invalid environment: {detail}")]
    InvalidEnvironment { service: String, detail: String },

    #[error("Service [{service}] depends on unknown service [{dependency}]")]
    UnknownDependency { service: String, dependency: String },

    #[error("Dependency cycle detected among services: {0}")]
    DependencyCycle(String),

    #[error("Service [{0}] did not reach a running state in time")]
    ServiceStartTimeout(String),
}

fn resolve_pull_tag(image_reference: &str) -> Option<String> {
    let last_segment = image_reference
        .rsplit('/')
        .next()
        .unwrap_or(image_reference);

    if last_segment.contains(':') || last_segment.contains('@') {
        None
    } else {
        Some("latest".to_string())
    }
}

/// Map the compose `restart:` key onto a Docker restart policy. An absent key
/// means `no`, matching the Compose default.
fn resolve_restart_policy(restart: Option<&str>) -> Result<RestartPolicy, DockerModuleError> {
    let (name, maximum_retry_count) = match restart {
        None | Some("no") => (RestartPolicyNameEnum::NO, None),
        Some("always") => (RestartPolicyNameEnum::ALWAYS, None),
        Some("unless-stopped") => (RestartPolicyNameEnum::UNLESS_STOPPED, None),
        Some("on-failure") => (RestartPolicyNameEnum::ON_FAILURE, None),
        Some(policy) => match policy.strip_prefix("on-failure:") {
            Some(max_retries) => {
                let max_retries = max_retries
                    .parse::<i64>()
                    .map_err(|_| DockerModuleError::InvalidRestartPolicy(policy.to_string()))?;
                if max_retries < 0 {
                    return Err(DockerModuleError::InvalidRestartPolicy(policy.to_string()));
                }
                (RestartPolicyNameEnum::ON_FAILURE, Some(max_retries))
            }
            None => {
                return Err(DockerModuleError::InvalidRestartPolicy(policy.to_string()));
            }
        },
    };

    Ok(RestartPolicy {
        name: Some(name),
        maximum_retry_count,
    })
}

/// Split the compose `command:` string into an argv vector. `shlex::split`
/// returns `None` on an unterminated quote or trailing backslash; surfacing that
/// as an error keeps a malformed command from silently falling back to the
/// image's default command.
fn resolve_command(
    service_name: &str,
    command: Option<&str>,
) -> Result<Option<Vec<String>>, DockerModuleError> {
    match command {
        None => Ok(None),
        Some(command) => {
            shlex::split(command)
                .map(Some)
                .ok_or_else(|| DockerModuleError::InvalidCommand {
                    service: service_name.to_string(),
                    command: command.to_string(),
                })
        }
    }
}

/// Parse the `KEY=VALUE` lines of an env file. The caller owns the file read, so
/// this stays a pure function over the content. Blank lines and `#` comments are
/// skipped; the split is on the first `=` so a value may itself contain one; one
/// layer of matching surrounding quotes is stripped. `export KEY=…` prefixes,
/// multi-line values, and interpolation inside env files are not supported.
fn parse_env_file_content(content: &str) -> Result<Vec<(String, String)>, String> {
    let mut entries = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("line [{}] is not KEY=VALUE", line))?;

        let value = value.trim();
        let unquoted = match value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) {
            Some(unquoted) => unquoted,
            None => value
                .strip_prefix('\'')
                .and_then(|v| v.strip_suffix('\''))
                .unwrap_or(value),
        };

        entries.push((key.trim().to_string(), unquoted.to_string()));
    }

    Ok(entries)
}

/// Merge a service's `env_file` and `environment` into the `KEY=VALUE` list
/// Docker expects. Both the list form (`- KEY=VALUE`, `- KEY`) and the mapping
/// form (`KEY: value`, `KEY:`) are accepted. A key given without a value
/// inherits from `variables` and is dropped when unset, matching Compose — a
/// bare key asks for a value to be passed through if present, unlike `${VAR}`,
/// which declares it required. Compose precedence: env files in the order
/// listed, then inline `environment` on top.
fn resolve_environment(
    service_name: &str,
    environment: Option<Environment>,
    env_file: Option<EnvFile>,
    variables: &BTreeMap<String, String>,
    compose_dir: &Path,
) -> Result<Vec<String>, DockerModuleError> {
    // A map rather than a Vec: Docker resolves a duplicated key to the last
    // occurrence, so collapsing them here keeps what the container sees and
    // what the compose file says in agreement.
    let mut resolved: BTreeMap<String, String> = BTreeMap::new();

    let env_files = match env_file {
        None => Vec::new(),
        Some(EnvFile::Single(path)) => vec![path],
        Some(EnvFile::Many(paths)) => paths,
    };

    for entry in env_files {
        let path = resolve_host_path(&entry, compose_dir)?;
        let content =
            fs::read_to_string(&path).map_err(|source| DockerModuleError::InvalidEnvironment {
                service: service_name.to_string(),
                detail: format!("cannot read env_file [{}]: {}", path.display(), source),
            })?;
        let parsed = parse_env_file_content(&content).map_err(|detail| {
            DockerModuleError::InvalidEnvironment {
                service: service_name.to_string(),
                detail: format!("env_file [{}]: {}", path.display(), detail),
            }
        })?;
        resolved.extend(parsed);
    }

    match environment {
        None => {}
        Some(Environment::List(entries)) => {
            for entry in entries {
                match entry.split_once('=') {
                    Some((key, value)) => {
                        resolved.insert(key.trim().to_string(), value.to_string());
                    }
                    None => {
                        let key = entry.trim();
                        if let Some(inherited) = variables.get(key) {
                            resolved.insert(key.to_string(), inherited.clone());
                        }
                    }
                }
            }
        }
        Some(Environment::Map(entries)) => {
            for (key, value) in entries {
                match value {
                    Value::Null => {
                        if let Some(inherited) = variables.get(&key) {
                            resolved.insert(key, inherited.clone());
                        }
                    }
                    Value::String(value) => {
                        resolved.insert(key, value);
                    }
                    Value::Number(value) => {
                        resolved.insert(key, value.to_string());
                    }
                    Value::Bool(value) => {
                        resolved.insert(key, value.to_string());
                    }
                    _ => {
                        return Err(DockerModuleError::InvalidEnvironment {
                            service: service_name.to_string(),
                            detail: format!("value of [{}] is not a scalar", key),
                        });
                    }
                }
            }
        }
    }

    Ok(resolved
        .into_iter()
        .map(|(key, value)| format!("{}={}", key, value))
        .collect())
}

fn is_host_path(source: &str) -> bool {
    source.starts_with('/')
        || source.starts_with("./")
        || source.starts_with("../")
        || source == "~"
        || source.starts_with("~/")
}

fn resolve_host_path(source: &str, compose_dir: &Path) -> Result<PathBuf, DockerModuleError> {
    let expanded = if source == "~" || source.starts_with("~/") {
        let home_dir = env::var("HOME").map_err(|_| {
            DockerModuleError::InvalidVolumeSpec(format!(
                "cannot expand [{}]: HOME is not set",
                source
            ))
        })?;
        match source.strip_prefix("~/") {
            Some(rest) => Path::new(&home_dir).join(rest),
            None => PathBuf::from(home_dir),
        }
    } else {
        PathBuf::from(source)
    };

    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        compose_dir.join(expanded)
    };

    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push("..");
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }

    Ok(normalized)
}

fn resolve_short_volume(
    volume_spec: &str,
    compose_dir: &Path,
) -> Result<ResolvedMount, DockerModuleError> {
    let mut parts = volume_spec.splitn(3, ':');
    let first = parts.next().unwrap_or_default();
    let second = parts.next();
    let modes = parts.next().unwrap_or_default();

    if first.is_empty() {
        return Err(DockerModuleError::InvalidVolumeSpec(
            volume_spec.to_string(),
        ));
    }

    let (source, target) = match second {
        Some(target) if !target.is_empty() => (Some(first), target),
        Some(_) => {
            return Err(DockerModuleError::InvalidVolumeSpec(
                volume_spec.to_string(),
            ));
        }
        None => (None, first),
    };

    let mut read_only = false;
    for mode in modes.split(',').filter(|mode| !mode.is_empty()) {
        match mode {
            "ro" => read_only = true,
            "rw" => read_only = false,
            unsupported_mode => warn!(
                ["DOCKER_INIT"],
                "Mount option [{}] in [{}] is not supported by the Mounts API, ignoring.",
                unsupported_mode,
                volume_spec
            ),
        }
    }

    let (mount_type, resolved_source) = match source {
        None => (MountType::VOLUME, None),
        Some(source) if is_host_path(source) => (
            MountType::BIND,
            Some(
                resolve_host_path(source, compose_dir)?
                    .to_string_lossy()
                    .into_owned(),
            ),
        ),
        Some(source) => (MountType::VOLUME, Some(source.to_string())),
    };

    Ok(ResolvedMount {
        mount_type,
        source: resolved_source,
        target: target.to_string(),
        read_only,
    })
}

fn resolve_long_volume(
    volume_spec: LongVolumeSpec,
    compose_dir: &Path,
) -> Result<ResolvedMount, DockerModuleError> {
    let mount_type = match volume_spec.mount_type.as_str() {
        "bind" => MountType::BIND,
        "volume" => MountType::VOLUME,
        "tmpfs" => MountType::TMPFS,
        "npipe" => MountType::NPIPE,
        unsupported_type => {
            return Err(DockerModuleError::InvalidVolumeSpec(format!(
                "unsupported mount type [{}] for target [{}]",
                unsupported_type, volume_spec.target
            )));
        }
    };

    let resolved_source = match (mount_type, volume_spec.source) {
        (MountType::TMPFS, _) => None,
        (MountType::BIND, Some(source)) => Some(
            resolve_host_path(&source, compose_dir)?
                .to_string_lossy()
                .into_owned(),
        ),
        (MountType::BIND, None) => {
            return Err(DockerModuleError::InvalidVolumeSpec(format!(
                "bind mount for target [{}] is missing a source",
                volume_spec.target
            )));
        }
        (_, source) => source,
    };

    Ok(ResolvedMount {
        mount_type,
        source: resolved_source,
        target: volume_spec.target,
        read_only: volume_spec.read_only,
    })
}

fn resolve_volume_spec(
    volume_spec: VolumeSpec,
    compose_dir: &Path,
) -> Result<ResolvedMount, DockerModuleError> {
    match volume_spec {
        VolumeSpec::Short(short_spec) => resolve_short_volume(&short_spec, compose_dir),
        VolumeSpec::Long(long_spec) => resolve_long_volume(long_spec, compose_dir),
    }
}

fn build_bollard_mount(resolved_mount: &ResolvedMount) -> Mount {
    Mount {
        typ: Some(resolved_mount.mount_type),
        source: resolved_mount.source.clone(),
        target: Some(resolved_mount.target.clone()),
        read_only: Some(resolved_mount.read_only),
        bind_options: matches!(resolved_mount.mount_type, MountType::BIND).then(|| {
            MountBindOptions {
                create_mountpoint: Some(true),
                ..Default::default()
            }
        }),
        ..Default::default()
    }
}

pub async fn find_docker_definitions() -> Result<DockerDefinitions, DockerModuleError> {
    let compose_file_dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/containers"));
    let mut definitions = DockerDefinitions::default();
    let variables: BTreeMap<String, String> = env::vars().collect();

    if compose_file_dir.exists() && compose_file_dir.is_dir() {
        let compose_files = fs::read_dir(compose_file_dir)?;

        for file in compose_files {
            let path = file?.path();
            if path.is_file() {
                if let Some(extension) = path.extension() {
                    if extension == "yml" || extension == "yaml" {
                        let yaml_content = fs::read_to_string(&path)?;
                        let compose_dir = path.parent().unwrap_or(compose_file_dir);

                        let compose_config: ComposeFile = serde_yaml::from_str(&yaml_content)?;

                        if let Some(services) = compose_config.services {
                            for (raw_name, mut config) in services {
                                let unique_name = raw_name.to_string();
                                if let Some(ref mut nets) = config.networks {
                                    for net in nets.iter_mut() {
                                        *net = net.to_string();
                                    }
                                }
                                if let Some(volumes) = config.volumes.take() {
                                    for volume_spec in volumes {
                                        config
                                            .mounts
                                            .push(resolve_volume_spec(volume_spec, compose_dir)?);
                                    }
                                }
                                config.restart_policy =
                                    Some(resolve_restart_policy(config.restart.as_deref())?);
                                config.command_argv =
                                    resolve_command(&unique_name, config.command.as_deref())?;
                                config.env = resolve_environment(
                                    &unique_name,
                                    config.environment.take(),
                                    config.env_file.take(),
                                    &variables,
                                    compose_dir,
                                )?;
                                definitions.services.push((unique_name, config));
                            }
                        }

                        if let Some(networks) = compose_config.networks {
                            for (raw_name, config) in networks {
                                let unique_name = raw_name.to_string();
                                definitions.networks.push((unique_name, config));
                            }
                        }

                        if let Some(volumes) = compose_config.volumes {
                            for (raw_name, config) in volumes {
                                let unique_name = raw_name.to_string();
                                definitions.volumes.push((unique_name, config));
                            }
                        }
                    }
                }
            }
        }
        info!(
            ["DOCKER_INIT"],
            "Found {} services, {} networks, {} volumes to initialize",
            definitions.services.len(),
            definitions.networks.len(),
            definitions.volumes.len()
        );
        Ok(definitions)
    } else {
        let compose_file_dir_path: String = compose_file_dir.to_string_lossy().into_owned();
        Err(DockerModuleError::ComposeFileDirectoryNotFound(
            compose_file_dir_path,
        ))
    }
}

pub async fn create_docker_client() -> Result<Docker, DockerModuleError> {
    Ok(Docker::connect_with_defaults()?)
}

pub async fn create_docker_networks(
    docker_networks: Vec<(String, Option<NetworkConfig>)>,
    docker: &Docker,
) -> Result<(), DockerModuleError> {
    for docker_network in docker_networks {
        let network_name = docker_network.0;

        let network_config = NetworkCreateRequest {
            name: network_name.clone(),
            driver: docker_network.1.unwrap_or_default().driver,
            ..Default::default()
        };

        match docker.create_network(network_config).await {
            Ok(_) => info!(
                ["DOCKER_INIT"],
                "Network [{}] created successfully.", &network_name
            ),
            Err(bollard::errors::Error::DockerResponseServerError { status_code, .. }) => {
                if status_code == 409 {
                    info!(
                        ["DOCKER_INIT"],
                        "Network [{}] already exists. skipping...", &network_name
                    );
                }
            }
            Err(network_creation_error) => {
                return Err(DockerModuleError::Bollard(network_creation_error));
            }
        }
    }
    Ok(())
}

pub async fn create_docker_volumes(
    docker_volumes: Vec<(String, Option<VolumeConfig>)>,
    docker: &Docker,
) -> Result<(), DockerModuleError> {
    for docker_volume in docker_volumes {
        let volume_name = docker_volume.0;

        let volume_config = VolumeCreateRequest {
            name: Some(volume_name.clone()),
            driver: docker_volume.1.unwrap_or_default().driver,
            ..Default::default()
        };

        match docker.create_volume(volume_config).await {
            Ok(_) => info!(
                ["DOCKER_INIT"],
                "Volume [{}] created successfully.", &volume_name
            ),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 409,
                message,
            }) => {
                warn!(
                    ["DOCKER_INIT"],
                    "Warning: Volume [{}] exists but has a config conflict: {}, proceeding anyway...",
                    &volume_name,
                    message
                );
            }
            Err(volume_creation_error) => {
                return Err(DockerModuleError::Bollard(volume_creation_error));
            }
        }
    }
    Ok(())
}

async fn stop_and_cleanup_container(
    docker: &Docker,
    service_name: &str,
    remove_containers_on_shutdown: bool,
) {
    if docker
        .inspect_container(service_name, None::<InspectContainerOptions>)
        .await
        .is_err()
    {
        info!(
            ["DOCKER_SHUTDOWN"],
            "Container '{}' was never created; nothing to stop.", service_name
        );
        return;
    }

    let stop_options = StopContainerOptions {
        signal: Some("SIGTERM".to_string()),
        t: Some(10),
    };
    match docker
        .stop_container(service_name, Some(stop_options))
        .await
    {
        Ok(_) => {
            info!(
                ["DOCKER_SHUTDOWN"],
                "Container '{}' stopped gracefully.", service_name
            );
            if remove_containers_on_shutdown {
                let remove_container_options =
                    RemoveContainerOptionsBuilder::default().force(true).build();
                match docker
                    .remove_container(service_name, Some(remove_container_options))
                    .await
                {
                    Ok(_) => info!(["DOCKER_SHUTDOWN"], "Container '{}' removed.", service_name),
                    Err(remove_container_error) => error!(
                        ["DOCKER_SHUTDOWN"],
                        "Failed to remove '{}': {}", service_name, remove_container_error
                    ),
                };
            } else {
                info!(
                    ["DOCKER_SHUTDOWN"],
                    "Container '{}' left in place (remove_containers_on_shutdown=false).",
                    service_name
                );
            }
        }
        Err(stop_container_error) => error!(
            ["DOCKER_SHUTDOWN"],
            "Failed to stop '{}': {}", service_name, stop_container_error
        ),
    }
}

/// Readiness a service task broadcasts to its dependents. Boot failure is
/// signalled by the sender being dropped (the task ends without sending
/// `Started`), which surfaces to a waiting dependent as a receiver error.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ServiceStatus {
    Pending,
    Started,
}

/// Poll cadence and cap when waiting for a dependency to become healthy or to
/// complete. The cap keeps a never-ready dependency from hanging startup forever.
const DEPENDENCY_POLL_INTERVAL: Duration = Duration::from_secs(1);
const DEPENDENCY_WAIT_MAX_ATTEMPTS: u32 = 120;

/// Validate the `depends_on` graph before booting anything: every referenced
/// dependency must be a known service, and there must be no cycles. Failing here
/// avoids a startup deadlock on an unsatisfiable graph.
fn validate_dependency_graph(
    services: &[(String, ServiceConfig)],
) -> Result<(), DockerModuleError> {
    use std::collections::HashSet;

    let known: HashSet<String> = services.iter().map(|(name, _)| name.clone()).collect();

    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    for (name, config) in services {
        let mut deps = Vec::new();
        for (dependency, _) in config.dependencies() {
            if !known.contains(&dependency) {
                return Err(DockerModuleError::UnknownDependency {
                    service: name.clone(),
                    dependency,
                });
            }
            deps.push(dependency);
        }
        graph.insert(name.clone(), deps);
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Visiting,
        Visited,
    }

    fn visit(
        node: &str,
        graph: &HashMap<String, Vec<String>>,
        marks: &mut HashMap<String, Mark>,
        stack: &mut Vec<String>,
    ) -> Result<(), DockerModuleError> {
        match marks.get(node) {
            Some(Mark::Visited) => return Ok(()),
            Some(Mark::Visiting) => {
                let cycle_start = stack.iter().position(|n| n == node).unwrap_or(0);
                let mut cycle: Vec<String> = stack[cycle_start..].to_vec();
                cycle.push(node.to_string());
                return Err(DockerModuleError::DependencyCycle(cycle.join(" -> ")));
            }
            None => {}
        }

        marks.insert(node.to_string(), Mark::Visiting);
        stack.push(node.to_string());
        if let Some(dependencies) = graph.get(node) {
            for dependency in dependencies {
                visit(dependency, graph, marks, stack)?;
            }
        }
        stack.pop();
        marks.insert(node.to_string(), Mark::Visited);
        Ok(())
    }

    let mut marks: HashMap<String, Mark> = HashMap::new();
    for (name, _) in services {
        let mut stack = Vec::new();
        visit(name, &graph, &mut marks, &mut stack)?;
    }

    Ok(())
}

/// Wait until `dependency_name` satisfies `condition`. Returns `Err` (with a
/// short reason) if the dependency failed to start, became unhealthy, exited
/// non-zero, or did not reach the condition within the cap.
async fn wait_for_dependency(
    docker: &Docker,
    dependency_name: &str,
    condition: DependencyCondition,
    status_receiver: &mut watch::Receiver<ServiceStatus>,
) -> Result<(), String> {
    // First wait until the dependency's container has at least been launched. A
    // closed channel means the dependency task ended without starting.
    if status_receiver
        .wait_for(|status| *status == ServiceStatus::Started)
        .await
        .is_err()
    {
        return Err("failed to start".to_string());
    }

    match condition {
        DependencyCondition::ServiceStarted => Ok(()),
        DependencyCondition::ServiceHealthy => wait_for_healthy(docker, dependency_name).await,
        DependencyCondition::ServiceCompletedSuccessfully => {
            wait_for_completion(docker, dependency_name).await
        }
    }
}

async fn wait_for_healthy(docker: &Docker, dependency_name: &str) -> Result<(), String> {
    for _ in 0..DEPENDENCY_WAIT_MAX_ATTEMPTS {
        let state = docker
            .inspect_container(dependency_name, None::<InspectContainerOptions>)
            .await
            .map_err(|inspect_error| format!("could not be inspected: {}", inspect_error))?
            .state;

        match state.as_ref().and_then(|state| state.health.as_ref()) {
            Some(health) => match health.status {
                Some(HealthStatusEnum::HEALTHY) => return Ok(()),
                Some(HealthStatusEnum::UNHEALTHY) => return Err("became unhealthy".to_string()),
                _ => {}
            },
            // No healthcheck defined on the container: fall back to "running".
            None => {
                if state.and_then(|state| state.running).unwrap_or(false) {
                    return Ok(());
                }
            }
        }

        sleep(DEPENDENCY_POLL_INTERVAL).await;
    }

    Err("did not become healthy in time".to_string())
}

async fn wait_for_completion(docker: &Docker, dependency_name: &str) -> Result<(), String> {
    for _ in 0..DEPENDENCY_WAIT_MAX_ATTEMPTS {
        let state = docker
            .inspect_container(dependency_name, None::<InspectContainerOptions>)
            .await
            .map_err(|inspect_error| format!("could not be inspected: {}", inspect_error))?
            .state;

        if let Some(state) = state {
            let terminal = matches!(
                state.status,
                Some(ContainerStateStatusEnum::EXITED | ContainerStateStatusEnum::DEAD)
            );
            if terminal {
                return match state.exit_code.unwrap_or(-1) {
                    0 => Ok(()),
                    exit_code => Err(format!("exited with code {}", exit_code)),
                };
            }
        }

        sleep(DEPENDENCY_POLL_INTERVAL).await;
    }

    Err("did not complete in time".to_string())
}

pub async fn start_docker_container(
    docker_services: Vec<(String, ServiceConfig)>,
    docker: &Docker,
    shutdown_broadcast_sender: &Sender<()>,
    join_set: &mut JoinSet<()>,
    remove_containers_on_shutdown: bool,
) -> Result<(), DockerModuleError> {
    validate_dependency_graph(&docker_services)?;

    let barrier = Arc::new(Barrier::new(docker_services.len() + 1));
    let mut startup_shutdown_receiver = shutdown_broadcast_sender.subscribe();

    // A task that fails to boot never reaches the barrier, so the barrier alone
    // would hang startup forever. Failures are reported here instead.
    let (failure_sender, mut failure_receiver) =
        mpsc::channel::<DockerModuleError>(docker_services.len().max(1));

    // One readiness channel per service. Producers publish `Started`; dependents
    // subscribe to the channels of the services they depend on.
    let mut status_senders: HashMap<String, watch::Sender<ServiceStatus>> = HashMap::new();
    let mut status_receivers: HashMap<String, watch::Receiver<ServiceStatus>> = HashMap::new();
    for (service_name, _) in &docker_services {
        let (sender, receiver) = watch::channel(ServiceStatus::Pending);
        status_senders.insert(service_name.clone(), sender);
        status_receivers.insert(service_name.clone(), receiver);
    }

    for (service_name, service_config) in docker_services {
        let barrier_cloned = barrier.clone();
        let docker_cloned = docker.clone();
        let failure_sender = failure_sender.clone();
        let mut shutdown_broadcast_sender_subscribed = shutdown_broadcast_sender.subscribe();

        let status_sender = status_senders
            .remove(&service_name)
            .expect("every service has a status sender");
        let dependency_receivers: Vec<(
            String,
            DependencyCondition,
            watch::Receiver<ServiceStatus>,
        )> = service_config
            .dependencies()
            .into_iter()
            .map(|(dependency_name, condition)| {
                let receiver = status_receivers
                    .get(&dependency_name)
                    .expect("dependency validated to exist")
                    .clone();
                (dependency_name, condition, receiver)
            })
            .collect();

        join_set.spawn(async move {
            // Phase 0: wait for every dependency to satisfy its condition.
            for (dependency_name, condition, mut dependency_receiver) in dependency_receivers {
                tokio::select! {
                    wait_result = wait_for_dependency(
                        &docker_cloned,
                        &dependency_name,
                        condition,
                        &mut dependency_receiver,
                    ) => {
                        if let Err(reason) = wait_result {
                            warn!(
                                ["DOCKER_INIT"],
                                "Service [{}] will not start: dependency [{}] {}.",
                                service_name, dependency_name, reason
                            );
                            return;
                        }
                    }
                    _ = shutdown_broadcast_sender_subscribed.recv() => {
                        info!(
                            ["DOCKER_SHUTDOWN"],
                            "Shutdown signal received by task '{}' while waiting for dependency '{}'. Aborting boot...",
                            service_name, dependency_name
                        );
                        return;
                    }
                }
            }

            tokio::select! {
                boot_result = boot_service(&docker_cloned, &service_name, service_config) => {
                    // `boot_service` already logged the cause. Report it upward so
                    // startup aborts; dropping `status_sender` on the way out lets
                    // dependents resolve their wait with an error instead of hanging.
                    if let Err(boot_error) = boot_result {
                        let _ = failure_sender.send(boot_error).await;
                        return;
                    }
                }
                _ = shutdown_broadcast_sender_subscribed.recv() => {
                    info!(
                        ["DOCKER_SHUTDOWN"],
                        "Shutdown signal received by task '{}' during startup. Aborting boot...",
                        service_name
                    );
                    stop_and_cleanup_container(
                        &docker_cloned,
                        &service_name,
                        remove_containers_on_shutdown,
                    )
                    .await;
                    return;
                }
            }

            // Publish readiness so dependents can proceed.
            let _ = status_sender.send(ServiceStatus::Started);

            info!(
                ["DOCKER_INIT"],
                "Service [{}] is started and healthy. Signaling barrier...", service_name
            );

            tokio::select! {
                _ = barrier_cloned.wait() => {}
                _ = shutdown_broadcast_sender_subscribed.recv() => {
                    info!(
                        ["DOCKER_SHUTDOWN"],
                        "Shutdown signal received by task '{}' while waiting at the barrier. Stopping container...",
                        service_name
                    );
                    stop_and_cleanup_container(
                        &docker_cloned,
                        &service_name,
                        remove_containers_on_shutdown,
                    )
                    .await;
                    return;
                }
            }

            // Restarts are owned by the Docker daemon through the container's
            // restart policy, so the task only waits for shutdown from here.
            let _ = shutdown_broadcast_sender_subscribed.recv().await;
            info!(
                ["DOCKER_SHUTDOWN"],
                "Shutdown signal received by task '{}'. Stopping container...", service_name
            );

            stop_and_cleanup_container(&docker_cloned, &service_name, remove_containers_on_shutdown)
                .await;
        });
    }

    tokio::select! {
        _ = barrier.wait() => {
            info!(["DOCKER_INIT"], "All Docker definitions are up");
        }
        Some(boot_error) = failure_receiver.recv() => {
            return Err(boot_error);
        }
        _ = startup_shutdown_receiver.recv() => {
            info!(
                ["DOCKER_INIT"],
                "Shutdown requested before all services were up. Skipping remaining startup."
            );
        }
    }
    Ok(())
}

async fn boot_service(
    docker: &Docker,
    service_name: &str,
    service_config: ServiceConfig,
) -> Result<(), DockerModuleError> {
    info!(
        ["DOCKER_INIT"],
        "Booting up service: {} (Image: {:?})", service_name, service_config.image
    );

    let mut host_config = HostConfig {
        restart_policy: service_config.restart_policy.clone(),
        ..Default::default()
    };

    if !service_config.mounts.is_empty() {
        host_config.mounts = Some(
            service_config
                .mounts
                .iter()
                .map(build_bollard_mount)
                .collect(),
        );
    }

    if let Some(ports) = &service_config.ports {
        let mut port_bindings = HashMap::new();
        for port_mapping in ports {
            let parts: Vec<&str> = port_mapping.split(':').collect();
            let (host_ip, host_port, container_port) = match parts.as_slice() {
                [host_port, container_port] => ("0.0.0.0", *host_port, *container_port),
                [host_ip, host_port, container_port] => (*host_ip, *host_port, *container_port),
                _ => continue,
            };

            port_bindings.insert(
                format!("{}/tcp", container_port),
                Some(vec![PortBinding {
                    host_ip: Some(host_ip.to_string()),
                    host_port: Some(host_port.to_string()),
                }]),
            );
        }
        host_config.port_bindings = Some(port_bindings);
    }

    let mut network_endpoints = HashMap::new();
    if let Some(networks) = &service_config.networks {
        for net in networks {
            network_endpoints.insert(net.clone(), EndpointSettings::default());
        }
    }

    let environment = service_config.env;

    let _container_name = service_config.container_name.unwrap_or_default();

    let user = service_config.user.unwrap_or_default();

    let container_configuration = ContainerCreateBody {
        image: service_config.image.clone(),
        host_config: Some(host_config),
        networking_config: Some(NetworkingConfig {
            endpoints_config: Some(network_endpoints),
        }),
        env: Some(environment),
        cmd: service_config.command_argv,
        user: Some(user),
        ..Default::default()
    };

    let create_options = CreateContainerOptions {
        name: Some(service_name.to_string()),
        platform: String::new(),
    };

    if let Some(image_name) = &service_config.image {
        let image_tag = resolve_pull_tag(image_name);

        info!(
            ["DOCKER_IMAGES"],
            "Checking image [{}{}] for service [{}]...",
            image_name,
            if let Some(image_tag) = &image_tag {
                format!(":{:}", image_tag)
            } else {
                String::new()
            },
            service_name
        );

        let pull_options = CreateImageOptions {
            from_image: Some(image_name.clone()),
            tag: image_tag,
            ..Default::default()
        };

        let mut image_stream = docker.create_image(Some(pull_options), None, None);

        while let Some(update) = image_stream.next().await {
            match update {
                Ok(info) => {
                    if let Some(status) = info.status {
                        info!(
                            ["DOCKER_IMAGES"],
                            "Pulling image [{}] for service [{}]: {}",
                            image_name,
                            service_name,
                            status
                        );
                    }
                }
                Err(image_pull_error) => {
                    error!(
                        ["DOCKER_IMAGES"],
                        "Fatal error pulling image [{}]: {}", image_name, image_pull_error
                    );
                    return Err(image_pull_error.into());
                }
            }
        }
        info!(["DOCKER_INIT"], "Image [{}] is ready.", image_name);
    }

    match docker
        .create_container(Some(create_options), container_configuration)
        .await
    {
        Ok(_) => info!(
            ["DOCKER_INIT"],
            "Container [{}] created successfully.", &service_name
        ),
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 409, ..
        }) => {
            warn!(
                ["DOCKER_INIT"],
                "Warning: Container [{}] exists, proceeding anyway...", &service_name
            );
        }
        Err(container_creation_error) => {
            error!(
                ["DOCKER_INIT"],
                "Failed to create container [{}]: {}", service_name, container_creation_error
            );
            return Err(container_creation_error.into());
        }
    };

    info!(["DOCKER_INIT"], "Starting container [{}].", &service_name);
    match docker
        .start_container(service_name, None::<StartContainerOptions>)
        .await
    {
        Ok(_) => {
            info!(
                ["DOCKER_INIT"],
                "Container [{}] started successfully, waiting for healthy state.", &service_name
            );
            let mut launched = false;
            for _ in 0..DEPENDENCY_WAIT_MAX_ATTEMPTS {
                let inspect = docker
                    .inspect_container(service_name, None::<InspectContainerOptions>)
                    .await?;
                if let Some(state) = inspect.state {
                    let running = state.running.unwrap_or(false);
                    // A short-lived container (e.g. a `service_completed_successfully`
                    // dependency) may exit before `running` is ever observed true;
                    // treat a terminal state as launched so the task can proceed.
                    let terminal = matches!(
                        state.status,
                        Some(ContainerStateStatusEnum::EXITED | ContainerStateStatusEnum::DEAD)
                    );
                    if running || terminal {
                        launched = true;
                        break;
                    }
                }
                sleep(DEPENDENCY_POLL_INTERVAL).await;
            }

            if !launched {
                error!(
                    ["DOCKER_INIT"],
                    "Container [{}] did not reach a running state in time.", &service_name
                );
                return Err(DockerModuleError::ServiceStartTimeout(
                    service_name.to_string(),
                ));
            }
        }
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 409, ..
        }) => {
            warn!(
                ["DOCKER_INIT"],
                "Warning: Container [{}] exists, proceeding anyway...", &service_name
            );
        }
        Err(container_start_error) => {
            error!(
                ["DOCKER_INIT"],
                "Failed to start container [{}]: {}", service_name, container_start_error
            );
            return Err(container_start_error.into());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_from_yaml(yaml: &str) -> ServiceConfig {
        serde_yaml::from_str(yaml).expect("valid service yaml")
    }

    fn services(specs: &[(&str, &str)]) -> Vec<(String, ServiceConfig)> {
        specs
            .iter()
            .map(|(name, yaml)| (name.to_string(), service_from_yaml(yaml)))
            .collect()
    }

    fn variables(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    /// Resolve a service whose `env_file` entries point at real files, written
    /// into a temporary directory named after `label` and removed afterwards.
    /// Env-file paths in `yaml` are relative to that directory.
    fn env_with_files(label: &str, yaml: &str, files: &[(&str, &str)]) -> Vec<String> {
        let dir = env::temp_dir().join(format!("core-docker-{}-{}", std::process::id(), label));
        fs::create_dir_all(&dir).expect("temp dir");
        for (name, content) in files {
            fs::write(dir.join(name), content).expect("write env file");
        }

        let service = service_from_yaml(yaml);
        let resolved = resolve_environment(
            "app",
            service.environment,
            service.env_file,
            &variables(&[]),
            &dir,
        );

        fs::remove_dir_all(&dir).expect("clean up temp dir");
        resolved.expect("valid environment")
    }

    fn env_of(yaml: &str, variables: &BTreeMap<String, String>) -> Vec<String> {
        let service = service_from_yaml(yaml);
        resolve_environment(
            "app",
            service.environment,
            service.env_file,
            variables,
            Path::new("/containers"),
        )
        .expect("valid environment")
    }

    #[test]
    fn depends_on_list_form_defaults_to_started() {
        let service = service_from_yaml("image: busybox\ndepends_on:\n  - db\n  - redis\n");
        let deps: HashMap<String, DependencyCondition> =
            service.dependencies().into_iter().collect();
        assert_eq!(deps.len(), 2);
        assert_eq!(deps["db"], DependencyCondition::ServiceStarted);
        assert_eq!(deps["redis"], DependencyCondition::ServiceStarted);
    }

    #[test]
    fn depends_on_map_form_reads_conditions() {
        let service = service_from_yaml(
            "image: busybox\n\
             depends_on:\n\
             \x20 db:\n\
             \x20   condition: service_healthy\n\
             \x20 init:\n\
             \x20   condition: service_completed_successfully\n",
        );
        let deps: HashMap<String, DependencyCondition> =
            service.dependencies().into_iter().collect();
        assert_eq!(deps["db"], DependencyCondition::ServiceHealthy);
        assert_eq!(
            deps["init"],
            DependencyCondition::ServiceCompletedSuccessfully
        );
    }

    #[test]
    fn depends_on_map_form_defaults_missing_condition() {
        let service = service_from_yaml("image: busybox\ndepends_on:\n  db: {}\n");
        let deps: HashMap<String, DependencyCondition> =
            service.dependencies().into_iter().collect();
        assert_eq!(deps["db"], DependencyCondition::ServiceStarted);
    }

    #[test]
    fn no_depends_on_yields_no_dependencies() {
        let service = service_from_yaml("image: busybox\n");
        assert!(service.dependencies().is_empty());
    }

    #[test]
    fn validate_accepts_valid_dag() {
        let services = services(&[
            ("db", "image: postgres\n"),
            ("app", "image: busybox\ndepends_on:\n  - db\n"),
        ]);
        assert!(validate_dependency_graph(&services).is_ok());
    }

    #[test]
    fn validate_rejects_unknown_dependency() {
        let services = services(&[("app", "image: busybox\ndepends_on:\n  - missing\n")]);
        match validate_dependency_graph(&services) {
            Err(DockerModuleError::UnknownDependency {
                service,
                dependency,
            }) => {
                assert_eq!(service, "app");
                assert_eq!(dependency, "missing");
            }
            other => panic!("expected UnknownDependency, got {:?}", other),
        }
    }

    #[test]
    fn validate_rejects_self_cycle() {
        let services = services(&[("a", "image: busybox\ndepends_on:\n  - a\n")]);
        assert!(matches!(
            validate_dependency_graph(&services),
            Err(DockerModuleError::DependencyCycle(_))
        ));
    }

    #[test]
    fn validate_rejects_direct_cycle() {
        let services = services(&[
            ("a", "image: busybox\ndepends_on:\n  - b\n"),
            ("b", "image: busybox\ndepends_on:\n  - a\n"),
        ]);
        assert!(matches!(
            validate_dependency_graph(&services),
            Err(DockerModuleError::DependencyCycle(_))
        ));
    }

    #[test]
    fn validate_rejects_multi_node_cycle() {
        let services = services(&[
            ("a", "image: busybox\ndepends_on:\n  - b\n"),
            ("b", "image: busybox\ndepends_on:\n  - c\n"),
            ("c", "image: busybox\ndepends_on:\n  - a\n"),
        ]);
        assert!(matches!(
            validate_dependency_graph(&services),
            Err(DockerModuleError::DependencyCycle(_))
        ));
    }

    #[test]
    fn unquoted_restart_no_parses_as_string_not_bool() {
        // serde_yaml follows the YAML 1.2 core schema, so bare `no` stays a
        // string. Guards against the classic compose `restart: no` gotcha.
        let service = service_from_yaml("image: busybox\nrestart: no\n");
        assert_eq!(service.restart.as_deref(), Some("no"));
    }

    #[test]
    fn restart_policy_defaults_to_no_when_absent() {
        let policy = resolve_restart_policy(None).expect("default policy");
        assert_eq!(policy.name, Some(RestartPolicyNameEnum::NO));
        assert_eq!(policy.maximum_retry_count, None);
    }

    #[test]
    fn restart_policy_maps_named_policies() {
        let cases = [
            ("no", RestartPolicyNameEnum::NO),
            ("always", RestartPolicyNameEnum::ALWAYS),
            ("unless-stopped", RestartPolicyNameEnum::UNLESS_STOPPED),
            ("on-failure", RestartPolicyNameEnum::ON_FAILURE),
        ];
        for (input, expected) in cases {
            let policy = resolve_restart_policy(Some(input)).expect("valid policy");
            assert_eq!(policy.name, Some(expected), "for input [{}]", input);
            assert_eq!(policy.maximum_retry_count, None, "for input [{}]", input);
        }
    }

    #[test]
    fn restart_policy_reads_on_failure_retry_count() {
        let policy = resolve_restart_policy(Some("on-failure:5")).expect("valid policy");
        assert_eq!(policy.name, Some(RestartPolicyNameEnum::ON_FAILURE));
        assert_eq!(policy.maximum_retry_count, Some(5));
    }

    #[test]
    fn restart_policy_rejects_invalid_values() {
        for input in ["sometimes", "on-failure:abc", "on-failure:-1", ""] {
            assert!(
                matches!(
                    resolve_restart_policy(Some(input)),
                    Err(DockerModuleError::InvalidRestartPolicy(_))
                ),
                "expected [{}] to be rejected",
                input
            );
        }
    }

    #[test]
    fn command_absent_yields_none() {
        assert_eq!(resolve_command("app", None).expect("no command"), None);
    }

    #[test]
    fn command_splits_shell_string_into_argv() {
        let argv = resolve_command("app", Some(r#"sh -c "echo hi""#)).expect("valid command");
        assert_eq!(argv, Some(vec!["sh".into(), "-c".into(), "echo hi".into()]));
    }

    #[test]
    fn empty_command_yields_empty_argv() {
        // An explicit `command: ""` is a valid parse, not a malformed one, and
        // must stay distinguishable from the rejected cases below.
        assert_eq!(
            resolve_command("app", Some("")).expect("valid command"),
            Some(Vec::new())
        );
    }

    #[test]
    fn command_rejects_unterminated_quote() {
        // Without this the container silently runs the image's default command.
        assert!(matches!(
            resolve_command("init", Some(r#"sh -c "chown -R 8443:8443 /data"#)),
            Err(DockerModuleError::InvalidCommand { .. })
        ));
    }

    #[test]
    fn command_rejects_trailing_backslash() {
        assert!(matches!(
            resolve_command("app", Some(r"echo hi \")),
            Err(DockerModuleError::InvalidCommand { .. })
        ));
    }

    #[test]
    fn environment_absent_yields_empty() {
        assert!(env_of("image: busybox\n", &variables(&[])).is_empty());
    }

    #[test]
    fn environment_list_form_passes_values_through() {
        let env = env_of(
            "image: busybox\nenvironment:\n  - POSTGRES_USER=nino\n  - POSTGRES_PASSWORD=nino\n",
            &variables(&[]),
        );
        assert_eq!(env, vec!["POSTGRES_PASSWORD=nino", "POSTGRES_USER=nino"]);
    }

    #[test]
    fn environment_list_form_keeps_explicit_empty_value() {
        // `KEY=` is the author asking for an empty value, not for the key to be
        // dropped the way a bare `KEY` is.
        let env = env_of(
            "image: busybox\nenvironment:\n  - EMPTY=\n",
            &variables(&[]),
        );
        assert_eq!(env, vec!["EMPTY="]);
    }

    #[test]
    fn environment_map_form_reads_key_value_pairs() {
        let env = env_of(
            "image: busybox\n\
             environment:\n\
             \x20 POSTGRES_USER: nino\n\
             \x20 POSTGRES_PASSWORD: nino\n",
            &variables(&[]),
        );
        assert_eq!(env, vec!["POSTGRES_PASSWORD=nino", "POSTGRES_USER=nino"]);
    }

    #[test]
    fn environment_map_form_coerces_numbers_and_bools() {
        // serde will not coerce a YAML integer or bool into a String, so a
        // `HashMap<String, String>` would fail the parse outright on these.
        let env = env_of(
            "image: busybox\n\
             environment:\n\
             \x20 REPLICAS: 3\n\
             \x20 DEBUG: true\n\
             \x20 RATIO: 1.5\n",
            &variables(&[]),
        );
        assert_eq!(env, vec!["DEBUG=true", "RATIO=1.5", "REPLICAS=3"]);
    }

    #[test]
    fn environment_map_form_keeps_unquoted_no_as_string() {
        // Same YAML 1.2 core-schema trap as `restart: no` — `no` stays a
        // string, so it must not arrive at the container as `false`.
        let env = env_of(
            "image: busybox\nenvironment:\n  ANSWER: no\n",
            &variables(&[]),
        );
        assert_eq!(env, vec!["ANSWER=no"]);
    }

    #[test]
    fn environment_inherits_value_from_host() {
        // Both spellings of "pass this through from the host": a bare list
        // entry and a mapping key with a null value.
        let host = variables(&[("HOME_FROM_HOST", "/home/nino")]);
        assert_eq!(
            env_of("image: busybox\nenvironment:\n  - HOME_FROM_HOST\n", &host),
            vec!["HOME_FROM_HOST=/home/nino"]
        );
        assert_eq!(
            env_of("image: busybox\nenvironment:\n  HOME_FROM_HOST:\n", &host),
            vec!["HOME_FROM_HOST=/home/nino"]
        );
    }

    #[test]
    fn environment_drops_inherited_key_when_host_is_unset() {
        // Compose drops it: a bare key asks for a value if present, unlike
        // `${VAR}`, which declares it required.
        assert!(
            env_of(
                "image: busybox\nenvironment:\n  - ABSENT\n",
                &variables(&[])
            )
            .is_empty()
        );
        assert!(env_of("image: busybox\nenvironment:\n  ABSENT:\n", &variables(&[])).is_empty());
    }

    #[test]
    fn environment_last_duplicate_key_wins() {
        // Docker resolves a duplicated key to the last occurrence; collapsing
        // it here keeps the container and the compose file in agreement.
        let env = env_of(
            "image: busybox\nenvironment:\n  - LEVEL=info\n  - LEVEL=debug\n",
            &variables(&[]),
        );
        assert_eq!(env, vec!["LEVEL=debug"]);
    }

    #[test]
    fn env_file_content_skips_comments_and_blank_lines() {
        let entries = parse_env_file_content(
            "# a comment\n\nPOSTGRES_USER=nino\n   \n  # indented comment\nDEBUG=true\n",
        )
        .expect("valid env file");
        assert_eq!(
            entries,
            vec![
                ("POSTGRES_USER".to_string(), "nino".to_string()),
                ("DEBUG".to_string(), "true".to_string()),
            ]
        );
    }

    #[test]
    fn env_file_content_splits_on_the_first_equals() {
        // A connection string carries its own `=`; splitting on the last one
        // (or on every one) would truncate it.
        let entries = parse_env_file_content("DSN=host=db user=nino\n").expect("valid env file");
        assert_eq!(
            entries,
            vec![("DSN".to_string(), "host=db user=nino".to_string())]
        );
    }

    #[test]
    fn env_file_content_strips_matching_quotes() {
        let entries =
            parse_env_file_content("DOUBLE=\"a b\"\nSINGLE='c d'\nBARE=e f\n").expect("valid");
        assert_eq!(
            entries,
            vec![
                ("DOUBLE".to_string(), "a b".to_string()),
                ("SINGLE".to_string(), "c d".to_string()),
                ("BARE".to_string(), "e f".to_string()),
            ]
        );
    }

    #[test]
    fn env_file_content_keeps_unbalanced_quote_verbatim() {
        // Only a matching pair is a quoting construct; a lone quote is part of
        // the value.
        let entries = parse_env_file_content("ODD=\"unclosed\n").expect("valid env file");
        assert_eq!(entries, vec![("ODD".to_string(), "\"unclosed".to_string())]);
    }

    #[test]
    fn env_file_content_rejects_line_without_equals() {
        assert!(parse_env_file_content("POSTGRES_USER nino\n").is_err());
    }

    #[test]
    fn environment_overrides_env_file() {
        let env = env_with_files(
            "override",
            "image: busybox\nenv_file: db.env\nenvironment:\n  - LEVEL=debug\n",
            &[("db.env", "LEVEL=info\nKEEP=yes\n")],
        );
        assert_eq!(env, vec!["KEEP=yes", "LEVEL=debug"]);
    }

    #[test]
    fn later_env_file_overrides_earlier() {
        let env = env_with_files(
            "ordering",
            "image: busybox\nenv_file:\n  - base.env\n  - local.env\n",
            &[
                ("base.env", "LEVEL=info\nKEEP=yes\n"),
                ("local.env", "LEVEL=debug\n"),
            ],
        );
        assert_eq!(env, vec!["KEEP=yes", "LEVEL=debug"]);
    }

    #[test]
    fn env_file_missing_file_reports_the_path() {
        let service = service_from_yaml("image: busybox\nenv_file: ./absent.env\n");
        match resolve_environment(
            "app",
            service.environment,
            service.env_file,
            &variables(&[]),
            Path::new("/nonexistent-compose-dir"),
        ) {
            Err(DockerModuleError::InvalidEnvironment { service, detail }) => {
                assert_eq!(service, "app");
                assert!(
                    detail.contains("/nonexistent-compose-dir/absent.env"),
                    "expected the resolved path in [{}]",
                    detail
                );
            }
            other => panic!("expected InvalidEnvironment, got {:?}", other.is_ok()),
        }
    }

    #[test]
    fn env_file_accepts_single_and_list_forms() {
        // Both spellings must deserialize; the read itself is exercised by the
        // missing-file case above.
        assert!(matches!(
            service_from_yaml("image: busybox\nenv_file: ./one.env\n").env_file,
            Some(EnvFile::Single(_))
        ));
        assert!(matches!(
            service_from_yaml("image: busybox\nenv_file:\n  - ./one.env\n  - ./two.env\n")
                .env_file,
            Some(EnvFile::Many(paths)) if paths.len() == 2
        ));
    }

    #[test]
    fn environment_map_form_rejects_non_scalar_value() {
        let service =
            service_from_yaml("image: busybox\nenvironment:\n  NESTED:\n    - one\n    - two\n");
        assert!(matches!(
            resolve_environment(
                "app",
                service.environment,
                service.env_file,
                &variables(&[]),
                Path::new("/containers"),
            ),
            Err(DockerModuleError::InvalidEnvironment { .. })
        ));
    }
}
