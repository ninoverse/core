use crate::{debug, error};

use crate::configuration::Configuration;

use sqlx::{
    Pool, Postgres,
    migrate::{MigrateDatabase, Migrator},
    postgres::PgPoolOptions,
};

pub async fn init_db(app_configuration: &Configuration) -> Result<Pool<Postgres>, sqlx::Error> {
    let mut pool: Result<Pool<Postgres>, sqlx::Error> = Err(sqlx::Error::PoolClosed);
    for _ in 0..5 {
        debug!(["DB_INIT"], "Trying to connect to the database...");
        pool = get_pool(&app_configuration).await;
        if pool.is_ok() {
            debug!(["DB_INIT"], "Connected successfully to the database");
            break;
        }
    }
    if let Ok(pool) = pool {
        debug!(
            ["DB_INIT"],
            "Running default migrations to initialize core database"
        );
        migrate_db(&pool)
            .await
            .expect("DB_INIT: Error while running default migrations");
        debug!(["DB_INIT"], "Default migrations run successfully");
        Ok(pool)
    } else {
        pool
    }
}

async fn get_pool(app_configuration: &Configuration) -> Result<Pool<Postgres>, sqlx::Error> {
    let connection_string = format!(
        "postgres://{}:{}@{}:{}/{}",
        app_configuration.database.user,
        app_configuration.database.password,
        app_configuration.database.host,
        app_configuration.database.port,
        app_configuration.database.name
    );
    match Postgres::database_exists(&connection_string).await {
        Ok(postgres_database_exists) => {
            if !postgres_database_exists {
                debug!(["DB_INIT"], "Database does not exists.");
                Postgres::create_database(&connection_string).await?;
                debug!(["DB_INIT"], "Database created.");
            } else {
                debug!(["DB_INIT"], "Database exists.");
            }
        }
        Err(postgres_error) => {
            error!(["DB_INIT"], "Failed to init db: {}", postgres_error);
            std::process::exit(1);
        }
    };
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(connection_string.as_str())
        .await?;
    Ok(pool)
}

async fn migrate_db(pool: &Pool<Postgres>) -> Result<(), sqlx::Error> {
    let migrator = Migrator::new(std::path::Path::new("./sql")).await?;
    migrator.run(pool).await?;
    Ok(())
}
