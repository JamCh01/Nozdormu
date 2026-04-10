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
    pub async fn put_object(
        &self,
        key: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<(), OssError> {
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
            "PUT",
            &canonical_uri,
            "",
            &headers,
            &content_hash,
            &date_str,
            &date_short,
        );

        let resp = self
            .http_client
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
            "GET",
            &canonical_uri,
            "",
            &headers,
            &content_hash,
            &date_str,
            &date_short,
        );

        let resp = self
            .http_client
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

    /// GET a byte range of an object.
    /// Returns the requested range bytes on success (HTTP 206 or 200).
    pub async fn get_object_range(
        &self,
        key: &str,
        start: u64,
        end: u64,
    ) -> Result<Vec<u8>, OssError> {
        let url = self.object_url(key);
        let now = Utc::now();
        let date_str = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_short = now.format("%Y%m%d").to_string();

        let content_hash = sha256_hex(b"");
        let host = self.host();
        let range_value = format!("bytes={}-{}", start, end);

        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), host.clone());
        headers.insert("range".to_string(), range_value.clone());
        headers.insert("x-amz-date".to_string(), date_str.clone());
        headers.insert("x-amz-content-sha256".to_string(), content_hash.clone());

        let canonical_uri = self.canonical_uri(key);
        let authorization = self.sign_v4(
            "GET",
            &canonical_uri,
            "",
            &headers,
            &content_hash,
            &date_str,
            &date_short,
        );

        let resp = self
            .http_client
            .get(&url)
            .header("Host", &host)
            .header("Range", &range_value)
            .header("X-Amz-Date", &date_str)
            .header("X-Amz-Content-Sha256", &content_hash)
            .header("Authorization", &authorization)
            .send()
            .await
            .map_err(|e| OssError::Network(e.to_string()))?;

        let status = resp.status().as_u16();
        if status == 206 || status == 200 {
            resp.bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| OssError::Network(e.to_string()))
        } else if status == 404 {
            Err(OssError::NotFound)
        } else {
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
            "DELETE",
            &canonical_uri,
            "",
            &headers,
            &content_hash,
            &date_str,
            &date_short,
        );

        let resp = self
            .http_client
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

    /// List all object keys under a given prefix using S3 ListObjectsV2.
    /// Handles pagination via ContinuationToken. Returns up to `max_keys` total.
    pub async fn list_objects(&self, prefix: &str, max_keys: u32) -> Result<Vec<String>, OssError> {
        let mut all_keys = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let remaining = max_keys.saturating_sub(all_keys.len() as u32);
            if remaining == 0 {
                break;
            }
            let batch_size = remaining.min(1000); // S3 max per request

            let mut query_parts = vec![
                format!("list-type=2"),
                format!("prefix={}", uri_encode_component(prefix)),
                format!("max-keys={}", batch_size),
            ];
            if let Some(ref token) = continuation_token {
                query_parts.push(format!(
                    "continuation-token={}",
                    uri_encode_component(token)
                ));
            }
            query_parts.sort(); // canonical query must be sorted
            let canonical_query = query_parts.join("&");

            let now = Utc::now();
            let date_str = now.format("%Y%m%dT%H%M%SZ").to_string();
            let date_short = now.format("%Y%m%d").to_string();
            let content_hash = sha256_hex(b"");
            let host = self.host();

            let mut headers = BTreeMap::new();
            headers.insert("host".to_string(), host.clone());
            headers.insert("x-amz-date".to_string(), date_str.clone());
            headers.insert("x-amz-content-sha256".to_string(), content_hash.clone());

            let canonical_uri = if self.path_style {
                format!("/{}/", self.bucket)
            } else {
                "/".to_string()
            };

            let authorization = self.sign_v4(
                "GET",
                &canonical_uri,
                &canonical_query,
                &headers,
                &content_hash,
                &date_str,
                &date_short,
            );

            let scheme = if self.use_ssl { "https" } else { "http" };
            let endpoint = self.endpoint.trim_end_matches('/');
            let host_part = endpoint
                .strip_prefix("https://")
                .or_else(|| endpoint.strip_prefix("http://"))
                .unwrap_or(endpoint);
            let url = if self.path_style {
                format!(
                    "{}://{}/{}/?{}",
                    scheme, host_part, self.bucket, canonical_query
                )
            } else {
                format!(
                    "{}://{}.{}/?{}",
                    scheme, self.bucket, host_part, canonical_query
                )
            };

            let resp = self
                .http_client
                .get(&url)
                .header("Host", &host)
                .header("X-Amz-Date", &date_str)
                .header("X-Amz-Content-Sha256", &content_hash)
                .header("Authorization", &authorization)
                .send()
                .await
                .map_err(|e| OssError::Network(e.to_string()))?;

            if !resp.status().is_success() {
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                return Err(OssError::Api(status, body));
            }

            let body = resp
                .text()
                .await
                .map_err(|e| OssError::Network(e.to_string()))?;
            let (keys, next_token) = parse_list_objects_v2(&body)?;
            all_keys.extend(keys);

            match next_token {
                Some(token) if !token.is_empty() => continuation_token = Some(token),
                _ => break,
            }
        }

        Ok(all_keys)
    }

    /// Delete multiple objects using S3 Multi-Object Delete.
    /// Batches into groups of 1000 (S3 limit per request).
    /// Returns the total count of successfully deleted objects.
    pub async fn delete_objects_batch(&self, keys: &[String]) -> Result<u32, OssError> {
        if keys.is_empty() {
            return Ok(0);
        }

        let mut total_deleted = 0u32;

        for chunk in keys.chunks(1000) {
            let xml_body = build_delete_xml(chunk);
            let body_bytes = xml_body.into_bytes();

            let now = Utc::now();
            let date_str = now.format("%Y%m%dT%H%M%SZ").to_string();
            let date_short = now.format("%Y%m%d").to_string();
            let content_hash = sha256_hex(&body_bytes);
            let host = self.host();

            let canonical_query = "delete=";

            let mut headers = BTreeMap::new();
            headers.insert("content-type".to_string(), "application/xml".to_string());
            headers.insert("content-length".to_string(), body_bytes.len().to_string());
            headers.insert("host".to_string(), host.clone());
            headers.insert("x-amz-date".to_string(), date_str.clone());
            headers.insert("x-amz-content-sha256".to_string(), content_hash.clone());

            let canonical_uri = if self.path_style {
                format!("/{}/", self.bucket)
            } else {
                "/".to_string()
            };

            let authorization = self.sign_v4(
                "POST",
                &canonical_uri,
                canonical_query,
                &headers,
                &content_hash,
                &date_str,
                &date_short,
            );

            let scheme = if self.use_ssl { "https" } else { "http" };
            let endpoint = self.endpoint.trim_end_matches('/');
            let host_part = endpoint
                .strip_prefix("https://")
                .or_else(|| endpoint.strip_prefix("http://"))
                .unwrap_or(endpoint);
            let url = if self.path_style {
                format!("{}://{}/{}/?delete=", scheme, host_part, self.bucket)
            } else {
                format!("{}://{}.{}/?delete=", scheme, self.bucket, host_part)
            };

            let resp = self
                .http_client
                .post(&url)
                .header("Host", &host)
                .header("X-Amz-Date", &date_str)
                .header("X-Amz-Content-Sha256", &content_hash)
                .header("Content-Type", "application/xml")
                .header("Authorization", &authorization)
                .body(body_bytes)
                .send()
                .await
                .map_err(|e| OssError::Network(e.to_string()))?;

            if !resp.status().is_success() {
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                return Err(OssError::Api(status, body));
            }

            // Count deleted objects from response
            let body = resp
                .text()
                .await
                .map_err(|e| OssError::Network(e.to_string()))?;
            total_deleted += parse_delete_result_count(&body);
        }

        Ok(total_deleted)
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
            method,
            canonical_uri,
            canonical_query,
            canonical_headers,
            signed_headers_str,
            content_hash
        );

        let scope = format!("{}/{}/s3/aws4_request", date_short, self.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            date_str,
            scope,
            sha256_hex(canonical_request.as_bytes())
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
    Xml(String),
}

