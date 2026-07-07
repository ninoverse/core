use std::fmt;

use config::{Config, ConfigError, Environment, File};
use serde::{Deserialize, Deserializer};

#[derive(Deserialize)]
pub struct PostgresDatabaseConfiguration {
    pub host: String,
    pub port: u16,
    pub name: String,
    pub user: String,
    pub password: String,
    pub pool_size: u32,
}

impl fmt::Debug for PostgresDatabaseConfiguration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PostgresDatabaseConfiguration")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("name", &self.name)
            .field("user", &self.user)
            .field("password", &"***")
            .field("pool_size", &self.pool_size)
            .finish()
    }
}

#[derive(Debug, Deserialize)]
pub struct KafkaBrokerConfiguration {
    pub broker: String,
    #[serde(deserialize_with = "deserialize_kafka_topics")]
    pub topics: Vec<KafkaTopic>,
    pub group_id: String,
}

#[derive(Debug, Deserialize)]
pub struct KafkaTopic {
    pub name: String,
    pub num_partition: i32,
    #[allow(dead_code)]
    pub offset: u64,
}

#[derive(Debug, Deserialize)]
pub struct Configuration {
    #[allow(dead_code)]
    pub host: String,
    pub port: u16,
    pub database: PostgresDatabaseConfiguration,
    pub kafka: KafkaBrokerConfiguration,
}

impl Configuration {
    pub fn build() -> Result<Self, ConfigError> {
        let builder = Config::builder()
            .add_source(File::with_name("config/default").required(false))
            .add_source(
                Environment::with_prefix("APP")
                    .try_parsing(true)
                    .separator("__")
                    .list_separator(",")
                    .with_list_parse_key("kafka.topics"),
            );

        builder.build()?.try_deserialize()
    }
}

fn parse_topic<E>(topic_string: &str) -> Result<KafkaTopic, E>
where
    E: serde::de::Error,
{
    let mut parts = topic_string.splitn(3, ':');
    let (name, num_partition, offset) = match (parts.next(), parts.next(), parts.next()) {
        (Some(name), Some(num_partition), Some(offset)) => {
            (name.trim(), num_partition.trim(), offset.trim())
        }
        _ => {
            return Err(E::custom(format!(
                "invalid topic '{topic_string}': expected format 'name:num_partition:offset'"
            )));
        }
    };

    if name.is_empty() {
        return Err(E::custom(format!(
            "invalid topic '{topic_string}': empty topic name"
        )));
    }

    let num_partition = num_partition.parse().map_err(|e| {
        E::custom(format!(
            "invalid num_partition '{num_partition}' in topic '{topic_string}': {e}"
        ))
    })?;

    let offset = offset.parse().map_err(|e| {
        E::custom(format!(
            "invalid offset '{offset}' in topic '{topic_string}': {e}"
        ))
    })?;

    Ok(KafkaTopic {
        name: name.to_string(),
        num_partition,
        offset,
    })
}

fn deserialize_kafka_topics<'de, D>(deserializer: D) -> Result<Vec<KafkaTopic>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw_list: Vec<String> = Vec::deserialize(deserializer)?;
    raw_list
        .iter()
        .map(|topic_string| parse_topic::<D::Error>(topic_string))
        .collect()
}
