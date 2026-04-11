use serde::{Deserialize, Serialize};

// ============================================================
// Log Channel Config — per-channel destination + enabled
// ============================================================

/// Configuration for a single log channel.
///
/// Each channel has an independent enabled/disabled switch and a destination
/// string whose meaning depends on the backend:
/// - Kafka/Pulsar: topic name
/// - NATS: subject
/// - RabbitMQ: routing_key
/// - Redis: stream_key
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogChannelConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub destination: String,
}

/// All 8 log channels with independent destinations and enabled switches.
///
/// Channels:
/// - `client_to_cdn`: request received → upstream connect start
/// - `cdn_to_origin`: upstream connect start → connection established
/// - `origin_to_cdn`: request sent → response headers received
/// - `cdn_to_client`: response headers received → response fully sent
/// - `waf`: WAF check events (blocked or passed)
/// - `cc`: CC rate-limit events (blocked or passed)
/// - `cache`: cache hit/miss/bypass events
/// - `access`: complete request log (all fields)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogChannelsConfig {
    #[serde(default = "default_channel_client_to_cdn")]
    pub client_to_cdn: LogChannelConfig,
    #[serde(default = "default_channel_cdn_to_origin")]
    pub cdn_to_origin: LogChannelConfig,
    #[serde(default = "default_channel_origin_to_cdn")]
    pub origin_to_cdn: LogChannelConfig,
    #[serde(default = "default_channel_cdn_to_client")]
    pub cdn_to_client: LogChannelConfig,
    #[serde(default = "default_channel_waf")]
    pub waf: LogChannelConfig,
    #[serde(default = "default_channel_cc")]
    pub cc: LogChannelConfig,
    #[serde(default = "default_channel_cache")]
    pub cache: LogChannelConfig,
    #[serde(default = "default_channel_access")]
    pub access: LogChannelConfig,
}

fn default_true() -> bool {
    true
}

fn make_channel(enabled: bool, suffix: &str) -> LogChannelConfig {
    LogChannelConfig {
        enabled,
        destination: format!("nozdormu-logs.{}", suffix),
    }
}

fn default_channel_client_to_cdn() -> LogChannelConfig {
    make_channel(true, "client_to_cdn")
}
fn default_channel_cdn_to_origin() -> LogChannelConfig {
    make_channel(true, "cdn_to_origin")
}
fn default_channel_origin_to_cdn() -> LogChannelConfig {
    make_channel(true, "origin_to_cdn")
}
fn default_channel_cdn_to_client() -> LogChannelConfig {
    make_channel(true, "cdn_to_client")
}
fn default_channel_waf() -> LogChannelConfig {
    make_channel(true, "waf")
}
fn default_channel_cc() -> LogChannelConfig {
    make_channel(true, "cc")
}
fn default_channel_cache() -> LogChannelConfig {
    make_channel(true, "cache")
}
fn default_channel_access() -> LogChannelConfig {
    make_channel(true, "access")
}

impl Default for LogChannelsConfig {
    fn default() -> Self {
        Self {
            client_to_cdn: default_channel_client_to_cdn(),
            cdn_to_origin: default_channel_cdn_to_origin(),
            origin_to_cdn: default_channel_origin_to_cdn(),
            cdn_to_client: default_channel_cdn_to_client(),
            waf: default_channel_waf(),
            cc: default_channel_cc(),
            cache: default_channel_cache(),
            access: default_channel_access(),
        }
    }
}

// ============================================================
// Backend Config Enum (tagged union)
// ============================================================

/// Log backend configuration. Selected via `"type"` field in JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LogBackendConfig {
    Redis(RedisLogConfig),
    Kafka(KafkaLogConfig),
    #[serde(rename = "rabbitmq")]
    RabbitMQ(RabbitMQLogConfig),
    Nats(NatsLogConfig),
    Pulsar(PulsarLogConfig),
}

impl LogBackendConfig {
    pub fn channels(&self) -> &LogChannelsConfig {
        match self {
            LogBackendConfig::Redis(c) => &c.channels,
            LogBackendConfig::Kafka(c) => &c.channels,
            LogBackendConfig::RabbitMQ(c) => &c.channels,
            LogBackendConfig::Nats(c) => &c.channels,
            LogBackendConfig::Pulsar(c) => &c.channels,
        }
    }
}

// ============================================================
// Redis
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisLogConfig {
    #[serde(default = "default_stream_key")]
    pub stream_key: String,
    #[serde(default = "default_max_len")]
    pub max_len: u64,
    #[serde(default)]
    pub channels: LogChannelsConfig,
}

