use async_trait::async_trait;
use cdn_common::RedisOps;
use cdn_config::NodeConfig;
use redis::AsyncCommands;

/// Redis connection pool with graceful degradation.
///
/// Wraps `redis::aio::ConnectionManager` which handles automatic reconnection.
/// When Redis is unavailable, all operations return no-op results.
pub struct RedisPool {
    conn: Option<redis::aio::ConnectionManager>,
    description: String,
}

impl RedisPool {
    /// Connect to Redis using NodeConfig settings.
    /// Returns a pool with `conn: None` if connection fails (graceful degradation).
    pub async fn connect(config: &NodeConfig) -> Self {
        match config.redis.mode.as_str() {
            "standalone" => Self::connect_standalone(config).await,
            "sentinel" => Self::connect_sentinel(config).await,
            _ => {
                log::warn!("[Redis] unknown mode '{}', running without Redis", config.redis.mode);
                Self::none()
            }
        }
    }

    /// Create a pool with no connection (offline mode).
    pub fn none() -> Self {
        Self {
            conn: None,
            description: "none".to_string(),
        }
    }

    pub fn is_available(&self) -> bool {
        self.conn.is_some()
    }

    pub fn describe(&self) -> &str {
        &self.description
    }

    /// PING Redis to check connectivity.
    pub async fn ping(&self) -> bool {
        let Some(mut conn) = self.conn.clone() else {
            return false;
        };
        redis::cmd("PING")
            .query_async::<String>(&mut conn)
            .await
            .is_ok()
    }

    /// Execute a Lua script (for distributed locks).
    pub async fn eval_script(
        &self,
        script: &str,
        keys: &[&str],
        args: &[&str],
    ) -> Result<redis::Value, String> {
        let Some(mut conn) = self.conn.clone() else {
            return Err("Redis not available".to_string());
        };
        let script = redis::Script::new(script);
        let mut invocation = script.prepare_invoke();
        for k in keys {
            invocation.key(*k);
        }
        for a in args {
            invocation.arg(*a);
        }
        invocation
            .invoke_async(&mut conn)
            .await
            .map_err(|e| e.to_string())
    }

