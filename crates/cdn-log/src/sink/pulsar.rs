use crate::config::PulsarLogConfig;
use crate::sink::{LogSink, LogSinkError};
use async_trait::async_trait;
use dashmap::DashMap;
use pulsar::{producer, Authentication, Pulsar, TokioExecutor};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Apache Pulsar log sink with multi-topic support.
///
/// The `destination` parameter in `send()` is used as the Pulsar topic.
/// Producers are lazily created per-topic and cached in a DashMap.
pub struct PulsarSink {
    client: Arc<Pulsar<TokioExecutor>>,
    producers: DashMap<String, Arc<Mutex<producer::Producer<TokioExecutor>>>>,
}

impl PulsarSink {
    pub async fn new(config: &PulsarLogConfig) -> Result<Self, LogSinkError> {
        let addr = config.urls.join(",");
        let mut builder = Pulsar::builder(&addr, TokioExecutor);

        if let Some(ref token) = config.token {
            builder = builder.with_auth(Authentication {
                name: "token".to_string(),
                data: token.as_bytes().to_vec(),
            });
        }

        let client: Pulsar<TokioExecutor> = builder
            .build()
            .await
            .map_err(|e| LogSinkError::Connection(format!("Pulsar connect failed: {}", e)))?;

        log::info!("[LogSink:pulsar] connected to {}", addr);

        Ok(Self {
            client: Arc::new(client),
            producers: DashMap::new(),
        })
    }

    async fn get_producer(
        &self,
        topic: &str,
    ) -> Result<Arc<Mutex<producer::Producer<TokioExecutor>>>, LogSinkError> {
        if let Some(p) = self.producers.get(topic) {
            return Ok(Arc::clone(p.value()));
        }
        let producer = self
            .client
            .producer()
            .with_topic(topic)
            .with_name(&format!("nozdormu-log-{}", topic))
            .build()
            .await
            .map_err(|e| {
                LogSinkError::Connection(format!(
                    "Pulsar producer for topic {} failed: {}",
                    topic, e
                ))
            })?;
        let producer = Arc::new(Mutex::new(producer));
        self.producers
            .insert(topic.to_string(), Arc::clone(&producer));
        log::info!("[LogSink:pulsar] created producer for topic: {}", topic);
        Ok(producer)
    }
}

#[async_trait]
impl LogSink for PulsarSink {
    async fn send(&self, destination: &str, entries: &[String]) -> Result<(), LogSinkError> {
        let producer_arc = self.get_producer(destination).await?;
        let mut producer = producer_arc.lock().await;
        let mut last_err = None;
        for json in entries {
            match producer.send_non_blocking(json.as_str()).await {
                Ok(receipt) => {
                    if let Err(e) = receipt.await {
                        log::warn!(
                            "[LogSink:pulsar] send to {} receipt error: {}",
                            destination,
                            e
                        );
                        last_err = Some(e.to_string());
                    }
                }
                Err(e) => {
                    log::warn!("[LogSink:pulsar] send to {} failed: {}", destination, e);
                    last_err = Some(e.to_string());
                }
            }
        }
        match last_err {
            Some(e) => Err(LogSinkError::Send(format!(
                "Pulsar send error (last): {}",
                e
            ))),
            None => Ok(()),
        }
    }

    async fn flush(&self) -> Result<(), LogSinkError> {
        for entry in self.producers.iter() {
            let mut p = entry.value().lock().await;
            if let Err(e) = p.send_batch().await {
                log::warn!("[LogSink:pulsar] flush error for {}: {}", entry.key(), e);
            }
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        "pulsar"
    }
}