fn default_stream_key() -> String {
    "nozdormu:log:requests".to_string()
}

fn default_max_len() -> u64 {
    100_000
}

impl Default for RedisLogConfig {
    fn default() -> Self {
        Self {
            stream_key: default_stream_key(),
            max_len: default_max_len(),
            channels: LogChannelsConfig::default(),
        }
    }
}

// ============================================================
// Kafka
// ============================================================

#[derive(Clone, Serialize, Deserialize)]
pub struct KafkaLogConfig {
    pub brokers: Vec<String>,
    #[serde(default = "default_kafka_topic")]
    pub topic: String,
    pub client_id: Option<String>,
    pub security_protocol: Option<String>,
    pub sasl_mechanism: Option<String>,
    pub sasl_username: Option<String>,
    pub sasl_password: Option<String>,
    #[serde(default = "default_kafka_acks")]
    pub acks: String,
    pub compression_type: Option<String>,
    #[serde(default)]
    pub channels: LogChannelsConfig,
}

fn default_kafka_topic() -> String {
    "nozdormu-logs".to_string()
}

fn default_kafka_acks() -> String {
    "1".to_string()
}

impl Default for KafkaLogConfig {
    fn default() -> Self {
        Self {
            brokers: vec!["localhost:9092".to_string()],
            topic: default_kafka_topic(),
            client_id: None,
            security_protocol: None,
            sasl_mechanism: None,
            sasl_username: None,
            sasl_password: None,
            acks: default_kafka_acks(),
            compression_type: None,
            channels: LogChannelsConfig::default(),
        }
    }
}

impl std::fmt::Debug for KafkaLogConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KafkaLogConfig")
            .field("brokers", &self.brokers)
            .field("topic", &self.topic)
            .field("client_id", &self.client_id)
            .field("security_protocol", &self.security_protocol)
            .field("sasl_mechanism", &self.sasl_mechanism)
            .field("sasl_username", &self.sasl_username)
            .field(
                "sasl_password",
                &self.sasl_password.as_ref().map(|_| "[REDACTED]"),
            )
            .field("acks", &self.acks)
            .field("compression_type", &self.compression_type)
            .field("channels", &self.channels)
            .finish()
    }
}

// ============================================================
// RabbitMQ
// ============================================================

#[derive(Clone, Serialize, Deserialize)]
pub struct RabbitMQLogConfig {
    pub urls: Vec<String>,
    #[serde(default = "default_rabbitmq_exchange")]
    pub exchange: String,
    #[serde(default = "default_rabbitmq_routing_key")]
    pub routing_key: String,
    #[serde(default = "default_rabbitmq_exchange_type")]
    pub exchange_type: String,
    #[serde(default = "default_true")]
    pub durable: bool,
    #[serde(default)]
    pub channels: LogChannelsConfig,
}

fn default_rabbitmq_exchange() -> String {
    "nozdormu.logs".to_string()
}

fn default_rabbitmq_routing_key() -> String {
    "request".to_string()
}

fn default_rabbitmq_exchange_type() -> String {
    "direct".to_string()
}

impl Default for RabbitMQLogConfig {
    fn default() -> Self {
        Self {
            urls: vec!["amqp://guest:guest@localhost:5672/%2f".to_string()],
            exchange: default_rabbitmq_exchange(),
            routing_key: default_rabbitmq_routing_key(),
            exchange_type: default_rabbitmq_exchange_type(),
            durable: default_true(),
            channels: LogChannelsConfig::default(),
        }
    }
}

impl std::fmt::Debug for RabbitMQLogConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redacted: Vec<String> = self
            .urls
            .iter()
            .map(|u| {
                if let Some(at_pos) = u.find('@') {
                    if let Some(scheme_end) = u.find("://") {
                        return format!("{}://[REDACTED]@{}", &u[..scheme_end], &u[at_pos + 1..]);
                    }
                }
                u.clone()
            })
            .collect();
        f.debug_struct("RabbitMQLogConfig")
            .field("urls", &redacted)
            .field("exchange", &self.exchange)
            .field("routing_key", &self.routing_key)
            .field("exchange_type", &self.exchange_type)
            .field("durable", &self.durable)
            .field("channels", &self.channels)
            .finish()
    }
}

// ============================================================
// NATS
// ============================================================