    /// XADD to a Redis Stream with approximate MAXLEN trimming.
    pub async fn xadd(
        &self,
        stream: &str,
        maxlen: u64,
        field: &str,
        value: &str,
    ) -> Result<(), String> {
        let Some(mut conn) = self.conn.clone() else {
            return Err("Redis not available".to_string());
        };
        redis::cmd("XADD")
            .arg(stream)
            .arg("MAXLEN")
            .arg("~")
            .arg(maxlen)
            .arg("*")
            .arg(field)
            .arg(value)
            .query_async::<String>(&mut conn)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// SET key value NX EX ttl (for distributed locks).
    /// Returns true if the key was set (lock acquired).
    pub async fn set_nx_ex(&self, key: &str, value: &str, ttl_secs: u64) -> bool {
        let Some(mut conn) = self.conn.clone() else {
            return false;
        };
        let result: Result<Option<String>, _> = redis::cmd("SET")
            .arg(key)
            .arg(value)
            .arg("NX")
            .arg("EX")
            .arg(ttl_secs)
            .query_async(&mut conn)
            .await;
        matches!(result, Ok(Some(_)))
    }

    /// SCAN keys matching a pattern. Returns all matching keys.
    /// Uses cursor-based iteration (safe for production, non-blocking).
    /// `count` is a hint for how many keys to return per iteration.
    /// Safety cap at 1,000,000 keys to prevent OOM.
    pub async fn scan_keys(&self, pattern: &str, count: usize) -> Result<Vec<String>, String> {
        let Some(mut conn) = self.conn.clone() else {
            return Err("Redis not available".to_string());
        };

        const MAX_KEYS: usize = 1_000_000;
        let mut all_keys = Vec::new();
        let mut cursor: u64 = 0;

        loop {
            let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(pattern)
                .arg("COUNT")
                .arg(count)
                .query_async(&mut conn)
                .await
                .map_err(|e| format!("SCAN failed: {}", e))?;

            all_keys.extend(keys);

            if all_keys.len() >= MAX_KEYS {
                log::warn!(
                    "[Redis] SCAN hit safety cap ({} keys) for pattern: {}",
                    MAX_KEYS, pattern
                );
                break;
            }

            cursor = next_cursor;
            if cursor == 0 {
                break;
            }
        }

        Ok(all_keys)
    }

    async fn connect_standalone(config: &NodeConfig) -> Self {
        let url = if let Some(ref pw) = config.redis.password {
            format!(
                "redis://:{}@{}:{}/{}",
                pw, config.redis.standalone.host, config.redis.standalone.port, config.redis.db
            )
        } else {
            format!(
                "redis://{}:{}/{}",
                config.redis.standalone.host, config.redis.standalone.port, config.redis.db
            )
        };

        let description = if url.contains('@') {
            format!("standalone:redis://***@{}", url.split('@').last().unwrap_or(""))
        } else {
            format!("standalone:{}", url)
        };

        match redis::Client::open(url.as_str()) {
            Ok(client) => match client.get_connection_manager().await {
                Ok(conn) => {
                    log::info!("[Redis] connected: {}", description);
                    Self { conn: Some(conn), description }
                }
                Err(e) => {
                    log::warn!("[Redis] connection failed ({}): {}", description, e);
                    Self { conn: None, description }
                }
            },
            Err(e) => {
                log::warn!("[Redis] client creation failed: {}", e);
                Self { conn: None, description }
            }
        }
    }

    async fn connect_sentinel(config: &NodeConfig) -> Self {
        let sentinels = &config.redis.sentinel.nodes;
        let master_name = &config.redis.sentinel.master_name;
        let description = format!("sentinel:{} ({})", master_name, sentinels.join(","));

        if sentinels.is_empty() {
            log::warn!("[Redis] sentinel mode but no nodes configured");
            return Self { conn: None, description };
        }

        // Build sentinel connection info
        let node_conn_info = redis::sentinel::SentinelNodeConnectionInfo {
            tls_mode: None,
            redis_connection_info: Some(redis::RedisConnectionInfo {
                db: config.redis.db as i64,
                username: None,
                password: config.redis.password.clone(),
                protocol: redis::ProtocolVersion::RESP2,
            }),
        };

        // Parse sentinel addresses as connection strings
        let sentinel_urls: Vec<String> = sentinels
            .iter()
            .map(|s| format!("redis://{}", s))
            .collect();

        match redis::sentinel::SentinelClient::build(
            sentinel_urls,
            master_name.to_string(),
            Some(node_conn_info),
            redis::sentinel::SentinelServerType::Master,
        ) {
            Ok(mut client) => {
                // SentinelClient discovers master and returns a connection
                match client.get_async_connection().await {
                    Ok(_async_conn) => {
                        // We got a connection, but we need a ConnectionManager for auto-reconnect.
                        // Use the sentinel to discover master address, then connect directly.
                        log::info!("[Redis] sentinel discovered master, connecting via ConnectionManager");

                        // Discover master address via a fresh sentinel query
                        let sentinel_url = format!("redis://{}", sentinels[0]);
                        if let Ok(sentinel_client) = redis::Client::open(sentinel_url.as_str()) {
                            if let Ok(mut sentinel_conn) = sentinel_client.get_multiplexed_async_connection().await {
                                let result: Result<(String, u16), _> = redis::cmd("SENTINEL")
                                    .arg("get-master-addr-by-name")
                                    .arg(master_name)
                                    .query_async(&mut sentinel_conn)
                                    .await;

                                if let Ok((host, port)) = result {
                                    let master_url = if let Some(ref pw) = config.redis.password {
                                        format!("redis://:{}@{}:{}/{}", pw, host, port, config.redis.db)
                                    } else {
                                        format!("redis://{}:{}/{}", host, port, config.redis.db)
                                    };

                                    if let Ok(master_client) = redis::Client::open(master_url.as_str()) {
                                        if let Ok(conn) = master_client.get_connection_manager().await {
                                            log::info!("[Redis] sentinel connected to master {}:{}", host, port);
                                            return Self { conn: Some(conn), description };
                                        }
                                    }
                                }
                            }
                        }

                        log::warn!("[Redis] sentinel master discovery failed, running without Redis");
                        Self { conn: None, description }
                    }
                    Err(e) => {
                        log::warn!("[Redis] sentinel connection failed: {}", e);
                        Self { conn: None, description }
                    }
                }
            }
            Err(e) => {
                log::warn!("[Redis] sentinel client build failed: {}", e);
                Self { conn: None, description }
            }
        }
    }
}

#[async_trait]
impl RedisOps for RedisPool {
    async fn get(&self, key: &str) -> Option<String> {
        let mut conn = self.conn.clone()?;
        match conn.get::<_, Option<String>>(key).await {
            Ok(val) => val,
            Err(e) => {
                log::warn!("[Redis] GET {} failed: {}", key, e);
                None
            }
        }
    }

