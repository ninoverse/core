mod db_handler;
mod docker;
mod error;
mod kafka_handler;
mod web_server;

use std::path::Path;
use std::sync::Arc;

use sqlx::{Pool, Postgres};

use tokio::{
    sync::{broadcast, mpsc::channel},
    task::JoinSet,
};

use configuration::NinoverseCoreConfiguration;

use web_server::init_request_handler;

use kafka_handler::init_kafka;

use docker::{
    create_docker_client, create_docker_networks, create_docker_volumes, find_docker_definitions,
    start_docker_container,
};

use error::{CoreError, CoreResult};

use logger::{error, info};

const CONFIG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/config/default");

#[tokio::main]
async fn main() -> CoreResult<()> {
    logger::init();
    info!(["MAIN"], "Program started");
    info!(["MAIN"], "Loading configuration");
    let app_configuration = NinoverseCoreConfiguration::build(Path::new(CONFIG))
        .map_err(|configuration_error| CoreError::Configuration(configuration_error.to_string()))?;
    info!(["MAIN"], "Configuration loaded: \n{:#?}", app_configuration);

    let (shutdown_broadcast_sender, mut shutdown_receiver) = broadcast::channel::<()>(1);
    let mut join_set = JoinSet::<()>::new();

    spawn_signal_handler(shutdown_broadcast_sender.clone());

    // The shutdown broadcast and the container drain below have to run whatever
    // startup did, so the outcome is captured rather than propagated with `?`.
    let startup_result = startup(
        app_configuration,
        &shutdown_broadcast_sender,
        &mut join_set,
        &mut shutdown_receiver,
    )
    .await;

    if let Err(startup_error) = &startup_result {
        error!(["MAIN"], "Startup failed: {}", startup_error);
    }

    let _ = shutdown_broadcast_sender.send(());

    info!(["MAIN"], "Waiting for containers to stop...");
    while let Some(res) = join_set.join_next().await {
        if let Err(e) = res {
            error!(["MAIN"], "A spawned task panicked during shutdown: {}", e);
        }
    }

    startup_result
}

/// The first Ctrl+C asks for a graceful shutdown; a second one forces the exit.
/// Installing `tokio::signal::ctrl_c` replaces the default SIGINT disposition,
/// so without this second stage a stuck task would leave the process unkillable.
fn spawn_signal_handler(shutdown_broadcast_sender: broadcast::Sender<()>) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!(["MAIN"], "\nCtrl+C detected! Initiating shutdown...");
            let _ = shutdown_broadcast_sender.send(());
        }

        if tokio::signal::ctrl_c().await.is_ok() {
            // Straight to stderr: the logger drains on a background thread with
            // no flush, so anything queued here would be lost on exit.
            eprintln!("Second Ctrl+C: forcing exit.");
            std::process::exit(130);
        }
    });
}

async fn startup(
    app_configuration: NinoverseCoreConfiguration,
    shutdown_broadcast_sender: &broadcast::Sender<()>,
    join_set: &mut JoinSet<()>,
    shutdown_receiver: &mut broadcast::Receiver<()>,
) -> CoreResult<()> {
    info!(["MAIN"], "Creating docker client");
    let docker = create_docker_client().await?;
    info!(["MAIN"], "Searching docker containers definitions");
    let docker_definitions = find_docker_definitions().await?;
    info!(["MAIN"], "Creating docker network");
    create_docker_networks(docker_definitions.networks, &docker).await?;
    info!(["MAIN"], "Creating docker volumes");
    create_docker_volumes(docker_definitions.volumes, &docker).await?;
    info!(["MAIN"], "Starting docker containers");
    start_docker_container(
        docker_definitions.services,
        &docker,
        shutdown_broadcast_sender,
        join_set,
        app_configuration.docker.remove_containers_on_shutdown,
    )
    .await?;

    info!(["MAIN"], "Initializing DB pool");
    let db_pool_result = tokio::select! {
        _ = shutdown_receiver.recv() => None,
        db_pool_result = db_handler::init_db(&app_configuration) => Some(db_pool_result),
    };

    let pool = match db_pool_result {
        None => {
            info!(
                ["MAIN"],
                "Shutdown requested during startup, skipping run phase."
            );
            return Ok(());
        }
        Some(db_pool_result) => db_pool_result?,
    };

    info!(["MAIN"], "Starting threads");
    run_threads(pool, app_configuration, shutdown_broadcast_sender).await
}

async fn run_threads(
    pool: Pool<Postgres>,
    app_configuration: NinoverseCoreConfiguration,
    shutdown_broadcast_sender: &broadcast::Sender<()>,
) -> CoreResult<()> {
    // Creating the mpsc thread message sender(multiple) and receiver(single).
    // The sender stays alive for this whole scope, which keeps the producer's
    // receive loop open until shutdown.
    let (kafka_thread_sender, kafka_thread_receiver) = channel(100);

    let app_configuration = Arc::new(app_configuration);

    // Bind before spawning so a bind failure aborts startup instead of panicking
    // inside a task.
    let server = init_request_handler(&pool, &app_configuration, &kafka_thread_sender)?;
    let server_handle = server.handle();

    // actix is built with `.disable_signals()`, so it is stopped from here.
    let mut server_shutdown_receiver = shutdown_broadcast_sender.subscribe();
    tokio::spawn(async move {
        let _ = server_shutdown_receiver.recv().await;
        info!(["RUN_THREADS"], "Stopping API listener.");
        server_handle.stop(true).await;
    });

    let mut run_tasks = JoinSet::<CoreResult<()>>::new();

    info!(["RUN_THREADS"], "Starting API listener thread.");
    run_tasks.spawn(async move { server.await.map_err(CoreError::from) });

    info!(["RUN_THREADS"], "Starting KAFKA thread.");
    let app_configuration_cloned = Arc::clone(&app_configuration);
    let kafka_shutdown_broadcast_sender = shutdown_broadcast_sender.clone();
    run_tasks.spawn(async move {
        init_kafka(
            app_configuration_cloned,
            kafka_thread_receiver,
            &kafka_shutdown_broadcast_sender,
        )
        .await
    });

    info!(["MAIN"], "Orchestrator running, press Ctrl+C to stop.");

    // The first task to finish ends the run phase; the rest are told to wind down
    // so a dying Kafka task cannot leave the server running on its own.
    let mut first_error = None;
    if let Some(join_result) = run_tasks.join_next().await {
        first_error = flatten_task_result(join_result).err();
    }
    let _ = shutdown_broadcast_sender.send(());

    while let Some(join_result) = run_tasks.join_next().await {
        if let Err(task_error) = flatten_task_result(join_result) {
            error!(["RUN_THREADS"], "Task terminated: {}", task_error);
            first_error.get_or_insert(task_error);
        }
    }

    match first_error {
        Some(first_error) => Err(first_error),
        None => Ok(()),
    }
}

fn flatten_task_result(
    join_result: Result<CoreResult<()>, tokio::task::JoinError>,
) -> CoreResult<()> {
    join_result?
}