impl std::fmt::Display for OssError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OssError::Network(e) => write!(f, "OSS network error: {}", e),
            OssError::Api(status, body) => write!(f, "OSS API error {}: {}", status, body),
            OssError::NotFound => write!(f, "OSS object not found"),
            OssError::Xml(e) => write!(f, "OSS XML parse error: {}", e),
        }
    }
}

/// URI-encode a path for S3 requests (RFC 3986).
/// Encodes each path segment individually, preserving `/` separators.
fn uri_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len() * 3);
    for (i, segment) in path.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        encode_component_into(segment, &mut out);
    }
    out
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

/// URI-encode a single component (not a path — encodes `/` too).
fn uri_encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    encode_component_into(s, &mut out);
    out
}

/// Shared encoder: appends percent-encoded bytes to an existing String.
fn encode_component_into(s: &str, out: &mut String) {
    use std::fmt::Write;
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{:02X}", b);
        }
    }
}

/// Parse S3 ListObjectsV2 XML response.
/// Returns (keys, next_continuation_token).
fn parse_list_objects_v2(xml: &str) -> Result<(Vec<String>, Option<String>), OssError> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    let mut keys = Vec::new();
    let mut next_token: Option<String> = None;
    let mut in_contents = false;
    let mut in_key = false;
    let mut in_next_token = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => {
                let name = e.name();
                let local = name.as_ref();
                if local == b"Contents" {
                    in_contents = true;
                } else if in_contents && local == b"Key" {
                    in_key = true;
                } else if local == b"NextContinuationToken" {
                    in_next_token = true;
                }
            }
            Ok(Event::End(ref e)) => {
                let name = e.name();
                let local = name.as_ref();
                if local == b"Contents" {
                    in_contents = false;
                } else if local == b"Key" {
                    in_key = false;
                } else if local == b"NextContinuationToken" {
                    in_next_token = false;
                }
            }
            Ok(Event::Text(ref e)) => {
                if in_key {
                    let text = e
                        .unescape()
                        .map_err(|err| OssError::Xml(format!("XML unescape error: {}", err)))?;
                    keys.push(text.to_string());
                } else if in_next_token {
                    let text = e
                        .unescape()
                        .map_err(|err| OssError::Xml(format!("XML unescape error: {}", err)))?;
                    next_token = Some(text.to_string());
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(OssError::Xml(format!("XML parse error: {}", e))),
            _ => {}
        }
    }

    Ok((keys, next_token))
}

