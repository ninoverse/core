use std::sync::Arc;

use chrono::DateTime;

use rdkafka::{
    ClientConfig, Message,
    admin::{AdminClient, AdminOptions, NewTopic, TopicReplication},
    client::ClientContext,
    consumer::{Consumer, StreamConsumer},
    producer::{FutureProducer, FutureRecord, future_producer::OwnedDeliveryResult},
    types::RDKafkaErrorCode,
};

use futures::{StreamExt, channel::oneshot::Canceled, stream::FuturesUnordered};

use tokio::sync::{broadcast, mpsc::Receiver};

use configuration::NinoverseCoreConfiguration;

use logger::{debug, error, info, warn};

use crate::error::{CoreResult, KafkaError, KafkaResult};

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

fn base_client_config(app_configuration: &NinoverseCoreConfiguration) -> ClientConfig {
    let mut client_configuration = ClientConfig::new();
    client_configuration
        .set("bootstrap.servers", &app_configuration.kafka.broker)
        .set(
            "security.protocol",
            &app_configuration.kafka.security_protocol,
        )
        .set("sasl.mechanism", &app_configuration.kafka.sasl_mechanism)
        .set("sasl.username", &app_configuration.kafka.sasl_username)
        .set("sasl.password", &app_configuration.kafka.sasl_password);
    client_configuration
}

async fn create_kafka_admin_client(
    app_configuration: &NinoverseCoreConfiguration,
) -> KafkaResult<AdminClient<KafkaBrokerContext>> {
    info!(["ADMIN_CLIENT_CREATION"], "Creating admin client.");
    match base_client_config(app_configuration).create_with_context(KafkaBrokerContext {}) {
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
            Err(KafkaError::RDKafka(admin_client_error))
        }
    }
}

async fn init_kafka_topics(
    admin_client: AdminClient<KafkaBrokerContext>,
    app_configuration: &NinoverseCoreConfiguration,
) -> KafkaResult<()> {
    info!(["TOPIC_CREATION"], "Creating topics object.");
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
        info!(
            ["TOPIC_CREATION"],
            "No topic created (no topic creation configured)."
        );
        return Err(KafkaError::NoTopicInConfiguration);
    }

    let options = AdminOptions::new();
    info!(["TOPIC_CREATION"], "Sending request to Kafka Admin Client");
    match admin_client
        .create_topics(&kafka_new_topics, &options)
        .await
    {
        Ok(topic_created_list) => {
            for topic_creation_result in topic_created_list {
                match topic_creation_result {
                    Ok(topic) => info!(["TOPIC_CREATION"], "Topic {}' created", topic),
                    Err((topic, error_code)) => match error_code {
                        RDKafkaErrorCode::TopicAlreadyExists => {
                            warn!(["TOPIC_CREATION"], "Topic '{}' already exists", topic)
                        }
                        other => {
                            error!(
                                ["TOPIC_CREATION"],
                                "Topic '{}' creation failed -> {:#?}", topic, other
                            );
                            return Err(KafkaError::ErrorCode(other));
                        }
                    },
                }
            }
            Ok(())
        }
        Err(topic_creation_error) => {
            error!(
                ["TOPIC_CREATION"],
                "Topic creation failed: {}", topic_creation_error
            );
            Err(KafkaError::RDKafka(topic_creation_error))
        }
    }
}

