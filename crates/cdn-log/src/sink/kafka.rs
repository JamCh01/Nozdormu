use crate::config::KafkaLogConfig;
use crate::sink::{LogSink, LogSinkError};
use async_trait::async_trait;
use rdkafka::config::ClientConfig;
use rdkafka::producer::{FutureProducer, FutureRecord, Producer};
use std::time::Duration;

/// Apache Kafka log sink.
///
/// The `destination` parameter in `send()` is used as the Kafka topic name.
pub struct KafkaSink {
    producer: FutureProducer,
}

impl KafkaSink {
    pub fn new(config: &KafkaLogConfig) -> Result<Self, LogSinkError> {
        let brokers = config.brokers.join(",");
        let mut client_config = ClientConfig::new();
        client_config
            .set("bootstrap.servers", &brokers)
            .set("message.timeout.ms", "5000")
            .set("request.required.acks", &config.acks);

        if let Some(ref client_id) = config.client_id {
            client_config.set("client.id", client_id);
        }
        if let Some(ref protocol) = config.security_protocol {
            client_config.set("security.protocol", protocol);
        }
        if let Some(ref mechanism) = config.sasl_mechanism {
            client_config.set("sasl.mechanism", mechanism);
        }
        if let Some(ref username) = config.sasl_username {
            client_config.set("sasl.username", username);
        }
        if let Some(ref password) = config.sasl_password {
            client_config.set("sasl.password", password);
        }
        if let Some(ref compression) = config.compression_type {
            client_config.set("compression.type", compression);
        }

        let producer: FutureProducer = client_config
            .create()
            .map_err(|e| LogSinkError::Config(format!("Kafka producer creation failed: {}", e)))?;

        log::info!("[LogSink:kafka] connected to brokers: {}", brokers);

        Ok(Self { producer })
    }
}

#[async_trait]
impl LogSink for KafkaSink {
    async fn send(&self, destination: &str, entries: &[String]) -> Result<(), LogSinkError> {
        let mut last_err = None;
        for json in entries {
            let record = FutureRecord::to(destination).payload(json).key("");
            if let Err((e, _)) = self.producer.send(record, Duration::from_secs(5)).await {
                log::warn!("[LogSink:kafka] send to {} failed: {}", destination, e);
                last_err = Some(e);
            }
        }
        match last_err {
            Some(e) => Err(LogSinkError::Send(format!(
                "Kafka send error (last): {}",
                e
            ))),
            None => Ok(()),
        }
    }

    async fn flush(&self) -> Result<(), LogSinkError> {
        self.producer
            .flush(Duration::from_secs(10))
            .map_err(|e| LogSinkError::Send(format!("Kafka flush failed: {}", e)))
    }

    fn name(&self) -> &'static str {
        "kafka"
    }
}
