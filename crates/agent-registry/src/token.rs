//! Credential format: a recognizable prefix and 32 random bytes in hex.

use control_plane_common::registry::Secret;
use sha2::{Digest, Sha256};
use std::fmt::Write;

pub const RUN_PREFIX: &str = "sona_plane_rt_";
pub const OPERATOR_PREFIX: &str = "sona_plane_op_";
pub const TOKEN_HEX_LEN: usize = 64;

pub fn generate(prefix: &str) -> Secret {
    let bytes: [u8; 32] = rand::random();
    let mut value = String::with_capacity(prefix.len() + TOKEN_HEX_LEN);
    value.push_str(prefix);
    for byte in bytes {
        // Writing to a String cannot fail.
        let _ = write!(value, "{byte:02x}");
    }
    Secret::new(value)
}

/// SHA-256 is enough here: tokens have full entropy, so there is nothing to brute-force.
pub fn hash(credential: &str) -> Vec<u8> {
    Sha256::digest(credential).to_vec()
}
