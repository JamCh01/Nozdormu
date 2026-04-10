use cdn_common::{CompressionAlgorithm, CompressionConfig};
use std::io::Write;

/// Negotiate the best compression algorithm based on client `Accept-Encoding`
/// and server configuration.
///
/// Returns the first algorithm from `config.algorithms` (server priority) that
/// the client also supports (not rejected with q=0).
pub fn negotiate(
    accept_encoding: &str,
    config: &CompressionConfig,
) -> Option<CompressionAlgorithm> {
    let client_encodings = parse_accept_encoding(accept_encoding);

    for algo in &config.algorithms {
        let token = algo.encoding_token();
        // Check if client supports this encoding (present and q > 0)
        let supported = client_encodings
            .iter()
            .any(|(name, q)| *name == token && *q > 0.0);
        if supported {
            return Some(algo.clone());
        }
    }
    None
}

/// Parse `Accept-Encoding` header into (token, quality) pairs.
///
/// Examples:
///   "gzip, br;q=1.0, zstd;q=0.5" → [("gzip", 1.0), ("br", 1.0), ("zstd", 0.5)]
///   "gzip;q=0" → [("gzip", 0.0)]  (explicitly rejected)
fn parse_accept_encoding(header: &str) -> Vec<(&str, f32)> {
    header
        .split(',')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            let mut iter = part.splitn(2, ';');
            let token = iter.next()?.trim();
            let quality = iter
                .next()
                .and_then(|params| {
                    params
                        .trim()
                        .strip_prefix("q=")
                        .and_then(|q| q.trim().parse::<f32>().ok())
                })
                .unwrap_or(1.0);
            Some((token, quality))
        })
        .collect()
}

/// Check if a content type is compressible according to the config.
///
/// Supports wildcard matching: `text/*` matches `text/html`, `text/plain`, etc.
pub fn is_compressible(content_type: &str, config: &CompressionConfig) -> bool {
    // Extract MIME type without parameters (e.g., "text/html; charset=utf-8" → "text/html")
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_lowercase();

    for pattern in &config.compressible_types {
        let pattern = pattern.to_lowercase();
        if pattern.ends_with("/*") {
            // Wildcard: "text/*" matches "text/html"
            let prefix = &pattern[..pattern.len() - 1]; // "text/"
            if mime.starts_with(prefix) {
                return true;
            }
        } else if mime == pattern {
            return true;
        }
    }
    false
}

/// Streaming compression encoder.
///
/// Wraps gzip, Brotli, or Zstandard encoders with a unified interface
/// for chunk-by-chunk compression in Pingora's `response_body_filter`.
pub enum Encoder {
    Gzip(flate2::write::GzEncoder<Vec<u8>>),
    Brotli(Box<brotli::enc::writer::CompressorWriter<Vec<u8>>>),
    Zstd(zstd::Encoder<'static, Vec<u8>>),
}

impl Encoder {
    /// Create a new encoder for the given algorithm and compression level.
    pub fn new(algo: &CompressionAlgorithm, level: u32) -> Self {
        match algo {
            CompressionAlgorithm::Gzip => {
                let gz_level = flate2::Compression::new(level.min(9));
                Encoder::Gzip(flate2::write::GzEncoder::new(Vec::new(), gz_level))
            }
            CompressionAlgorithm::Brotli => {
                // Brotli quality: 0-11, buffer size 4096
                let br_level = level.min(11);
                let writer = brotli::enc::writer::CompressorWriter::new(
                    Vec::new(),
                    4096,
                    br_level,
                    22, // lgwin (window size log2)
                );
                Encoder::Brotli(Box::new(writer))
            }
            CompressionAlgorithm::Zstd => {
                let zstd_level = (level as i32).min(22);
                let encoder = zstd::Encoder::new(Vec::new(), zstd_level)
                    .expect("failed to create zstd encoder");
                Encoder::Zstd(encoder)
            }
        }
    }

