use crate::sink::{LogSink, LogSinkError};
use async_nats::jetstream;
use async_trait::async_trait;
use bytes::Bytes;

/// NATS log sink with optional JetStream persistence.
///
/// The `destination` parameter in `send()` is used as the NATS subject.
pub struct NatsSink {
    client: async_nats::Client,
    jetstream: Option<jetstream::Context>,
}

impl NatsSink {
    pub async fn new(config: &crate::config::NatsLogConfig) -> Result<Self, LogSinkError> {
        let server_addr = config.urls.join(",");
        let mut opts = async_nats::ConnectOptions::new();

        if let Some(ref user) = config.username {
            if let Some(ref pass) = config.password {
                opts = opts.user_and_password(user.to_string(), pass.to_string());
            }
        }
        if let Some(ref token) = config.token {
            opts = opts.token(token.to_string());
        }

        let client = opts
            .connect(&server_addr)
            .await
            .map_err(|e| LogSinkError::Connection(format!("NATS connect failed: {}", e)))?;

        let jetstream = if let Some(ref stream_name) = config.stream_name {
            let js = jetstream::new(client.clone());
            // Collect all enabled channel subjects for the JetStream stream
            let mut subjects = vec![config.subject.clone()];
            let ch = &config.channels;
            for dest in [
                &ch.client_to_cdn.destination,
                &ch.cdn_to_origin.destination,
                &ch.origin_to_cdn.destination,
                &ch.cdn_to_client.destination,
                &ch.waf.destination,
                &ch.cc.destination,
                &ch.cache.destination,
                &ch.access.destination,
            ] {
                if !subjects.contains(dest) {
                    subjects.push(dest.clone());
                }
            }
            let stream_config = jetstream::stream::Config {
                name: stream_name.clone(),
                subjects,
                ..Default::default()
            };
            js.get_or_create_stream(stream_config).await.map_err(|e| {
                LogSinkError::Connection(format!("JetStream stream setup failed: {}", e))
            })?;
            log::info!("[LogSink:nats] JetStream enabled, stream: {}", stream_name);
            Some(js)
        } else {
            None
        };

        log::info!("[LogSink:nats] connected to {}", server_addr);

        Ok(Self { client, jetstream })
    }
}

#[async_trait]
impl LogSink for NatsSink {
    async fn send(&self, destination: &str, entries: &[String]) -> Result<(), LogSinkError> {
        let mut last_err = None;
        for json in entries {
            let payload = Bytes::from(json.clone());
            let result = if let Some(ref js) = self.jetstream {
                js.publish(destination.to_string(), payload)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            } else {
                self.client
                    .publish(destination.to_string(), payload)
                    .await
                    .map_err(|e| e.to_string())
            };
            if let Err(e) = result {
                log::warn!("[LogSink:nats] publish to {} failed: {}", destination, e);
                last_err = Some(e);
            }
        }
        match last_err {
            Some(e) => Err(LogSinkError::Send(format!("NATS send error (last): {}", e))),
            None => Ok(()),
        }
    }

    async fn flush(&self) -> Result<(), LogSinkError> {
        self.client
            .flush()
            .await
            .map_err(|e| LogSinkError::Send(format!("NATS flush failed: {}", e)))
    }

    fn name(&self) -> &'static str {
        "nats"
    }
}
