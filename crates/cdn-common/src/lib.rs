pub mod error;
pub mod redis_trait;
pub mod types;

pub use error::{CdnError, CdnResult};
pub use redis_trait::RedisOps;
pub use types::*;

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
