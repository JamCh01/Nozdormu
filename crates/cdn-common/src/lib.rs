pub mod error;
pub mod types;
pub mod redis_trait;

pub use error::{CdnError, CdnResult};
pub use types::*;
pub use redis_trait::RedisOps;

/// Zero-allocation hex encoding for byte slices.
pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX_CHARS[(b >> 4) as usize] as char);
        s.push(HEX_CHARS[(b & 0x0f) as usize] as char);
    }
    s
}
