use crate::{error, info};

use crate::configuration::Configuration;

use sqlx::{Connection, PgConnection, Executor};
use sqlx::postgres::PgConnectOptions;
use sqlx::{
    Pool, Postgres,
    postgres::PgPoolOptions,
};
// use thiserror::Error;

// #[derive(Error, Debug)]

// pub enum PostgresModuleError {
//     #[error("Sqlx(postgres) error")]
//     SQLXError(#[from] sqlx::Error),
// }

pub async fn init_db(app_configuration: &Configuration) -> Result<Pool<Postgres>, sqlx::Error> {
    let mut last_err = None;
    for attempt in 1..=5 {
        match get_pool(app_configuration).await {
            Ok(pool) => {
                info!(
                    ["DB_INIT"],
                    "Running default migrations to initialize core database"
                );
                migrate_db(&pool).await?;
                info!(["DB_INIT"], "Default migrations run successfully");
                return Ok(pool);
            }
            Err(postgres_get_pool_error) => {
                error!(["DB_INIT"], "Attempt {}/{} failed: {}", attempt, app_configuration.database.connection_attempt, postgres_get_pool_error);
                last_err = Some(postgres_get_pool_error);
                tokio::time::sleep(std::time::Duration::from_secs(attempt * 2)).await;
            }
        }
    }
    Err(last_err.unwrap())
}

async fn get_pool(app_configuration: &Configuration) -> Result<Pool<Postgres>, sqlx::Error> {
    let conection_option = PgConnectOptions::new()
        .host(&app_configuration.database.host)
        .port(app_configuration.database.port)
        .username(&app_configuration.database.user)
        .password(&app_configuration.database.password);

    let mut admin_connection = PgConnection::connect_with(&conection_option.clone().database("postgres")).await?;

    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname = $1)")
        .bind(&app_configuration.database.name)
        .fetch_one(&mut admin_connection)
        .await?;

    if !exists {
        info!(["DB_INIT"], "Database does not exist, creating...");
        let create_stmt = format!(
            "CREATE DATABASE \"{}\"",
            app_configuration.database.name.replace('"', "\"\"")
        );
        admin_connection.execute(create_stmt.as_str()).await?;
        info!(["DB_INIT"], "Database created.");
    }
    admin_connection.close().await?;

    PgPoolOptions::new()
        .max_connections(5)
        .connect_with(conection_option.database(&app_configuration.database.name))
        .await
}

async fn migrate_db(pool: &Pool<Postgres>) -> Result<(), sqlx::Error> {
    sqlx::migrate!("./sql").run(pool).await?;
    Ok(())
}
