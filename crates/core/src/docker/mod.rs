use bollard::{
    Docker, models::{Mount, MountBindOptions, MountType, NetworkCreateRequest, VolumeCreateRequest}, plugin::{ContainerCreateBody, EndpointSettings, HostConfig, NetworkingConfig, PortBinding}, query_parameters::{
        CreateContainerOptions, CreateImageOptions, InspectContainerOptions, RemoveContainerOptionsBuilder, StartContainerOptions, StopContainerOptions,
    },
};
use futures::StreamExt;
use serde::Deserialize;
use serde_yaml;
use std::sync::Arc;
use std::{collections::HashMap, env, fs, path::{Component, Path, PathBuf}};
use thiserror::Error;
use tokio::{
   sync::{Barrier, broadcast::Sender}, task::JoinSet, time::{Duration, sleep},
};

use crate::{error, info, warn};

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
    pub environment: Option<Vec<String>>,
    pub container_name: Option<String>,
    pub command: Option<Vec<String>>,
    pub user: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum VolumeSpec {
    Short(String),
    Long(LongVolumeSpec),
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

    #[error("Failed to read the file")]
    Read(#[from] std::io::Error),

    #[error("Failed to parse the yaml content")]
    Parse(#[from] serde_yaml::Error),

    #[error("Bollard(docker) error")]
    Bollard(#[from] bollard::errors::Error),


    #[error("Invalid volume specification: {0}")]
    InvalidVolumeSpec(String),
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
                                let unique_name = format!("{}", raw_name);
                                if let Some(ref mut nets) = config.networks {
                                    for net in nets.iter_mut() {
                                        *net = format!("{}", net);
                                    }
                                }
                                if let Some(volumes) = config.volumes.take() {
                                    for volume_spec in volumes {
                                        config
                                            .mounts
                                            .push(resolve_volume_spec(volume_spec, compose_dir)?);
                                    }
                                }
                                definitions.services.push((unique_name, config));
                            }
                        }

                        if let Some(networks) = compose_config.networks {
                            for (raw_name, config) in networks {
                                let unique_name = format!("{}", raw_name);
                                definitions.networks.push((unique_name, config));
                            }
                        }

                        if let Some(volumes) = compose_config.volumes {
                            for (raw_name, config) in volumes {
                                let unique_name = format!("{}", raw_name);
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
        let volume_name = Some(docker_volume.0);

        let volume_config = VolumeCreateRequest {
            name: volume_name.clone(),
            driver: docker_volume.1.unwrap_or_default().driver,
            ..Default::default()
        };

        match docker.create_volume(volume_config).await {
            Ok(_) => info!(
                ["DOCKER_INIT"],
                "Volume [{}] created successfully.",
                &volume_name.unwrap_or(String::from("unnamed volume"))
            ),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code,
                message,
            }) if status_code == 409 => {
                warn!(
                    ["DOCKER_INIT"],
                    "Warning: Volume [{}] exists but has a config conflict: {}, proceeding anyway...",
                    &volume_name.unwrap_or(String::from("unnamed volume")),
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
    match docker.stop_container(service_name, Some(stop_options)).await {
        Ok(_) => {
            info!(["DOCKER_SHUTDOWN"], "Container '{}' stopped gracefully.", service_name);
            if remove_containers_on_shutdown {
                let remove_container_options = RemoveContainerOptionsBuilder::default()
                    .force(true)
                    .build();
                match docker.remove_container(service_name, Some(remove_container_options)).await {
                    Ok(_) => info!(["DOCKER_SHUTDOWN"], "Container '{}' removed.", service_name),
                    Err(remove_container_error) => error!(["DOCKER_SHUTDOWN"], "Failed to remove '{}': {}", service_name, remove_container_error),
                };
            } else {
                info!(["DOCKER_SHUTDOWN"], "Container '{}' left in place (remove_containers_on_shutdown=false).", service_name);
            }
        },
        Err(stop_container_error) => error!(["DOCKER_SHUTDOWN"], "Failed to stop '{}': {}", service_name, stop_container_error),
    }
}

pub async fn start_docker_container(
    docker_services: Vec<(String, ServiceConfig)>,
    docker: &Docker,
    shutdown_broadcast_sender: & Sender<()>,
    join_set: &mut JoinSet<()>,
    remove_containers_on_shutdown: bool,
) -> Result<(), DockerModuleError> {
    let barrier = Arc::new(Barrier::new(docker_services.len() + 1));
    let mut startup_shutdown_receiver = shutdown_broadcast_sender.subscribe();

    for (service_name, service_config) in docker_services {
        let barrier_cloned = barrier.clone();
        let docker_cloned = docker.clone();
        let mut shutdown_broadcast_sender_subscribed = shutdown_broadcast_sender.subscribe();

        join_set.spawn(async move {
            tokio::select! {
                _ = boot_service(&docker_cloned, &service_name, service_config) => {}
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

            loop {
                tokio::select! {
                    _ = sleep(std::time::Duration::from_secs(5)) => {

                        let inspect = docker_cloned
                            .inspect_container(&service_name, None::<InspectContainerOptions>)
                            .await;

                        match inspect {
                            Ok(details) => {
                                let is_running = details
                                    .state
                                    .and_then(|container| container.running)
                                    .unwrap_or(false);
                                if !is_running {
                                    warn!(
                                        ["DOCKER_INIT"],
                                        "Crash detected for [{}]! Restarting...", service_name
                                    );
                                    let _ = docker_cloned
                                        .start_container(&service_name, None::<StartContainerOptions>)
                                        .await;
                                }
                            }
                            Err(docker_inspect_error) => {
                                error!(
                                    ["DOCKER_INIT"],
                                    "Error inspecting [{}]: {}. check if docker daemon is running",
                                    service_name,
                                    docker_inspect_error
                                );
                            }
                        }
                    }

                    _ = shutdown_broadcast_sender_subscribed.recv() => {
                        info!(["DOCKER_SHUTDOWN"], "Shutdown signal received by task '{}'. Stopping container...", service_name);

                        stop_and_cleanup_container(
                            &docker_cloned,
                            &service_name,
                            remove_containers_on_shutdown,
                        )
                        .await;

                        break;
                    }
                }

            }
        });
    }

    tokio::select! {
        _ = barrier.wait() => {
            info!(["DOCKER_INIT"], "All Docker definitions are up");
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

async fn boot_service(docker: &Docker, service_name: &str, service_config: ServiceConfig) {
    info!(
        ["DOCKER_INIT"],
        "Booting up service: {} (Image: {:?})", service_name, service_config.image
    );

    let mut host_config = HostConfig::default();

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
            if parts.len() == 2 {
                let host_port = parts[0].to_string();
                let container_port = format!("{}/tcp", parts[1]);

                port_bindings.insert(
                    container_port,
                    Some(vec![PortBinding {
                        host_ip: Some("0.0.0.0".to_string()),
                        host_port: Some(host_port),
                    }]),
                );
            }
        }
        host_config.port_bindings = Some(port_bindings);
    }

    let mut network_endpoints = HashMap::new();
    if let Some(networks) = &service_config.networks {
        for net in networks {
            network_endpoints.insert(net.clone(), EndpointSettings::default());
        }
    }

    let environment = service_config.environment.unwrap_or_default();

    let _container_name = service_config.container_name.unwrap_or_default();

    let command = service_config.command.unwrap_or_default();

    let user = service_config.user.unwrap_or_default();

    let container_configuration = ContainerCreateBody {
        image: service_config.image.clone(),
        host_config: Some(host_config),
        networking_config: Some(NetworkingConfig {
            endpoints_config: Some(network_endpoints),
        }),
        env: Some(environment),
        cmd: Some(command),
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
            "Checking image [{}{}] for service [{}]...", image_name,if let Some(image_tag) = &image_tag {
                format!(":{:}", image_tag)
            } else {
                String::new()
            }  , service_name                 
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
                Err(e) => {
                    panic!("Fatal error pulling image [{}]: {}", image_name, e);
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
        Err(bollard::errors::Error::DockerResponseServerError { status_code, .. })
            if status_code == 409 =>
        {
            warn!(
                ["DOCKER_INIT"],
                "Warning: Container [{}] exists, proceeding anyway...", &service_name
            );
        }
        Err(volume_creation_error) => {
            panic!(
                "Failed to create container [{}]: {}",
                service_name, volume_creation_error
            );
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
                "Container [{}] started successfully, waiting for healthy state.",
                &service_name
            );
            loop {
                let inspect = docker
                    .inspect_container(service_name, None::<InspectContainerOptions>)
                    .await
                    .unwrap();
                if let Some(state) = inspect.state {
                    if state.running.unwrap_or(false) {
                        break;
                    }
                }
                sleep(Duration::from_secs(1)).await;
            }
        }
        Err(bollard::errors::Error::DockerResponseServerError { status_code, .. })
            if status_code == 409 =>
        {
            warn!(
                ["DOCKER_INIT"],
                "Warning: Container [{}] exists, proceeding anyway...", &service_name
            );
        }
        Err(volume_creation_error) => {
            panic!(
                "Failed to start container [{}]: {}",
                service_name, volume_creation_error
            );
        }
    }
}
