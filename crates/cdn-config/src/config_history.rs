use crate::node_config::EtcdConfig;
use serde::{Deserialize, Serialize};

/// Maximum versions retained per site. Older versions are pruned on write.
const MAX_VERSIONS_PER_SITE: u64 = 50;

/// Maximum CAS retry attempts for atomic version counter increment.
const MAX_CAS_RETRIES: u32 = 5;

// ── Data structures ──

/// The type of change that produced this version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigChangeType {
    Created,
    Updated,
    Deleted,
    Rollback,
}

/// A full versioned snapshot of a site config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigVersionSnapshot {
    pub version: u64,
    pub site_id: String,
    pub timestamp: String,
    pub etcd_revision: i64,
    pub change_type: ConfigChangeType,
    /// Raw SiteConfig JSON — stored as Value to avoid schema evolution issues.
    pub config: serde_json::Value,
}

/// Per-site version counter, stored at the `meta` key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionMeta {
    pub latest_version: u64,
}

/// Summary entry returned in history listings (without the full config body).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigVersionSummary {
    pub version: u64,
    pub site_id: String,
    pub timestamp: String,
    pub etcd_revision: i64,
    pub change_type: ConfigChangeType,
}

impl ConfigVersionSnapshot {
    pub fn to_summary(&self) -> ConfigVersionSummary {
        ConfigVersionSummary {
            version: self.version,
            site_id: self.site_id.clone(),
            timestamp: self.timestamp.clone(),
            etcd_revision: self.etcd_revision,
            change_type: self.change_type.clone(),
        }
    }
}

// ── Key helpers ──

fn meta_key(prefix: &str, site_id: &str) -> String {
    format!("{}/config_history/{}/meta", prefix, site_id)
}

pub fn version_key(prefix: &str, site_id: &str, version: u64) -> String {
    format!("{}/config_history/{}/v/{:010}", prefix, site_id, version)
}

fn version_prefix(prefix: &str, site_id: &str) -> String {
    format!("{}/config_history/{}/v/", prefix, site_id)
}

// ── etcd helpers ──

async fn connect(
    etcd_config: &EtcdConfig,
) -> Result<etcd_client::Client, Box<dyn std::error::Error + Send + Sync>> {
    let endpoints: Vec<&str> = etcd_config.endpoints.iter().map(|s| s.as_str()).collect();
    let client = etcd_client::Client::connect(&endpoints, None).await?;
    Ok(client)
}

// ── Public API ──

/// Save a versioned snapshot after a site config change.
///
/// Uses etcd Txn (CAS on mod_revision) to atomically increment the per-site
/// version counter. Retries up to `MAX_CAS_RETRIES` times on conflict.
/// Prunes old versions if count exceeds `MAX_VERSIONS_PER_SITE`.
pub async fn save_version(
    etcd_config: &EtcdConfig,
    site_id: &str,
    config_json: &[u8],
    etcd_revision: i64,
    change_type: ConfigChangeType,
) -> Result<u64, Box<dyn std::error::Error + Send + Sync>> {
    let mut client = connect(etcd_config).await?;
    let mk = meta_key(&etcd_config.prefix, site_id);

    let config_value: serde_json::Value = serde_json::from_slice(config_json)
        .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));

    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    for attempt in 0..MAX_CAS_RETRIES {
        // 1. Read current meta
        let resp = client.get(mk.as_bytes(), None).await?;
        let (new_version, compare) = if let Some(kv) = resp.kvs().first() {
            let meta: VersionMeta = serde_json::from_slice(kv.value())?;
            let mod_rev = kv.mod_revision();
            (
                meta.latest_version + 1,
                etcd_client::Compare::mod_revision(
                    mk.clone(),
                    etcd_client::CompareOp::Equal,
                    mod_rev,
                ),
            )
        } else {
            // Key does not exist — compare version == 0 (key absent)
            (
                1,
                etcd_client::Compare::version(mk.clone(), etcd_client::CompareOp::Equal, 0),
            )
        };

        // 2. Build snapshot
        let snapshot = ConfigVersionSnapshot {
            version: new_version,
            site_id: site_id.to_string(),
            timestamp: timestamp.clone(),
            etcd_revision,
            change_type: change_type.clone(),
            config: config_value.clone(),
        };

        let new_meta = VersionMeta {
            latest_version: new_version,
        };
        let meta_json = serde_json::to_vec(&new_meta)?;
        let snapshot_json = serde_json::to_vec(&snapshot)?;
        let vk = version_key(&etcd_config.prefix, site_id, new_version);

        // 3. CAS transaction: compare → put meta + put snapshot
        let txn = etcd_client::Txn::new().when(vec![compare]).and_then(vec![
            etcd_client::TxnOp::put(mk.clone(), meta_json, None),
            etcd_client::TxnOp::put(vk, snapshot_json, None),
        ]);

        let txn_resp = client.txn(txn).await?;

        if txn_resp.succeeded() {
            log::info!(
                "[config_history] saved version {} for site '{}' (rev={})",
                new_version,
                site_id,
                etcd_revision
            );

            // 4. Prune old versions if needed
            if new_version > MAX_VERSIONS_PER_SITE {
                if let Err(e) =
                    prune_old_versions(&mut client, &etcd_config.prefix, site_id, new_version).await
                {
                    log::warn!(
                        "[config_history] failed to prune old versions for '{}': {}",
                        site_id,
                        e
                    );
                }
            }

            return Ok(new_version);
        }

        // CAS failed — another node incremented first, retry
        log::debug!(
            "[config_history] CAS conflict for site '{}', attempt {}/{}",
            site_id,
            attempt + 1,
            MAX_CAS_RETRIES
        );
    }

    Err(format!(
        "CAS conflict: failed to save version for site '{}' after {} attempts",
        site_id, MAX_CAS_RETRIES
    )
    .into())
}

