use chrono::Utc;
use ring::hmac;
use std::collections::BTreeMap;

/// S3-compatible object storage client with AWS Signature V4.
pub struct OssClient {
    endpoint: String,
    bucket: String,
    region: String,
    access_key_id: String,
    secret_access_key: String,
    use_ssl: bool,
    path_style: bool,
    http_client: reqwest::Client,
}

impl OssClient {
    pub fn new(
        endpoint: &str,
        bucket: &str,
        region: &str,
        access_key_id: &str,
        secret_access_key: &str,
        use_ssl: bool,
        path_style: bool,
    ) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            bucket: bucket.to_string(),
            region: region.to_string(),
            access_key_id: access_key_id.to_string(),
            secret_access_key: secret_access_key.to_string(),
            use_ssl,
            path_style,
            http_client: reqwest::Client::new(),
        }
    }

    /// PUT an object.
    pub async fn put_object(&self, key: &str, body: Vec<u8>, content_type: &str) -> Result<(), OssError> {
        let url = self.object_url(key);
        let now = Utc::now();
        let date_str = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_short = now.format("%Y%m%d").to_string();

        // Offload SHA-256 to blocking thread pool for large bodies (> 1MB)
        let content_hash = if body.len() > 1_048_576 {
            let body_ref = body.clone();
            tokio::task::spawn_blocking(move || sha256_hex(&body_ref))
                .await
                .map_err(|e| OssError::Network(format!("hash task failed: {}", e)))?
        } else {
            sha256_hex(&body)
        };
        let host = self.host();

        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), host.clone());
        headers.insert("x-amz-date".to_string(), date_str.clone());
        headers.insert("x-amz-content-sha256".to_string(), content_hash.clone());
        headers.insert("content-type".to_string(), content_type.to_string());
        headers.insert("content-length".to_string(), body.len().to_string());

        let canonical_uri = self.canonical_uri(key);
        let authorization = self.sign_v4(
            "PUT", &canonical_uri, "", &headers, &content_hash, &date_str, &date_short,
        );

        let resp = self.http_client
            .put(&url)
            .header("Host", &host)
            .header("X-Amz-Date", &date_str)
            .header("X-Amz-Content-Sha256", &content_hash)
            .header("Content-Type", content_type)
            .header("Authorization", &authorization)
            .body(body)
            .send()
            .await
            .map_err(|e| OssError::Network(e.to_string()))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            Err(OssError::Api(status, body))
        }
    }

    /// GET an object.
    pub async fn get_object(&self, key: &str) -> Result<Vec<u8>, OssError> {
        let url = self.object_url(key);
        let now = Utc::now();
        let date_str = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_short = now.format("%Y%m%d").to_string();

        let content_hash = sha256_hex(b""); // empty body for GET
        let host = self.host();

        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), host.clone());
        headers.insert("x-amz-date".to_string(), date_str.clone());
        headers.insert("x-amz-content-sha256".to_string(), content_hash.clone());

        let canonical_uri = self.canonical_uri(key);
        let authorization = self.sign_v4(
            "GET", &canonical_uri, "", &headers, &content_hash, &date_str, &date_short,
        );

        let resp = self.http_client
            .get(&url)
            .header("Host", &host)
            .header("X-Amz-Date", &date_str)
            .header("X-Amz-Content-Sha256", &content_hash)
            .header("Authorization", &authorization)
            .send()
            .await
            .map_err(|e| OssError::Network(e.to_string()))?;

        if resp.status().is_success() {
            resp.bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| OssError::Network(e.to_string()))
        } else if resp.status().as_u16() == 404 {
            Err(OssError::NotFound)
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            Err(OssError::Api(status, body))
        }
    }

    /// DELETE an object.
    pub async fn delete_object(&self, key: &str) -> Result<(), OssError> {
        let url = self.object_url(key);
        let now = Utc::now();
        let date_str = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_short = now.format("%Y%m%d").to_string();

        let content_hash = sha256_hex(b"");
        let host = self.host();

        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), host.clone());
        headers.insert("x-amz-date".to_string(), date_str.clone());
        headers.insert("x-amz-content-sha256".to_string(), content_hash.clone());

        let canonical_uri = self.canonical_uri(key);
        let authorization = self.sign_v4(
            "DELETE", &canonical_uri, "", &headers, &content_hash, &date_str, &date_short,
        );

        let resp = self.http_client
            .delete(&url)
            .header("Host", &host)
            .header("X-Amz-Date", &date_str)
            .header("X-Amz-Content-Sha256", &content_hash)
            .header("Authorization", &authorization)
            .send()
            .await
            .map_err(|e| OssError::Network(e.to_string()))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            Err(OssError::Api(status, body))
        }
    }

    // ── AWS Signature V4 ──

    fn sign_v4(
        &self,
        method: &str,
        canonical_uri: &str,
        canonical_query: &str,
        headers: &BTreeMap<String, String>,
        content_hash: &str,
        date_str: &str,
        date_short: &str,
    ) -> String {
        let signed_headers: Vec<&str> = headers.keys().map(|k| k.as_str()).collect();
        let signed_headers_str = signed_headers.join(";");

        let canonical_headers: String = headers
            .iter()
            .map(|(k, v)| format!("{}:{}\n", k, v.trim()))
            .collect();

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method, canonical_uri, canonical_query, canonical_headers, signed_headers_str, content_hash
        );

        let scope = format!("{}/{}/s3/aws4_request", date_short, self.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            date_str, scope, sha256_hex(canonical_request.as_bytes())
        );

        // Derive signing key
        let k_date = hmac_sha256(
            format!("AWS4{}", self.secret_access_key).as_bytes(),
            date_short.as_bytes(),
        );
        let k_region = hmac_sha256(&k_date, self.region.as_bytes());
        let k_service = hmac_sha256(&k_region, b"s3");
        let k_signing = hmac_sha256(&k_service, b"aws4_request");

        let signature = cdn_common::hex_encode(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));

        format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            self.access_key_id, scope, signed_headers_str, signature
        )
    }

    fn host(&self) -> String {
        let endpoint = self.endpoint.trim_end_matches('/');
        // Strip scheme for Host header
        let host = endpoint
            .strip_prefix("https://")
            .or_else(|| endpoint.strip_prefix("http://"))
            .unwrap_or(endpoint);
        if self.path_style {
            host.to_string()
        } else {
            format!("{}.{}", self.bucket, host)
        }
    }

    fn object_url(&self, key: &str) -> String {
        let scheme = if self.use_ssl { "https" } else { "http" };
        let endpoint = self.endpoint.trim_end_matches('/');
        let host = endpoint
            .strip_prefix("https://")
            .or_else(|| endpoint.strip_prefix("http://"))
            .unwrap_or(endpoint);
        let encoded_key = uri_encode_path(key);

        if self.path_style {
            format!("{}://{}/{}/{}", scheme, host, self.bucket, encoded_key)
        } else {
            format!("{}://{}.{}/{}", scheme, self.bucket, host, encoded_key)
        }
    }

    fn canonical_uri(&self, key: &str) -> String {
        let encoded_key = uri_encode_path(key);
        if self.path_style {
            format!("/{}/{}", self.bucket, encoded_key)
        } else {
            format!("/{}", encoded_key)
        }
    }
}