    async fn setex(&self, key: &str, seconds: u64, value: &str) -> Result<(), String> {
        let Some(mut conn) = self.conn.clone() else {
            return Ok(()); // graceful degradation
        };
        conn.set_ex::<_, _, ()>(key, value, seconds)
            .await
            .map_err(|e| e.to_string())
    }

    async fn del(&self, key: &str) -> Result<(), String> {
        let Some(mut conn) = self.conn.clone() else {
            return Ok(());
        };
        conn.del::<_, ()>(key)
            .await
            .map_err(|e| e.to_string())
    }

    async fn incr_by(&self, key: &str, delta: u64) -> Result<u64, String> {
        let Some(mut conn) = self.conn.clone() else {
            return Ok(0);
        };
        conn.incr::<_, _, u64>(key, delta)
            .await
            .map_err(|e| e.to_string())
    }

    async fn incr_by_ex(&self, key: &str, delta: u64, expire_secs: u64) -> Result<u64, String> {
        let Some(mut conn) = self.conn.clone() else {
            return Ok(0);
        };
        // Lua script: INCRBY + EXPIRE atomically so counters don't leak
        let script = redis::Script::new(r#"
            local val = redis.call("INCRBY", KEYS[1], ARGV[1])
            redis.call("EXPIRE", KEYS[1], ARGV[2])
            return val
        "#);
        let mut invocation = script.prepare_invoke();
        invocation.key(key);
        invocation.arg(delta);
        invocation.arg(expire_secs);
        invocation
            .invoke_async::<u64>(&mut conn)
            .await
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_none_pool() {
        let pool = RedisPool::none();
        assert!(!pool.is_available());
        assert_eq!(pool.describe(), "none");
    }

    #[tokio::test]
    async fn test_none_pool_graceful_ops() {
        let pool = RedisPool::none();
        assert!(!pool.ping().await);
        assert_eq!(RedisOps::get(&pool, "key").await, None);
        assert!(RedisOps::setex(&pool, "key", 60, "val").await.is_ok());
        assert!(RedisOps::del(&pool, "key").await.is_ok());
        assert_eq!(RedisOps::incr_by(&pool, "key", 1).await.unwrap(), 0);
        assert_eq!(RedisOps::incr_by_ex(&pool, "key", 1, 60).await.unwrap(), 0);
        assert!(!pool.set_nx_ex("key", "val", 60).await);
    }

    #[tokio::test]
    async fn test_none_pool_scan_returns_error() {
        let pool = RedisPool::none();
        let result = pool.scan_keys("nozdormu:cache:meta:*", 100).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Redis not available");
    }
}
