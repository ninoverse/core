use std::sync::Arc;

use chrono::DateTime;

use rdkafka::{
    ClientConfig, Message,
    admin::{AdminClient, AdminOptions, NewTopic, TopicReplication},
    client::ClientContext,
    consumer::{Consumer, StreamConsumer},
    error::KafkaError,
    producer::{FutureProducer, FutureRecord, future_producer::OwnedDeliveryResult},
    types::RDKafkaErrorCode,
};

use futures::{StreamExt, channel::oneshot::Canceled, stream::FuturesUnordered};

use tokio::sync::mpsc::Receiver;

use crate::{configuration::Configuration, debug, error, info, warn};

pub struct KafkaBrokerContext {}

impl ClientContext for KafkaBrokerContext {
    const ENABLE_REFRESH_OAUTH_TOKEN: bool = false;
}

pub struct KafkaChannelMessage {
    pub topic: String,
    pub sender: String,
    pub content: String,
}

async fn log_kafka_message(message: rdkafka::message::OwnedMessage) {
    let key = message
        .key()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default();

    let message_timestamp = message.timestamp().to_millis().unwrap_or_default();
    let timestamp = DateTime::from_timestamp_millis(message_timestamp).unwrap_or_default();

    let payload = message
        .payload()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default();

    debug!(
        ["KAFKA_MESSAGE"],
        "{}:{}/{}/{}/{}: {}",
        message.topic(),
        message.partition(),
        message.offset(),
        key.as_str(),
        timestamp.to_string().as_str(),
        payload
    );
}

fn log_delivery_outcome(outcome: Result<OwnedDeliveryResult, Canceled>) {
    match outcome {
        // Delivered — `Ok((partition, offset))`. Nothing to do (log here if wanted).
        Ok(Ok(_)) => {}
        Ok(Err((send_error, message))) => {
            let key = message
                .key()
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                .unwrap_or_default();
            error!(
                ["PRODUCER"],
                "Delivery failed (key '{}'): {:#?}", key, send_error
            );
        }
        Err(_canceled) => {
            error!(["PRODUCER"], "Delivery report dropped before completion");
        }
    }
}

async fn create_kafka_admin_client(
    app_configuration: &Configuration,
) -> Result<AdminClient<KafkaBrokerContext>, KafkaError> {
    info!(["ADMIN_CLIENT_CREATION"], "Creating admin client.");
    match ClientConfig::new()
        .set("bootstrap.servers", &app_configuration.kafka.broker)
        .create_with_context(KafkaBrokerContext {})
    {
        Ok(admin_client) => {
            info!(
                ["ADMIN_CLIENT_CREATION"],
                "Admin client created successfully"
            );
            Ok(admin_client)
        }
        Err(admin_client_error) => {
            error!(
                ["ADMIN_CLIENT_CREATION"],
                "Failed to create AdminClient with custom context"
            );
            Err(admin_client_error)
        }
    }
}

async fn init_kafka_topics(
    admin_client: AdminClient<KafkaBrokerContext>,
    app_configuration: &Configuration,
) {
    debug!(["TOPIC_CREATION"], "Creating topics object.");
    let kafka_topics = &app_configuration.kafka.topics;
    let kafka_new_topics: Vec<NewTopic<'_>> = kafka_topics
        .iter()
        .map(|element| NewTopic {
            name: &element.name,
            num_partitions: element.num_partition,
            replication: TopicReplication::Fixed(1),
            config: vec![],
        })
        .collect();

    if kafka_new_topics.is_empty() {
        debug!(
            ["TOPIC_CREATION"],
            "No topic created (no topic creation configured)."
        );
        return;
    }

    let options = AdminOptions::new();
    debug!(["TOPIC_CREATION"], "Sending request to Kafka Admin Client");
    match admin_client
        .create_topics(&kafka_new_topics, &options)
        .await
    {
        Ok(topic_created_list) => {
            for topic_creation_result in topic_created_list {
                match topic_creation_result {
                    Ok(topic) => debug!(["TOPIC_CREATION"], "{}", topic),
                    Err((topic, error_code)) => match error_code {
                        RDKafkaErrorCode::TopicAlreadyExists => {
                            warn!(["TOPIC_CREATION"], "Topic '{}' already exists", topic)
                        }
                        other => error!(
                            ["TOPIC_CREATION"],
                            "Topic '{}' creation failed -> {:#?}", topic, other
                        ),
                    },
                }
            }
        }
        Err(topic_creation_error) => {
            error!(
                ["TOPIC_CREATION"],
                "Topic creation failed: {}", topic_creation_error
            );
        }
    }
}

