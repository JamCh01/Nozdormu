pub mod demux;

use crate::config::IngestConfig;
use crate::store::LiveStreamStore;
use async_trait::async_trait;
use pingora::server::ShutdownWatch;
use pingora::services::background::BackgroundService;
use std::net::SocketAddr;
use std::sync::Arc;

/// SRT ingest background service.
///
/// Listens for SRT connections on the configured UDP address and spawns
/// a handler task for each connection.
pub struct SrtIngestService {
    listen_addr: SocketAddr,
    store: Arc<LiveStreamStore>,
    config: IngestConfig,
}

impl SrtIngestService {
    pub fn new(listen_addr: SocketAddr, store: Arc<LiveStreamStore>, config: IngestConfig) -> Self {
        Self {
            listen_addr,
            store,
            config,
        }
    }
}

#[async_trait]
impl BackgroundService for SrtIngestService {
    async fn start(&self, mut shutdown: ShutdownWatch) {
        let (_listener, mut incoming) = match srt_tokio::SrtListener::builder()
            .bind(self.listen_addr)
            .await
        {
            Ok(pair) => pair,
            Err(e) => {
                log::error!("[SRT] failed to bind {}: {}", self.listen_addr, e);
                return;
            }
        };
        log::info!("[SRT] listening on {}", self.listen_addr);

        use futures_util::StreamExt;

        loop {
            tokio::select! {
                request = incoming.incoming().next() => {
                    match request {
                        Some(request) => {
                            crate::metrics::INGEST_CONNECTIONS_TOTAL
                                .with_label_values(&["srt"])
                                .inc();

                            let store = Arc::clone(&self.store);
                            let config = self.config.clone();

                            tokio::spawn(async move {
                                if let Err(e) = demux::handle_srt_connection(
                                    request, store, config
                                ).await {
                                    log::warn!("[SRT] stream error: {}", e);
                                }
                            });
                        }
                        None => {
                            log::info!("[SRT] listener closed");
                            break;
                        }
                    }
                }
                _ = shutdown.changed() => {
                    log::info!("[SRT] shutting down");
                    break;
                }
            }
        }
    }
}
