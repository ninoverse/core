use thiserror::Error;

use rdkafka::error::{KafkaError as KafkaErrorFromLibrary, RDKafkaErrorCode};

use crate::docker::DockerModuleError;

#[derive(Debug, Error)]
pub enum KafkaError {
    #[error("Kafka library: {0}")]
    RDKafka(#[from] KafkaErrorFromLibrary),

    #[error("Kafka error code: {0}")]
    ErrorCode(#[from] RDKafkaErrorCode),

    #[error("No topic configured")]
    NoTopicInConfiguration,
}

pub type KafkaResult<T> = std::result::Result<T, KafkaError>;

#[derive(Debug, Error)]
pub enum CoreError {
    /// `NinoverseCoreConfiguration::build` returns `Box<dyn Error>`, which is
    /// neither `Send` nor `Sync`, so the message is carried instead.
    #[error("Configuration: {0}")]
    Configuration(String),

    #[error(transparent)]
    Docker(#[from] DockerModuleError),

    #[error("Database: {0}")]
    Database(#[from] sqlx::Error),

    #[error(transparent)]
    Kafka(#[from] KafkaError),

    #[error("HTTP server: {0}")]
    WebServer(#[from] std::io::Error),

    #[error("Task join: {0}")]
    Join(#[from] tokio::task::JoinError),
}

pub type CoreResult<T> = std::result::Result<T, CoreError>;