pub async fn init_kafka(
    app_configuration: Arc<Configuration>,
    kafka_thread_receiver: Receiver<KafkaChannelMessage>,
) {
    let admin_client = match create_kafka_admin_client(&app_configuration).await {
        Ok(client) => client,
        Err(_) => {
            error!(["INIT_KAFKA"], "Admin client creation failed");
            return;
        }
    };
    init_kafka_topics(admin_client, &app_configuration).await;

    let app_configuration_cloned = app_configuration.clone();
    let consumer_handle = tokio::spawn(async move {
        // TODO: Add retries
        if let Err(consumer_error) = init_kafka_consumer(&app_configuration_cloned).await {
            error!(["INIT_KAFKA"], "Consumer terminated: {}", consumer_error);
        }
    });

    let app_configuration_cloned = app_configuration.clone();
    let producer_handle = tokio::spawn(async move {
        // TODO: Add retries
        if let Err(producer_error) =
            init_kafka_producer(kafka_thread_receiver, &app_configuration_cloned).await
        {
            error!(["INIT_KAFKA"], "Producer terminated: {}", producer_error);
        }
    });

    if let Err(join_error) = tokio::try_join!(consumer_handle, producer_handle) {
        error!(["INIT_KAFKA"], "Kafka task join error: {}", join_error);
    }
}

async fn init_kafka_consumer(app_configuration: &Configuration) -> Result<(), KafkaError> {
    let consumer = create_kafka_consumer(app_configuration).await?;
    info!(["CONSUMER"], "Thread started, consuming stream.");

    consumer
        .stream()
        .for_each(|message_result| async {
            match message_result {
                Ok(borrowed_message) => log_kafka_message(borrowed_message.detach()).await,
                Err(consumer_error) => {
                    error!(
                        ["CONSUMER"],
                        "Stream error (continuing): {}", consumer_error
                    )
                }
            }
        })
        .await;

    Ok(())
}

async fn create_kafka_consumer(
    app_configuration: &Configuration,
) -> Result<StreamConsumer, KafkaError> {
    info!(["CONSUMER_CREATION"], "Creating consumer.");
    let consumer: StreamConsumer = ClientConfig::new()
        .set("group.id", &app_configuration.kafka.group_id)
        .set("bootstrap.servers", &app_configuration.kafka.broker)
        .set("enable.partition.eof", "false")
        .set("session.timeout.ms", "6000")
        .set("enable.auto.commit", "true")
        .set("auto.offset.reset", "earliest")
        .create()
        .inspect_err(|_| error!(["CONSUMER_CREATION"], "creation failed"))?;

    let topic_names: Vec<&str> = app_configuration
        .kafka
        .topics
        .iter()
        .map(|topic| topic.name.as_str())
        .collect();

    if topic_names.is_empty() {
        warn!(
            ["CONSUMER_CREATION"],
            "No topics configured to subscribe to."
        );
        return Ok(consumer);
    }

    info!(
        ["CONSUMER_CREATION"],
        "Subscribing to topics: {:?}", topic_names
    );
    match consumer.subscribe(&topic_names) {
        Ok(_) => {
            info!(["CONSUMER_CREATION"], "Subscribed.");
            Ok(consumer)
        }
        Err(consumer_subscription_error) => {
            error!(["CONSUMER_CREATION"], "Can't subscribe to topics");
            Err(consumer_subscription_error)
        }
    }
}

async fn init_kafka_producer(
    mut kafka_thread_receiver: Receiver<KafkaChannelMessage>,
    app_configuration: &Configuration,
) -> Result<(), KafkaError> {
    let producer = create_kafka_producer(app_configuration).await?;
    info!(["PRODUCER"], "Thread started, ready to send messages.");

    const MAX_IN_FLIGHT: usize = 1000;
    let mut in_flight = FuturesUnordered::new();

    loop {
        while in_flight.len() >= MAX_IN_FLIGHT {
            if let Some(outcome) = in_flight.next().await {
                log_delivery_outcome(outcome);
            }
        }

        tokio::select! {
            message = kafka_thread_receiver.recv() => match message {
                Some(KafkaChannelMessage { topic, sender, content }) => {
                    match producer.send_result(
                        FutureRecord::to(&topic).payload(&content).key(&sender),
                    ) {
                        Ok(delivery) => in_flight.push(delivery),
                        Err((enqueue_error, _record)) => error!(
                            ["PRODUCER"], "Enqueue failed: {:#?}", enqueue_error
                        ),
                    }
                }
                None => break,
            },

            Some(outcome) = in_flight.next(), if !in_flight.is_empty() => {
                log_delivery_outcome(outcome);
            }
        }
    }

    while let Some(outcome) = in_flight.next().await {
        log_delivery_outcome(outcome);
    }

    Ok(())
}

async fn create_kafka_producer(
    app_configuration: &Configuration,
) -> Result<FutureProducer, KafkaError> {
    info!(["PRODUCER_CREATION"], "Creating producer.");
    let producer: FutureProducer = ClientConfig::new()
        .set("bootstrap.servers", &app_configuration.kafka.broker)
        .set("message.timeout.ms", "5000")
        .set("enable.idempotence", "true")
        .create()
        .inspect_err(|_| error!(["PRODUCER_CREATION"], "Producer creation error"))?;

    info!(["PRODUCER_CREATION"], "Producer created.");
    Ok(producer)
}