#[derive(Clone, Serialize, Deserialize)]
pub struct NatsLogConfig {
    pub urls: Vec<String>,
    #[serde(default = "default_nats_subject")]
    pub subject: String,
    pub stream_name: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub token: Option<String>,
    #[serde(default)]
    pub channels: LogChannelsConfig,
}

fn default_nats_subject() -> String {
    "nozdormu.logs".to_string()
}

impl Default for NatsLogConfig {
    fn default() -> Self {
        Self {
            urls: vec!["nats://localhost:4222".to_string()],
            subject: default_nats_subject(),
            stream_name: None,
            username: None,
            password: None,
            token: None,
            channels: LogChannelsConfig::default(),
        }
    }
}

impl std::fmt::Debug for NatsLogConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NatsLogConfig")
            .field("urls", &self.urls)
            .field("subject", &self.subject)
            .field("stream_name", &self.stream_name)
            .field("username", &self.username)
            .field(
                "password",
                &self.password.as_ref().map(|_| "[REDACTED]"),
            )
            .field("token", &self.token.as_ref().map(|_| "[REDACTED]"))
            .field("channels", &self.channels)
            .finish()
    }
}

// ============================================================
// Pulsar
// ============================================================

#[derive(Clone, Serialize, Deserialize)]
pub struct PulsarLogConfig {
    pub urls: Vec<String>,
    #[serde(default = "default_pulsar_topic")]
    pub topic: String,
    pub token: Option<String>,
    #[serde(default)]
    pub channels: LogChannelsConfig,
}

fn default_pulsar_topic() -> String {
    "persistent://public/default/nozdormu-logs".to_string()
}

impl Default for PulsarLogConfig {
    fn default() -> Self {
        Self {
            urls: vec!["pulsar://localhost:6650".to_string()],
            topic: default_pulsar_topic(),
            token: None,
            channels: LogChannelsConfig::default(),
        }
    }
}

