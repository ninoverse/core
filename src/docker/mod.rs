use bollard::{
    Docker, models::{NetworkCreateRequest, VolumeCreateRequest}, plugin::{ContainerCreateBody, EndpointSettings, HostConfig, NetworkingConfig, PortBinding}, query_parameters::{
        CreateContainerOptions, CreateImageOptions, InspectContainerOptions, RemoveContainerOptionsBuilder, StartContainerOptions, StopContainerOptions,
    },
};
use futures::StreamExt;
use serde::Deserialize;
use serde_yaml;
use std::sync::Arc;
use std::{collections::HashMap, fs, path::Path};
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
    pub volumes: Option<Vec<String>>,
    pub environment: Option<Vec<String>>,
    pub container_name: Option<String>,
    pub command: Option<Vec<String>>
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
}

pub async fn find_docker_definitions() -> Result<DockerDefinitions, DockerModuleError> {
    let compose_file_dir = Path::new("containers");
    let mut definitions = DockerDefinitions::default();

    if compose_file_dir.exists() && compose_file_dir.is_dir() {
        let compose_files = fs::read_dir(compose_file_dir)?;

        for file in compose_files {
            let path = file?.path();
            if path.is_file() {
                if let Some(extension) = path.extension() {
                    if extension == "yml" || extension == "yaml" {
                        let yaml_content = fs::read_to_string(&path)?;

                        let compose_config: ComposeFile = serde_yaml::from_str(&yaml_content)?;

                        if let Some(services) = compose_config.services {
                            for (raw_name, mut config) in services {
                                let unique_name = format!("{}", raw_name);
                                if let Some(ref mut nets) = config.networks {
                                    for net in nets.iter_mut() {
                                        *net = format!("{}", net);
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

pub async fn start_docker_container(
    docker_services: Vec<(String, ServiceConfig)>,
    docker: &Docker,
    shutdown_broadcast_sender: & Sender<()>,
    join_set: &mut JoinSet<()>
) -> Result<(), DockerModuleError> {
    let barrier = Arc::new(Barrier::new(docker_services.len() + 1));

    for (service_name, service_config) in docker_services {
        let barrier_cloned = barrier.clone();
        let docker_cloned = docker.clone();
        let mut shutdown_broadcast_sender_subscribed = shutdown_broadcast_sender.subscribe();

        join_set.spawn(async move {
            info!(
                ["DOCKER_INIT"],
                "Booting up service: {} (Image: {:?})", service_name, service_config.image
            );

            let mut host_config = HostConfig::default();

            if let Some(volumes) = &service_config.volumes {
                host_config.binds = Some(volumes.clone());
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

            let container_configuration = ContainerCreateBody {
                image: service_config.image.clone(),
                host_config: Some(host_config),
                networking_config: Some(NetworkingConfig {
                    endpoints_config: Some(network_endpoints),
                }),
                env: Some(environment),
                cmd: Some(command),
                ..Default::default()
            };

            let create_options = CreateContainerOptions {
                name: Some(service_name.clone()),
                platform: String::new(),
            };

            if let Some(image_name) = &service_config.image {
                info!(
                    ["DOCKER_IMAGES"],
                    "Checking image [{}] for service [{}]...", image_name, service_name
                );

                let pull_options = CreateImageOptions {
                    from_image: Some(image_name.clone()),
                    ..Default::default()
                };

                let mut image_stream = docker_cloned.create_image(Some(pull_options), None, None);

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

            match docker_cloned
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
            match docker_cloned
                .start_container(&service_name, None::<StartContainerOptions>)
                .await
            {
                Ok(_) => {
                    info!(
                        ["DOCKER_INIT"],
                        "Container [{}] started successfully, waiting for healthy state.",
                        &service_name
                    );
                    loop {
                        let inspect = docker_cloned
                            .inspect_container(&service_name, None::<InspectContainerOptions>)
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

            info!(
                ["DOCKER_INIT"],
                "Service [{}] is started and healthy. Signaling barrier...", service_name
            );

            barrier_cloned.wait().await;

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
                        
                        let stop_options = StopContainerOptions { signal: Some("SIGTERM".to_string()), t: Some(10) };
                        match docker_cloned.stop_container(&service_name, Some(stop_options)).await {
                            Ok(_) => {
                                info!(["DOCKER_SHUTDOWN"], "Container '{}' stopped gracefully.", &service_name);
                                let remove_container_options = RemoveContainerOptionsBuilder::default()
                                    .force(true)
                                    .build();
                                match docker_cloned.remove_container(&service_name, Some(remove_container_options)).await {
                                    Ok(_) => info!(["DOCKER_SHUTDOWN"], "Container '{}' removed.", service_name),
                                    Err(remove_container_error) => error!(["DOCKER_SHUTDOWN"], "Failed to remove '{}': {}", service_name, remove_container_error),
                                };
                            },
                            Err(stop_container_error) => error!(["DOCKER_SHUTDOWN"], "Failed to stop '{}': {}", service_name, stop_container_error),
                        }
                        
                        break;
                    }
                }
                
            }
        });
    }

    barrier.wait().await;
    info!(["DOCKER_INIT"], "All Docker definitions are up");
    Ok(())
}