/// Build XML body for S3 Multi-Object Delete request.
fn build_delete_xml(keys: &[String]) -> String {
    let mut xml =
        String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Delete><Quiet>true</Quiet>");
    for key in keys {
        xml.push_str("<Object><Key>");
        // Escape XML special characters in key
        for ch in key.chars() {
            match ch {
                '&' => xml.push_str("&amp;"),
                '<' => xml.push_str("&lt;"),
                '>' => xml.push_str("&gt;"),
                '"' => xml.push_str("&quot;"),
                '\'' => xml.push_str("&apos;"),
                _ => xml.push(ch),
            }
        }
        xml.push_str("</Key></Object>");
    }
    xml.push_str("</Delete>");
    xml
}

/// Parse S3 Multi-Object Delete response and count <Deleted> elements.
fn parse_delete_result_count(xml: &str) -> u32 {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    let mut count = 0u32;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) if e.name().as_ref() == b"Deleted" => {
                count += 1;
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }

    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_style_url() {
        let client = OssClient::new(
            "https://s3.amazonaws.com",
            "mybucket",
            "us-east-1",
            "AKID",
            "SECRET",
            true,
            true,
        );
        assert_eq!(
            client.object_url("cache/site1/ab/abcdef"),
            "https://s3.amazonaws.com/mybucket/cache/site1/ab/abcdef"
        );
    }

    #[test]
    fn test_virtual_host_url() {
        let client = OssClient::new(
            "https://s3.amazonaws.com",
            "mybucket",
            "us-east-1",
            "AKID",
            "SECRET",
            true,
            false,
        );
        assert_eq!(
            client.object_url("cache/site1/ab/abcdef"),
            "https://mybucket.s3.amazonaws.com/cache/site1/ab/abcdef"
        );
    }

    #[test]
    fn test_host_path_style() {
        let client = OssClient::new(
            "https://s3.amazonaws.com",
            "mybucket",
            "us-east-1",
            "AKID",
            "SECRET",
            true,
            true,
        );
        assert_eq!(client.host(), "s3.amazonaws.com");
    }

    #[test]
    fn test_host_virtual() {
        let client = OssClient::new(
            "https://s3.amazonaws.com",
            "mybucket",
            "us-east-1",
            "AKID",
            "SECRET",
            true,
            false,
        );
        assert_eq!(client.host(), "mybucket.s3.amazonaws.com");
    }

    #[test]
    fn test_sha256_hex() {
        let hash = sha256_hex(b"");
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_parse_list_objects_v2() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Contents><Key>cache/site1/ab/abcdef1234</Key></Contents>
  <Contents><Key>cache/site1/cd/cdef5678</Key></Contents>
  <NextContinuationToken>token123</NextContinuationToken>
</ListBucketResult>"#;
        let (keys, token) = parse_list_objects_v2(xml).unwrap();
        assert_eq!(
            keys,
            vec!["cache/site1/ab/abcdef1234", "cache/site1/cd/cdef5678"]
        );
        assert_eq!(token, Some("token123".to_string()));
    }

    #[test]
    fn test_parse_list_objects_v2_no_continuation() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <Contents><Key>cache/site1/ab/abc</Key></Contents>
