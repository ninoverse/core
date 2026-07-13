mod configuration;
mod db_handler;
mod docker;
mod kafka_handler;
mod logger;
mod web_server;

// use std::sync::Arc;

// use sqlx::{Pool, Postgres};

use tokio::{
    sync::broadcast,
    task::JoinSet,
    time::{Duration, sleep},
};

use configuration::Configuration;

// use web_server::init_request_handler;

// use kafka_handler::init_kafka;

use docker::{
    create_docker_client, create_docker_networks, create_docker_volumes, find_docker_definitions,
    start_docker_container,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    logger::init();
    info!(["MAIN"], "Program started");
    info!(["MAIN"], "Loading configuration");
    let app_configuration = match Configuration::build() {
        Ok(app_configuration) => {
            info!(["MAIN"], "Configuration loaded: \n{:#?}", app_configuration);
            app_configuration
        }
        Err(configuration_error) => {
            error!(
                ["MAIN"],
                "Failed to load configuration: {}", configuration_error
            );
            std::process::exit(1);
        }
    };
    let (shutdown_broadcast_sender, _) = broadcast::channel::<()>(1);
    let mut join_set = JoinSet::<()>::new();
    info!(["MAIN"], "Creating docker client");
    let docker = create_docker_client().await?;
    info!(["MAIN"], "Searching docker containers definitions");
    let docker_definitions = find_docker_definitions().await?;
    // TODO: Could be together (volumes + network)
    info!(["MAIN"], "Creating docker network");
    create_docker_networks(docker_definitions.networks, &docker).await?;
    info!(["MAIN"], "Creating docker volumes");
    create_docker_volumes(docker_definitions.volumes, &docker).await?;
    info!(["MAIN"], "Starting docker containers");
    start_docker_container(
        docker_definitions.services,
        &docker,
        &shutdown_broadcast_sender,
        &mut join_set,
    )
    .await?;
    info!(["MAIN"], "Initializing DB pool");
    let _pool = db_handler::init_db(&app_configuration).await?;
    info!(["MAIN"], "Starting threads");
    // run_threads(pool, app_configuration).await?;
    info!(["MAIN"], "Orchestrator running, press Ctrl+C to stop.");
    tokio::signal::ctrl_c().await?;
    info!(["MAIN"], "\nCtrl+C detected! Initiating shutdown...");
    let _ = shutdown_broadcast_sender.send(());
    while let Some(res) = join_set.join_next().await {
        if let Err(e) = res {
            error!(["MAIN"], "A spawned task panicked during shutdown: {}", e);
        }
    };
    Ok(())
}

// async fn run_threads(
//     pool: Pool<Postgres>,
//     app_configuration: Configuration,
// ) -> Result<(), Box<dyn std::error::Error>> {
//     // Creating the mpsc thread message sender(multiple) and receiver(single)
//     let (kafka_thread_sender, kafka_thread_receiver) = channel(100);

//     // Creating the Arc pointers for shared objects
//     let pool = Arc::new(pool);
//     let app_configuration = Arc::new(app_configuration);
//     let kafka_thread_sender = Arc::new(kafka_thread_sender);

//     // Cloning the Arc pointers to pass them to the thread
//     let app_configuration_cloned = Arc::clone(&app_configuration);
//     let kafka_thread_sender_cloned = Arc::clone(&kafka_thread_sender);

//     // Starting webserver thread
//     let api_listener_thread_handler = tokio::spawn(async move {
//         info!(["RUN_THREADS"], "Starting API listener thread.");
//         let _ = init_request_handler(
//             &pool,
//             &app_configuration_cloned,
//             &kafka_thread_sender_cloned,
//         )
//         .expect("RUN_THREADS: Error in the HTTP Server.")
//         .await;
//     });

//     // Cloning the Arc pointers to pass them to the thread
//     let app_configuration_cloned = Arc::clone(&app_configuration);

//     // Starting kafka thread
//     let kafka_thread_handler = tokio::spawn(async move {
//         info!(["RUN_THREADS"], "Starting KAFKA thread.");
//         init_kafka(app_configuration_cloned, kafka_thread_receiver).await;
//     });

//     // Start the threads
//     if let Err(joined_threads_error) =
//         tokio::try_join!(api_listener_thread_handler, kafka_thread_handler)
//     {
//         error!(
//             ["RUN_THREADS"],
//             "Some error occured in a thread: {}", joined_threads_error
//         )
//     }
//     Ok(())
// }