impl std::fmt::Debug for PulsarLogConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PulsarLogConfig")
            .field("urls", &self.urls)
            .field("topic", &self.topic)
            .field("token", &self.token.as_ref().map(|_| "[REDACTED]"))
            .field("channels", &self.channels)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channels_default() {
        let ch = LogChannelsConfig::default();
        assert!(ch.access.enabled);
        assert!(ch.waf.enabled);
        assert_eq!(ch.access.destination, "nozdormu-logs.access");
        assert_eq!(ch.client_to_cdn.destination, "nozdormu-logs.client_to_cdn");
        assert_eq!(ch.waf.destination, "nozdormu-logs.waf");
    }

    #[test]
    fn test_channels_partial_override() {
        let json = r#"{"waf":{"enabled":false,"destination":"custom.waf"}}"#;
        let ch: LogChannelsConfig = serde_json::from_str(json).unwrap();
        assert!(!ch.waf.enabled);
        assert_eq!(ch.waf.destination, "custom.waf");
        // Others keep defaults
        assert!(ch.access.enabled);
        assert_eq!(ch.access.destination, "nozdormu-logs.access");
    }

    #[test]
    fn test_channels_all_disabled() {
        let json = r#"{
            "client_to_cdn":{"enabled":false,"destination":"a"},
            "cdn_to_origin":{"enabled":false,"destination":"b"},
            "origin_to_cdn":{"enabled":false,"destination":"c"},
            "cdn_to_client":{"enabled":false,"destination":"d"},
            "waf":{"enabled":false,"destination":"e"},
            "cc":{"enabled":false,"destination":"f"},
            "cache":{"enabled":false,"destination":"g"},
            "access":{"enabled":false,"destination":"h"}
        }"#;
        let ch: LogChannelsConfig = serde_json::from_str(json).unwrap();
        assert!(!ch.client_to_cdn.enabled);
        assert!(!ch.access.enabled);
    }

    #[test]
    fn test_kafka_with_channels() {
        let json = r#"{
            "type":"kafka","brokers":["k1:9092"],"topic":"logs",
            "channels":{"waf":{"enabled":false,"destination":"waf-topic"}}
        }"#;
        let cfg: LogBackendConfig = serde_json::from_str(json).unwrap();
        match cfg {
            LogBackendConfig::Kafka(k) => {
                assert!(!k.channels.waf.enabled);
                assert_eq!(k.channels.waf.destination, "waf-topic");
                assert!(k.channels.access.enabled);
            }
            _ => panic!("expected Kafka"),
        }
    }

    #[test]
    fn test_redis_log_config_defaults() {
        let cfg = RedisLogConfig::default();
        assert_eq!(cfg.stream_key, "nozdormu:log:requests");
        assert_eq!(cfg.max_len, 100_000);
        assert!(cfg.channels.access.enabled);
    }

    #[test]
    fn test_kafka_log_config_serde() {
        let json = r#"{"type":"kafka","brokers":["kafka1:9092","kafka2:9092"],"topic":"my-logs","acks":"all"}"#;
        let cfg: LogBackendConfig = serde_json::from_str(json).unwrap();
        match cfg {
            LogBackendConfig::Kafka(k) => {
                assert_eq!(k.brokers.len(), 2);
                assert_eq!(k.topic, "my-logs");
                assert_eq!(k.acks, "all");
            }
            _ => panic!("expected Kafka"),
        }
    }

    #[test]
    fn test_rabbitmq_log_config_serde() {
        let json = r#"{"type":"rabbitmq","urls":["amqp://user:pass@rabbit1:5672/%2f"],"exchange":"logs","routing_key":"req"}"#;
        let cfg: LogBackendConfig = serde_json::from_str(json).unwrap();
        match cfg {
            LogBackendConfig::RabbitMQ(r) => {
                assert_eq!(r.urls.len(), 1);
                assert_eq!(r.exchange, "logs");
                assert!(r.durable);
            }
            _ => panic!("expected RabbitMQ"),
        }
    }

    #[test]
    fn test_nats_log_config_serde() {
        let json = r#"{"type":"nats","urls":["nats://n1:4222"],"subject":"cdn.logs","stream_name":"CDN_LOGS"}"#;
        let cfg: LogBackendConfig = serde_json::from_str(json).unwrap();
        match cfg {
            LogBackendConfig::Nats(n) => {
                assert_eq!(n.subject, "cdn.logs");
                assert_eq!(n.stream_name.as_deref(), Some("CDN_LOGS"));
            }
            _ => panic!("expected NATS"),
        }
    }

    #[test]
    fn test_pulsar_log_config_serde() {
        let json = r#"{"type":"pulsar","urls":["pulsar://p1:6650"],"topic":"persistent://public/default/logs"}"#;
        let cfg: LogBackendConfig = serde_json::from_str(json).unwrap();
        match cfg {
            LogBackendConfig::Pulsar(p) => {
                assert_eq!(p.topic, "persistent://public/default/logs");
            }
            _ => panic!("expected Pulsar"),
        }
    }

    #[test]
    fn test_redis_backend_config_defaults() {
        let json = r#"{"type":"redis"}"#;
        let cfg: LogBackendConfig = serde_json::from_str(json).unwrap();
        match cfg {
            LogBackendConfig::Redis(r) => {
                assert_eq!(r.stream_key, "nozdormu:log:requests");
                assert_eq!(r.max_len, 100_000);
            }
            _ => panic!("expected Redis"),
        }
    }

    #[test]
    fn test_kafka_debug_redacts_password() {
        let cfg = KafkaLogConfig {
            sasl_password: Some("secret123".to_string()),
            ..KafkaLogConfig::default()
        };
        let debug = format!("{:?}", cfg);
        assert!(!debug.contains("secret123"));
        assert!(debug.contains("REDACTED"));
    }

    #[test]
    fn test_backend_channels_accessor() {
        let json = r#"{"type":"redis","channels":{"waf":{"enabled":false,"destination":"x"}}}"#;
        let cfg: LogBackendConfig = serde_json::from_str(json).unwrap();
        assert!(!cfg.channels().waf.enabled);
        assert_eq!(cfg.channels().waf.destination, "x");
    }

    #[test]
    fn test_backend_config_roundtrip() {
        let configs = vec![
            serde_json::json!({"type": "redis", "stream_key": "test", "max_len": 1000}),
            serde_json::json!({"type": "kafka", "brokers": ["b1:9092"], "topic": "t"}),
            serde_json::json!({"type": "rabbitmq", "urls": ["amqp://localhost"], "exchange": "e"}),
            serde_json::json!({"type": "nats", "urls": ["nats://localhost"], "subject": "s"}),
            serde_json::json!({"type": "pulsar", "urls": ["pulsar://localhost"], "topic": "t"}),
        ];
        for cfg_json in configs {
            let cfg: LogBackendConfig = serde_json::from_value(cfg_json.clone()).unwrap();
            let reserialized = serde_json::to_value(&cfg).unwrap();
            assert_eq!(reserialized["type"], cfg_json["type"]);
        }
    }
}
