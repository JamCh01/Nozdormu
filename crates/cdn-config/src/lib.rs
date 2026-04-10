pub mod etcd_watcher;
pub mod global_config;
pub mod live_config;
pub mod node_config;
pub mod schema;
pub mod types;

pub use etcd_watcher::load_global_config;
pub use etcd_watcher::EtcdConfigManager;
pub use global_config::GlobalConfig;
pub use live_config::LiveConfig;
pub use node_config::BootstrapConfig;
pub use node_config::EabCredentials;
pub use node_config::NodeConfig;
pub use types::*;

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
