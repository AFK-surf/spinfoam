//! The Linux compiler isolation contract has no macOS implementation.
use super::toolchain::Toolchain;
use std::{collections::BTreeMap, path::Path};
use tokio_util::sync::CancellationToken;

pub struct Output {
    pub elf: Vec<u8>,
    pub diagnostics: String,
    pub truncated: bool,
}
pub fn launch(_: &Path) -> anyhow::Result<()> {
    anyhow::bail!("sandboxed compilation currently requires Linux")
}
pub fn worker(_: &str) -> anyhow::Result<()> {
    anyhow::bail!("sandboxed compilation currently requires Linux")
}
pub async fn run(
    _: &Toolchain,
    _: &Path,
    _: &BTreeMap<String, String>,
    _: &str,
    _: &CancellationToken,
) -> anyhow::Result<Output> {
    anyhow::bail!("sandboxed compilation currently requires Linux")
}