    /// Write a chunk of data and return any compressed output available.
    ///
    /// The encoder may buffer data internally; not every input chunk
    /// produces output. Call `finish()` to flush the final bytes.
    /// Returns Err on write/flush failure — caller should stop compressing.
    pub fn write_chunk(&mut self, data: &[u8]) -> Result<Vec<u8>, std::io::Error> {
        match self {
            Encoder::Gzip(enc) => {
                enc.write_all(data)?;
                enc.flush()?;
                Ok(std::mem::take(enc.get_mut()))
            }
            Encoder::Brotli(enc) => {
                enc.write_all(data)?;
                enc.flush()?;
                Ok(std::mem::take(enc.get_mut()))
            }
            Encoder::Zstd(enc) => {
                enc.write_all(data)?;
                enc.flush()?;
                Ok(std::mem::take(enc.get_mut()))
            }
        }
    }

    /// Finalize the compression stream and return any remaining bytes.
    pub fn finish(self) -> Vec<u8> {
        match self {
            Encoder::Gzip(enc) => enc.finish().unwrap_or_default(),
            Encoder::Brotli(enc) => {
                // CompressorWriter::into_inner() finalizes and returns the inner writer
                enc.into_inner()
            }
            Encoder::Zstd(enc) => enc.finish().unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> CompressionConfig {
        CompressionConfig {
            enabled: true,
            algorithms: vec![
                CompressionAlgorithm::Zstd,
                CompressionAlgorithm::Brotli,
                CompressionAlgorithm::Gzip,
            ],
            level: 6,
            min_size: 256,
            compressible_types: vec![
                "text/*".to_string(),
                "application/json".to_string(),
                "image/svg+xml".to_string(),
            ],
        }
    }

    // ── negotiate tests ──

    #[test]
    fn test_negotiate_prefers_server_order() {
        let config = test_config(); // server order: zstd, br, gzip
                                    // Client supports all three
        let result = negotiate("gzip, br, zstd", &config);
        assert_eq!(result, Some(CompressionAlgorithm::Zstd));
    }

    #[test]
    fn test_negotiate_client_only_gzip() {
        let config = test_config();
        let result = negotiate("gzip", &config);
        assert_eq!(result, Some(CompressionAlgorithm::Gzip));
    }

    #[test]
    fn test_negotiate_client_rejects_with_q0() {
        let config = test_config();
        // Client rejects zstd (q=0), supports br and gzip
        let result = negotiate("zstd;q=0, br, gzip", &config);
        assert_eq!(result, Some(CompressionAlgorithm::Brotli));
    }

    #[test]
    fn test_negotiate_no_match() {
        let config = CompressionConfig {
            enabled: true,
            algorithms: vec![CompressionAlgorithm::Zstd],
            ..Default::default()
        };
        let result = negotiate("gzip, br", &config);
        assert_eq!(result, None);
    }

    #[test]
    fn test_negotiate_empty_accept() {
        let config = test_config();
        let result = negotiate("", &config);
        assert_eq!(result, None);
    }

    #[test]
    fn test_negotiate_identity_only() {
        let config = test_config();
        let result = negotiate("identity", &config);
        assert_eq!(result, None);
    }

    #[test]
    fn test_negotiate_with_quality_values() {
        let config = test_config();
        let result = negotiate("gzip;q=0.5, br;q=1.0", &config);
        // Server prefers zstd (not supported), then br (supported)
        assert_eq!(result, Some(CompressionAlgorithm::Brotli));
    }

    // ── is_compressible tests ──

    #[test]
    fn test_compressible_text_html() {
        let config = test_config();
        assert!(is_compressible("text/html", &config));
    }

    #[test]
    fn test_compressible_text_wildcard() {
        let config = test_config();
        assert!(is_compressible("text/plain", &config));
        assert!(is_compressible("text/css", &config));
        assert!(is_compressible("text/javascript", &config));
    }

    #[test]
    fn test_compressible_json() {
        let config = test_config();
        assert!(is_compressible("application/json", &config));
    }

    #[test]
    fn test_compressible_with_charset() {
        let config = test_config();
        assert!(is_compressible("text/html; charset=utf-8", &config));
    }

    #[test]
    fn test_not_compressible_image() {
        let config = test_config();
        assert!(!is_compressible("image/jpeg", &config));
        assert!(!is_compressible("image/png", &config));
    }

    #[test]
    fn test_compressible_svg() {
        let config = test_config();
        assert!(is_compressible("image/svg+xml", &config));
    }

    #[test]
    fn test_not_compressible_video() {
        let config = test_config();
        assert!(!is_compressible("video/mp4", &config));
    }

    #[test]
    fn test_compressible_case_insensitive() {
        let config = test_config();
        assert!(is_compressible("Text/HTML", &config));
        assert!(is_compressible("APPLICATION/JSON", &config));
    }

    // ── Encoder round-trip tests ──

    #[test]
    fn test_gzip_roundtrip() {
        let data = b"Hello, World! This is a test of gzip compression.";
        let mut encoder = Encoder::new(&CompressionAlgorithm::Gzip, 6);
        let chunk = encoder.write_chunk(data).unwrap();
        let final_bytes = encoder.finish();

        let compressed: Vec<u8> = [chunk, final_bytes].concat();
        assert!(!compressed.is_empty());

        // Decompress and verify
        use flate2::read::GzDecoder;
        use std::io::Read;
        let mut decoder = GzDecoder::new(&compressed[..]);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_brotli_roundtrip() {
        let data = b"Hello, World! This is a test of Brotli compression.";
        let mut encoder = Encoder::new(&CompressionAlgorithm::Brotli, 6);
        let chunk = encoder.write_chunk(data).unwrap();
        let final_bytes = encoder.finish();

        let compressed: Vec<u8> = [chunk, final_bytes].concat();
        assert!(!compressed.is_empty());

        // Decompress and verify
        let mut decompressed = Vec::new();
        brotli::BrotliDecompress(&mut &compressed[..], &mut decompressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_zstd_roundtrip() {
        let data = b"Hello, World! This is a test of Zstandard compression.";
        let mut encoder = Encoder::new(&CompressionAlgorithm::Zstd, 6);
        let chunk = encoder.write_chunk(data).unwrap();
        let final_bytes = encoder.finish();

        let compressed: Vec<u8> = [chunk, final_bytes].concat();
        assert!(!compressed.is_empty());

        // Decompress and verify
        let decompressed = zstd::decode_all(&compressed[..]).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_multi_chunk_gzip() {
        let chunks: Vec<&[u8]> = vec![
            b"First chunk of data. ",
            b"Second chunk of data. ",
            b"Third and final chunk.",
        ];
        let expected: Vec<u8> = chunks.iter().flat_map(|c| c.iter().copied()).collect();

        let mut encoder = Encoder::new(&CompressionAlgorithm::Gzip, 6);
        let mut compressed = Vec::new();
        for chunk in &chunks {
            compressed.extend(encoder.write_chunk(chunk).unwrap());
        }
        compressed.extend(encoder.finish());

        // Decompress and verify
        use flate2::read::GzDecoder;
        use std::io::Read;
        let mut decoder = GzDecoder::new(&compressed[..]);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).unwrap();
        assert_eq!(decompressed, expected);
    }

    // ── CompressionAlgorithm tests ──

    #[test]
    fn test_encoding_tokens() {
        assert_eq!(CompressionAlgorithm::Gzip.encoding_token(), "gzip");
        assert_eq!(CompressionAlgorithm::Brotli.encoding_token(), "br");
        assert_eq!(CompressionAlgorithm::Zstd.encoding_token(), "zstd");
    }

    #[test]
    fn test_from_token() {
        assert_eq!(
            CompressionAlgorithm::from_token("gzip"),
            Some(CompressionAlgorithm::Gzip)
        );
        assert_eq!(
            CompressionAlgorithm::from_token("br"),
            Some(CompressionAlgorithm::Brotli)
        );
        assert_eq!(
            CompressionAlgorithm::from_token("zstd"),
            Some(CompressionAlgorithm::Zstd)
        );
        assert_eq!(CompressionAlgorithm::from_token("deflate"), None);
        assert_eq!(CompressionAlgorithm::from_token("identity"), None);
    }
}
