pub mod types;
pub mod node_config;
pub mod live_config;
pub mod etcd_watcher;
pub mod schema;

pub use types::*;
pub use node_config::NodeConfig;
pub use live_config::LiveConfig;
pub use etcd_watcher::EtcdConfigManager;

use cdn_common::CdnResult;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct RawConfig {
    cdn: CdnConfig,
}

/// Load the basic CDN config from a YAML file (Pingora server config format).
/// This is used for the initial bootstrap before etcd is available.
pub fn load_cdn_config(path: &Path) -> CdnResult<CdnConfig> {
    let content = std::fs::read_to_string(path).map_err(cdn_common::CdnError::Io)?;
    let raw: RawConfig =
        serde_yaml::from_str(&content).map_err(|e| cdn_common::CdnError::Config(e.to_string()))?;
    log::info!("loaded CDN config from {}", path.display());
    log::info!(
        "  upstreams: {} backends, lb: {:?}",
        raw.cdn.upstreams.len(),
        raw.cdn.load_balancing
    );
    Ok(raw.cdn)
}
