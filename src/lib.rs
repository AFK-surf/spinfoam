pub mod build;
pub mod error;
pub mod helpers;
pub mod host;
pub mod outbox;
pub mod protocol;
pub mod runtime;
pub mod stdio;
pub const ASYNC_EBPF_REVISION: &str = "7e8a7cbce195d68a1d579608b7a28b55f6fce7fd";
pub const SDK: &str = include_str!("../sdk/spinfoam.h");
pub fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