pub async fn init_kafka(
    app_configuration: Arc<NinoverseCoreConfiguration>,
    kafka_thread_receiver: Receiver<KafkaChannelMessage>,
    shutdown_broadcast_sender: &broadcast::Sender<()>,
) -> CoreResult<()> {
    // Subscribe before the first await: a `broadcast::Receiver` only sees values
    // sent after it subscribed, so subscribing later could miss a shutdown that
    // arrives during admin-client or topic setup and leave these tasks running.
    let mut topic_shutdown_receiver = shutdown_broadcast_sender.subscribe();
    let mut consumer_shutdown_receiver = shutdown_broadcast_sender.subscribe();
    let mut producer_shutdown_receiver = shutdown_broadcast_sender.subscribe();

    let admin_client = match create_kafka_admin_client(&app_configuration).await {
        Ok(client) => client,
        Err(admin_client_creation_error) => {
            error!(["INIT_KAFKA"], "Admin client creation failed");
            return Err(admin_client_creation_error.into());
        }
    };

    tokio::select! {
        topic_result = init_kafka_topics(admin_client, &app_configuration) => topic_result?,
        _ = topic_shutdown_receiver.recv() => {
            info!(
                ["INIT_KAFKA"],
                "Shutdown requested during topic creation. Aborting Kafka startup."
            );
            return Ok(());
        }
    }

    let app_configuration_cloned = app_configuration.clone();
    let consumer_handle = tokio::spawn(async move {
        // TODO: Add retries
        init_kafka_consumer(&app_configuration_cloned, &mut consumer_shutdown_receiver).await
    });

    let app_configuration_cloned = app_configuration.clone();
    let producer_handle = tokio::spawn(async move {
        // TODO: Add retries
        init_kafka_producer(
            kafka_thread_receiver,
            &app_configuration_cloned,
            &mut producer_shutdown_receiver,
        )
        .await
    });

    match tokio::try_join!(consumer_handle, producer_handle) {
        Ok((consumer_result, producer_result)) => {
            if let Err(consumer_error) = &consumer_result {
                error!(["INIT_KAFKA"], "Consumer terminated: {}", consumer_error);
            }
            if let Err(producer_error) = &producer_result {
                error!(["INIT_KAFKA"], "Producer terminated: {}", producer_error);
            }
            consumer_result?;
            producer_result?;
            Ok(())
        }
        Err(join_error) => {
            error!(["INIT_KAFKA"], "Kafka task join error: {}", join_error);
            Err(join_error.into())
        }
    }
}

async fn init_kafka_consumer(
    app_configuration: &NinoverseCoreConfiguration,
    shutdown_receiver: &mut broadcast::Receiver<()>,
) -> KafkaResult<()> {
    let consumer = create_kafka_consumer(app_configuration).await?;
    info!(["CONSUMER"], "Thread started, consuming stream.");

    loop {
        tokio::select! {
            // `StreamConsumer::recv` is cancel-safe, so losing this branch to the
            // shutdown arm cannot drop an already-acknowledged message.
            message_result = consumer.recv() => match message_result {
                Ok(borrowed_message) => log_kafka_message(borrowed_message.detach()).await,
                Err(consumer_error) => {
                    error!(
                        ["CONSUMER"],
                        "Stream error (continuing): {}", consumer_error
                    )
                }
            },
            _ = shutdown_receiver.recv() => {
                info!(["CONSUMER"], "Shutdown signal received. Stopping consumer.");
                break;
            }
        }
    }

    Ok(())
}

async fn create_kafka_consumer(
    app_configuration: &NinoverseCoreConfiguration,
) -> KafkaResult<StreamConsumer> {
    info!(["CONSUMER_CREATION"], "Creating consumer.");
    let consumer: StreamConsumer = base_client_config(app_configuration)
        .set("group.id", &app_configuration.kafka.group_id)
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
            Err(KafkaError::RDKafka(consumer_subscription_error))
        }
    }
}

async fn init_kafka_producer(
    mut kafka_thread_receiver: Receiver<KafkaChannelMessage>,
    app_configuration: &NinoverseCoreConfiguration,
    shutdown_receiver: &mut broadcast::Receiver<()>,
) -> KafkaResult<()> {
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

            _ = shutdown_receiver.recv() => {
                info!(
                    ["PRODUCER"],
                    "Shutdown signal received. Draining in-flight messages."
                );
                break;
            }
        }
    }

    while let Some(outcome) = in_flight.next().await {
        log_delivery_outcome(outcome);
    }

    Ok(())
}

async fn create_kafka_producer(
    app_configuration: &NinoverseCoreConfiguration,
) -> KafkaResult<FutureProducer> {
    info!(["PRODUCER_CREATION"], "Creating producer.");
    let producer: FutureProducer = base_client_config(app_configuration)
        .set("message.timeout.ms", "5000")
        .set("enable.idempotence", "true")
        .create()
        .inspect_err(|_| error!(["PRODUCER_CREATION"], "Producer creation error"))?;

    info!(["PRODUCER_CREATION"], "Producer created.");
    Ok(producer)
}