</ListBucketResult>"#;
        let (keys, token) = parse_list_objects_v2(xml).unwrap();
        assert_eq!(keys.len(), 1);
        assert!(token.is_none());
    }

    #[test]
    fn test_parse_list_objects_v2_empty() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult></ListBucketResult>"#;
        let (keys, token) = parse_list_objects_v2(xml).unwrap();
        assert!(keys.is_empty());
        assert!(token.is_none());
    }

    #[test]
    fn test_build_delete_xml() {
        let keys = vec![
            "cache/site1/ab/abc".to_string(),
            "cache/site1/cd/def".to_string(),
        ];
        let xml = build_delete_xml(&keys);
        assert!(xml.contains("<Quiet>true</Quiet>"));
        assert!(xml.contains("<Key>cache/site1/ab/abc</Key>"));
        assert!(xml.contains("<Key>cache/site1/cd/def</Key>"));
        assert!(xml.starts_with("<?xml"));
        assert!(xml.ends_with("</Delete>"));
    }

    #[test]
    fn test_build_delete_xml_escapes_special_chars() {
        let keys = vec!["cache/site&1/ab/a<b>c".to_string()];
        let xml = build_delete_xml(&keys);
        assert!(xml.contains("site&amp;1"));
        assert!(xml.contains("a&lt;b&gt;c"));
    }

    #[test]
    fn test_parse_delete_result_count() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<DeleteResult>
  <Deleted><Key>cache/site1/ab/abc</Key></Deleted>
  <Deleted><Key>cache/site1/cd/def</Key></Deleted>
  <Error><Key>cache/site1/ef/ghi</Key><Code>AccessDenied</Code></Error>
</DeleteResult>"#;
        assert_eq!(parse_delete_result_count(xml), 2);
    }

    #[test]
    fn test_parse_delete_result_count_empty() {
        let xml = r#"<DeleteResult></DeleteResult>"#;
        assert_eq!(parse_delete_result_count(xml), 0);
    }

    #[test]
    fn test_uri_encode_component() {
        assert_eq!(uri_encode_component("hello"), "hello");
        assert_eq!(uri_encode_component("a/b"), "a%2Fb");
        assert_eq!(uri_encode_component("a b"), "a%20b");
        assert_eq!(uri_encode_component("a=1&b=2"), "a%3D1%26b%3D2");
    }
}
