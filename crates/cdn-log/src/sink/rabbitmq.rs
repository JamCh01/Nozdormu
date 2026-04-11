use crate::config::RabbitMQLogConfig;
use crate::sink::{LogSink, LogSinkError};
use async_trait::async_trait;
use lapin::options::{BasicPublishOptions, ExchangeDeclareOptions};
use lapin::types::FieldTable;
use lapin::{BasicProperties, Channel, Connection, ConnectionProperties, ExchangeKind};
use tokio::sync::RwLock;

/// RabbitMQ (AMQP 0.9.1) log sink.
///
/// The `destination` parameter in `send()` is used as the routing_key.
pub struct RabbitMQSink {
    channel: RwLock<Option<Channel>>,
    config: RabbitMQLogConfig,
}

impl RabbitMQSink {
    pub fn new(config: RabbitMQLogConfig) -> Self {
        log::info!(
            "[LogSink:rabbitmq] configured with {} URL(s), exchange: {}",
            config.urls.len(),
            config.exchange
        );
        Self {
            channel: RwLock::new(None),
            config,
        }
    }

    async fn connect(&self) -> Result<Channel, LogSinkError> {
        let props = ConnectionProperties::default();
        for url in &self.config.urls {
            match Connection::connect(url, props.clone()).await {
                Ok(conn) => {
                    let channel = conn.create_channel().await.map_err(|e| {
                        LogSinkError::Connection(format!("channel creation failed: {}", e))
                    })?;
                    let kind = match self.config.exchange_type.as_str() {
                        "fanout" => ExchangeKind::Fanout,
                        "topic" => ExchangeKind::Topic,
                        "headers" => ExchangeKind::Headers,
                        _ => ExchangeKind::Direct,
                    };
                    let opts = ExchangeDeclareOptions {
                        durable: self.config.durable,
                        ..ExchangeDeclareOptions::default()
                    };
                    channel
                        .exchange_declare(&self.config.exchange, kind, opts, FieldTable::default())
                        .await
                        .map_err(|e| {
                            LogSinkError::Connection(format!("exchange declare failed: {}", e))
                        })?;
                    log::info!("[LogSink:rabbitmq] connected to {}", url);
                    return Ok(channel);
                }
                Err(e) => {
                    log::warn!("[LogSink:rabbitmq] failed to connect to {}: {}", url, e);
                    continue;
                }
            }
        }
        Err(LogSinkError::Connection(
            "all RabbitMQ URLs failed".to_string(),
        ))
    }

    async fn get_channel(&self) -> Result<Channel, LogSinkError> {
        {
            let guard = self.channel.read().await;
            if let Some(ref ch) = *guard {
                if ch.status().connected() {
                    return Ok(ch.clone());
                }
            }
        }
        let new_channel = self.connect().await?;
        let mut guard = self.channel.write().await;
        *guard = Some(new_channel.clone());
        Ok(new_channel)
    }
}

#[async_trait]
impl LogSink for RabbitMQSink {
    async fn send(
        &self,
        destination: &str,
        entries: &[String],
    ) -> Result<(), LogSinkError> {
        let channel = self.get_channel().await?;
        let mut last_err = None;
        for json in entries {
            let result = channel
                .basic_publish(
                    &self.config.exchange,
                    destination,
                    BasicPublishOptions::default(),
                    json.as_bytes(),
                    BasicProperties::default()
                        .with_content_type("application/json".into())
                        .with_delivery_mode(if self.config.durable { 2 } else { 1 }),
                )
                .await;
            match result {
                Ok(confirm) => {
                    if let Err(e) = confirm.await {
                        log::warn!("[LogSink:rabbitmq] publish confirm failed: {}", e);
                        last_err = Some(e.to_string());
                    }
                }
                Err(e) => {
                    log::warn!("[LogSink:rabbitmq] publish to {} failed: {}", destination, e);
                    last_err = Some(e.to_string());
                    let mut guard = self.channel.write().await;
                    *guard = None;
                    break;
                }
            }
        }
        match last_err {
            Some(e) => Err(LogSinkError::Send(format!(
                "RabbitMQ send error (last): {}",
                e
            ))),
            None => Ok(()),
        }
    }

    async fn flush(&self) -> Result<(), LogSinkError> {
        Ok(())
    }

    fn name(&self) -> &'static str {
        "rabbitmq"
    }
}