#[derive(Debug)]
pub enum OssError {
    Network(String),
    Api(u16, String),
    NotFound,
}

impl std::fmt::Display for OssError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OssError::Network(e) => write!(f, "OSS network error: {}", e),
            OssError::Api(status, body) => write!(f, "OSS API error {}: {}", status, body),
            OssError::NotFound => write!(f, "OSS object not found"),
        }
    }
}

/// URI-encode a path for S3 requests (RFC 3986).
/// Encodes each path segment individually, preserving `/` separators.
fn uri_encode_path(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            segment
                .bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
                        format!("{}", b as char)
                    } else {
                        format!("%{:02X}", b)
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let k = hmac::Key::new(hmac::HMAC_SHA256, key);
    hmac::sign(&k, data).as_ref().to_vec()
}

fn sha256_hex(data: &[u8]) -> String {
    use ring::digest;
    let hash = digest::digest(&digest::SHA256, data);
    cdn_common::hex_encode(hash.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_style_url() {
        let client = OssClient::new(
            "https://s3.amazonaws.com", "mybucket", "us-east-1",
            "AKID", "SECRET", true, true,
        );
        assert_eq!(
            client.object_url("cache/site1/ab/abcdef"),
            "https://s3.amazonaws.com/mybucket/cache/site1/ab/abcdef"
        );
    }

    #[test]
    fn test_virtual_host_url() {
        let client = OssClient::new(
            "https://s3.amazonaws.com", "mybucket", "us-east-1",
            "AKID", "SECRET", true, false,
        );
        assert_eq!(
            client.object_url("cache/site1/ab/abcdef"),
            "https://mybucket.s3.amazonaws.com/cache/site1/ab/abcdef"
        );
    }

    #[test]
    fn test_host_path_style() {
        let client = OssClient::new(
            "https://s3.amazonaws.com", "mybucket", "us-east-1",
            "AKID", "SECRET", true, true,
        );
        assert_eq!(client.host(), "s3.amazonaws.com");
    }

    #[test]
    fn test_host_virtual() {
        let client = OssClient::new(
            "https://s3.amazonaws.com", "mybucket", "us-east-1",
            "AKID", "SECRET", true, false,
        );
        assert_eq!(client.host(), "mybucket.s3.amazonaws.com");
    }

    #[test]
    fn test_sha256_hex() {
        let hash = sha256_hex(b"");
        assert_eq!(hash, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }
}
