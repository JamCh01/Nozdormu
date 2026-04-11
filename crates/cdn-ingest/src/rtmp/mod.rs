pub mod session;

use crate::config::IngestConfig;
use crate::store::LiveStreamStore;
use async_trait::async_trait;
use pingora::server::ShutdownWatch;
use pingora::services::background::BackgroundService;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

/// RTMP ingest background service.
///
/// Listens for RTMP connections on the configured address and spawns
/// a handler task for each connection.
pub struct RtmpIngestService {
    listen_addr: SocketAddr,
    store: Arc<LiveStreamStore>,
    config: IngestConfig,
}

impl RtmpIngestService {
    pub fn new(listen_addr: SocketAddr, store: Arc<LiveStreamStore>, config: IngestConfig) -> Self {
        Self {
            listen_addr,
            store,
            config,
        }
    }
}

#[async_trait]
impl BackgroundService for RtmpIngestService {
    async fn start(&self, mut shutdown: ShutdownWatch) {
        let listener = match TcpListener::bind(&self.listen_addr).await {
            Ok(l) => l,
            Err(e) => {
                log::error!("[RTMP] failed to bind {}: {}", self.listen_addr, e);
                return;
            }
        };
        log::info!("[RTMP] listening on {}", self.listen_addr);

        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, addr)) => {
                            crate::metrics::INGEST_CONNECTIONS_TOTAL
                                .with_label_values(&["rtmp"])
                                .inc();
                            let store = Arc::clone(&self.store);
                            let config = self.config.clone();
                            tokio::spawn(async move {
                                if let Err(e) = session::handle_rtmp_connection(
                                    stream, addr, store, config
                                ).await {
                                    log::warn!("[RTMP] connection from {} error: {}", addr, e);
                                }
                            });
                        }
                        Err(e) => {
                            log::warn!("[RTMP] accept error: {}", e);
                        }
                    }
                }
                _ = shutdown.changed() => {
                    log::info!("[RTMP] shutting down");
                    break;
                }
            }
        }
    }
}
