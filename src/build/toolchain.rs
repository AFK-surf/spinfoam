use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilePin {
    pub path: PathBuf,
    pub sha256: String,
}
impl FilePin {
    pub fn new(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = std::fs::canonicalize(path)?;
        let sha256 = crate::sha256(&std::fs::read(&path)?);
        Ok(Self { path, sha256 })
    }
    pub fn verify(&self) -> anyhow::Result<()> {
        if crate::sha256(
            &std::fs::read(&self.path).with_context(|| format!("read {}", self.path.display()))?,
        ) != self.sha256
        {
            bail!("toolchain digest changed: {}", self.path.display());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Toolchain {
    pub format: u32,
    pub clang: FilePin,
    pub llc: FilePin,
    pub bwrap: FilePin,
    /// Destination in the sandbox mapped to its pinned host library.
    pub libraries: BTreeMap<PathBuf, FilePin>,
    pub clang_version: String,
    pub llc_version: String,
}
fn resolve(name: &str) -> anyhow::Result<PathBuf> {
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let path = dir.join(name);
        if path.is_file() {
            return Ok(path);
        }
    }
    bail!("{name} is not in PATH")
}
fn version(path: &Path) -> anyhow::Result<String> {
    let output = Command::new(path).arg("--version").output()?;
    if !output.status.success() {
        bail!("{} --version failed", path.display());
    }
    Ok(String::from_utf8(output.stdout)?
        .lines()
        .take(3)
        .collect::<Vec<_>>()
        .join("\n"))
}
impl Toolchain {
    pub fn discover() -> anyhow::Result<Self> {
        let clang = FilePin::new(resolve("clang")?)?;
        let llc = FilePin::new(resolve("llc")?)?;
        let bwrap = FilePin::new(resolve("bwrap")?)?;
        let runner = std::env::current_exe()?;
        let mut libraries = BTreeMap::new();
        for binary in [&clang.path, &llc.path, &runner] {
            let output = Command::new("ldd").arg(binary).output()?;
            if !output.status.success() {
                bail!("ldd failed for {}", binary.display());
            }
            for word in std::str::from_utf8(&output.stdout)?
                .split_whitespace()
                .filter(|w| w.starts_with('/'))
            {
                let mut dest = PathBuf::new();
                for part in Path::new(word).components() {
                    if matches!(part, std::path::Component::ParentDir) {
                        dest.pop();
                    } else {
                        dest.push(part);
                    }
                }
                libraries.insert(dest.clone(), FilePin::new(dest)?);
            }
        }
        Ok(Self {
            format: 1,
            clang_version: version(&clang.path)?,
            llc_version: version(&llc.path)?,
            clang,
            llc,
            bwrap,
            libraries,
        })
    }
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)?;
        if bytes.len() > 65536 {
            bail!("toolchain manifest too large");
        }
        let this: Self = serde_json::from_slice(&bytes)?;
        if this.format != 1 {
            bail!("unsupported toolchain manifest");
        }
        for file in [&this.clang, &this.llc, &this.bwrap] {
            file.verify()?;
        }
        for (dest, file) in &this.libraries {
            if !["/lib", "/lib64", "/usr/lib", "/usr/lib64"]
                .iter()
                .any(|p| dest.starts_with(p))
                || dest
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                bail!("invalid library destination");
            }
            file.verify()?;
        }
        Ok(this)
    }
    pub fn digest(&self) -> String {
        crate::sha256(&serde_json::to_vec(self).expect("serializable manifest"))
    }
}