/// List version summaries for a site (newest first, no config body).
pub async fn list_versions(
    etcd_config: &EtcdConfig,
    site_id: &str,
) -> Result<Vec<ConfigVersionSummary>, Box<dyn std::error::Error + Send + Sync>> {
    let mut client = connect(etcd_config).await?;
    let prefix = version_prefix(&etcd_config.prefix, site_id);
    let opts = etcd_client::GetOptions::new().with_prefix();
    let resp = client.get(prefix.as_bytes(), Some(opts)).await?;

    let mut summaries: Vec<ConfigVersionSummary> = Vec::new();
    for kv in resp.kvs() {
        match serde_json::from_slice::<ConfigVersionSnapshot>(kv.value()) {
            Ok(snapshot) => summaries.push(snapshot.to_summary()),
            Err(e) => {
                let key = String::from_utf8_lossy(kv.key());
                log::warn!(
                    "[config_history] failed to parse snapshot at '{}': {}",
                    key,
                    e
                );
            }
        }
    }

    // Sort newest first
    summaries.sort_by(|a, b| b.version.cmp(&a.version));
    Ok(summaries)
}

/// Get a specific version snapshot (with full config body).
pub async fn get_version(
    etcd_config: &EtcdConfig,
    site_id: &str,
    version: u64,
) -> Result<Option<ConfigVersionSnapshot>, Box<dyn std::error::Error + Send + Sync>> {
    let mut client = connect(etcd_config).await?;
    let vk = version_key(&etcd_config.prefix, site_id, version);
    let resp = client.get(vk.as_bytes(), None).await?;

    match resp.kvs().first() {
        Some(kv) => {
            let snapshot: ConfigVersionSnapshot = serde_json::from_slice(kv.value())?;
            Ok(Some(snapshot))
        }
        None => Ok(None),
    }
}

/// Rollback: read a historical version and write its config back to
/// `{prefix}/sites/{site_id}`. Also saves a new version snapshot with
/// `change_type = Rollback`.
///
/// Returns `(new_version, put_revision)` on success. The caller can use
/// `put_revision` to set a pending change type on the watcher so the
/// resulting watch event is tagged as `Rollback` instead of `Updated`.
pub async fn rollback_to_version(
    etcd_config: &EtcdConfig,
    site_id: &str,
    version: u64,
) -> Result<(u64, i64), Box<dyn std::error::Error + Send + Sync>> {
    let mut client = connect(etcd_config).await?;

    // 1. Read the historical snapshot
    let vk = version_key(&etcd_config.prefix, site_id, version);
    let resp = client.get(vk.as_bytes(), None).await?;
    let snapshot = match resp.kvs().first() {
        Some(kv) => serde_json::from_slice::<ConfigVersionSnapshot>(kv.value())?,
        None => return Err(format!("version {} not found for site '{}'", version, site_id).into()),
    };

    // 2. Write the config back to the live site key
    let site_key = format!("{}/sites/{}", etcd_config.prefix, site_id);
    let config_bytes = serde_json::to_vec(&snapshot.config)?;
    let put_resp = client.put(site_key, config_bytes.clone(), None).await?;
    let put_revision = put_resp.header().map(|h| h.revision()).unwrap_or(0);

    // 3. Save a new version snapshot with Rollback type
    // Use the raw config bytes from the snapshot
    let new_version = save_version(
        etcd_config,
        site_id,
        &config_bytes,
        put_revision,
        ConfigChangeType::Rollback,
    )
    .await?;

    log::info!(
        "[config_history] rolled back site '{}' to version {}, new version {}",
        site_id,
        version,
        new_version
    );

    Ok((new_version, put_revision))
}

