use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct CdnConfig {
    pub listen: String,
    pub metrics_listen: String,
    pub upstreams: Vec<UpstreamConfig>,
    #[serde(default)]
    pub health_check: HealthCheckConfig,
    #[serde(default = "default_load_balancing")]
    pub load_balancing: LoadBalancingAlgorithm,
}

#[derive(Debug, Deserialize, Clone)]
pub struct UpstreamConfig {
    pub address: String,
    #[serde(default)]
    pub tls: bool,
    #[serde(default)]
    pub sni: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct HealthCheckConfig {
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    #[serde(default)]
    pub r#type: HealthCheckType,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            interval_secs: default_interval(),
            r#type: HealthCheckType::default(),
        }
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "snake_case")]
pub enum HealthCheckType {
    #[default]
    Tcp,
    Http,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "snake_case")]
pub enum LoadBalancingAlgorithm {
    #[default]
    RoundRobin,
    Ketama,
}

fn default_interval() -> u64 {
    10
}

fn default_load_balancing() -> LoadBalancingAlgorithm {
    LoadBalancingAlgorithm::default()
}
