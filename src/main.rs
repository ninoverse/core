mod configuration;
mod db_handler;
mod kafka_handler;
mod logger;
mod web_server;

use std::sync::Arc;

use crate::configuration::Configuration;

use web_server::init_request_handler;

use kafka_handler::init_kafka;

use sqlx::{Pool, Postgres};

use tokio::sync::mpsc::channel;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    logger::init();
    debug!(["MAIN"], "Program started");
    debug!(["MAIN"], "Loading configuration");
    let app_configuration = match Configuration::build() {
        Ok(app_configuration) => app_configuration,
        Err(configuration_error) => {
            error!(
                ["MAIN"],
                "Failed to load configuration: {}", configuration_error
            );
            std::process::exit(1);
        }
    };
    start_app(app_configuration).await?;
    debug!(["MAIN"], "Gracefully shutting down the system");
    Ok(())
}

async fn start_app(app_configuration: Configuration) -> Result<(), Box<dyn std::error::Error>> {
    debug!(["MAIN"], "Configuration loaded: \n{:#?}", app_configuration);
    debug!(["MAIN"], "Initializing DB pool");
    let pool = db_handler::init_db(&app_configuration).await?;
    debug!(["MAIN"], "Starting threads");
    run_threads(pool, app_configuration).await?;
    Ok(())
}

async fn run_threads(
    pool: Pool<Postgres>,
    app_configuration: Configuration,
) -> Result<(), Box<dyn std::error::Error>> {
    // Creating the mpsc thread message sender(multiple) and receiver(single)
    let (kafka_thread_sender, kafka_thread_receiver) = channel(100);

    // Creating the Arc pointers for shared objects
    let pool = Arc::new(pool);
    let app_configuration = Arc::new(app_configuration);
    let kafka_thread_sender = Arc::new(kafka_thread_sender);

    // Cloning the Arc pointers to pass them to the thread
    let app_configuration_cloned = Arc::clone(&app_configuration);
    let kafka_thread_sender_cloned = Arc::clone(&kafka_thread_sender);

    // Starting webserver thread
    let api_listener_thread_handler = tokio::spawn(async move {
        debug!(["RUN_THREADS"], "Starting API listener thread.");
        let _ = init_request_handler(
            &pool,
            &app_configuration_cloned,
            &kafka_thread_sender_cloned,
        )
        .expect("RUN_THREADS: Error in the HTTP Server.")
        .await;
    });

    // Cloning the Arc pointers to pass them to the thread
    let app_configuration_cloned = Arc::clone(&app_configuration);

    // Starting kafka thread
    let kafka_thread_handler = tokio::spawn(async move {
        debug!(["RUN_THREADS"], "Starting KAFKA thread.");
        init_kafka(app_configuration_cloned, kafka_thread_receiver).await;
    });

    // Start the threads
    if let Err(joined_threads_error) =
        tokio::try_join!(api_listener_thread_handler, kafka_thread_handler)
    {
        error!(
            ["RUN_THREADS"],
            "Some error occured in a thread: {}", joined_threads_error
        )
    }
    Ok(())
}