/// Prune versions older than `MAX_VERSIONS_PER_SITE` for a given site.
async fn prune_old_versions(
    client: &mut etcd_client::Client,
    prefix: &str,
    site_id: &str,
    current_version: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cutoff = current_version - MAX_VERSIONS_PER_SITE;

    // Range delete: from v/0000000001 to v/{cutoff+1:010} (exclusive end)
    let range_start = version_key(prefix, site_id, 1);
    let range_end = version_key(prefix, site_id, cutoff + 1);

    let opts = etcd_client::DeleteOptions::new().with_range(range_end);
    let resp = client.delete(range_start.as_bytes(), Some(opts)).await?;

    let deleted = resp.deleted();
    if deleted > 0 {
        log::info!(
            "[config_history] pruned {} old versions for site '{}' (kept versions {}-{})",
            deleted,
            site_id,
            cutoff + 1,
            current_version
        );
    }

    Ok(())
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_change_type_serde() {
        assert_eq!(
            serde_json::to_string(&ConfigChangeType::Created).unwrap(),
            "\"created\""
        );
        assert_eq!(
            serde_json::to_string(&ConfigChangeType::Updated).unwrap(),
            "\"updated\""
        );
        assert_eq!(
            serde_json::to_string(&ConfigChangeType::Deleted).unwrap(),
            "\"deleted\""
        );
        assert_eq!(
            serde_json::to_string(&ConfigChangeType::Rollback).unwrap(),
            "\"rollback\""
        );

        let ct: ConfigChangeType = serde_json::from_str("\"rollback\"").unwrap();
        assert_eq!(ct, ConfigChangeType::Rollback);
    }

    #[test]
    fn test_version_snapshot_serde_roundtrip() {
        let snapshot = ConfigVersionSnapshot {
            version: 42,
            site_id: "my-site".to_string(),
            timestamp: "2026-04-11T10:30:00Z".to_string(),
            etcd_revision: 1234,
            change_type: ConfigChangeType::Updated,
            config: serde_json::json!({"site_id": "my-site", "enabled": true}),
        };

        let json = serde_json::to_string(&snapshot).unwrap();
        let parsed: ConfigVersionSnapshot = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.version, 42);
        assert_eq!(parsed.site_id, "my-site");
        assert_eq!(parsed.timestamp, "2026-04-11T10:30:00Z");
        assert_eq!(parsed.etcd_revision, 1234);
        assert_eq!(parsed.change_type, ConfigChangeType::Updated);
        assert_eq!(parsed.config["site_id"], "my-site");
        assert_eq!(parsed.config["enabled"], true);
    }

    #[test]
    fn test_version_meta_serde() {
        let meta = VersionMeta { latest_version: 10 };
        let json = serde_json::to_string(&meta).unwrap();
        assert_eq!(json, r#"{"latest_version":10}"#);

        let parsed: VersionMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.latest_version, 10);
    }

    #[test]
    fn test_version_key_formatting() {
        assert_eq!(
            version_key("/nozdormu", "site-1", 1),
            "/nozdormu/config_history/site-1/v/0000000001"
        );
        assert_eq!(
            version_key("/nozdormu", "site-1", 42),
            "/nozdormu/config_history/site-1/v/0000000042"
        );
        assert_eq!(
            version_key("/nozdormu", "my-site", 9999999999),
            "/nozdormu/config_history/my-site/v/9999999999"
        );
    }

    #[test]
    fn test_meta_key_formatting() {
        assert_eq!(
            meta_key("/nozdormu", "site-1"),
            "/nozdormu/config_history/site-1/meta"
        );
    }

    #[test]
    fn test_version_prefix_formatting() {
        assert_eq!(
            version_prefix("/nozdormu", "site-1"),
            "/nozdormu/config_history/site-1/v/"
        );
    }

    #[test]
    fn test_snapshot_to_summary() {
        let snapshot = ConfigVersionSnapshot {
            version: 5,
            site_id: "test".to_string(),
            timestamp: "2026-04-11T00:00:00Z".to_string(),
            etcd_revision: 100,
            change_type: ConfigChangeType::Created,
            config: serde_json::json!({"big": "config"}),
        };

        let summary = snapshot.to_summary();
        assert_eq!(summary.version, 5);
        assert_eq!(summary.site_id, "test");
        assert_eq!(summary.timestamp, "2026-04-11T00:00:00Z");
        assert_eq!(summary.etcd_revision, 100);
        assert_eq!(summary.change_type, ConfigChangeType::Created);
    }

    #[test]
    fn test_config_change_type_equality() {
        assert_eq!(ConfigChangeType::Created, ConfigChangeType::Created);
        assert_ne!(ConfigChangeType::Created, ConfigChangeType::Updated);
        assert_ne!(ConfigChangeType::Deleted, ConfigChangeType::Rollback);
    }
}
