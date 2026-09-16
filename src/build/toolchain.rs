use anyhow::bail;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Toolchain {
    pub clang: PathBuf,
    pub llc: PathBuf,
    pub bwrap: PathBuf,
    /// Destination in the sandbox mapped to its resolved host library.
    pub libraries: BTreeMap<PathBuf, PathBuf>,
    pub clang_version: String,
    pub llc_version: String,
}
fn resolve(name: &str) -> anyhow::Result<PathBuf> {
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let path = dir.join(name);
        if path.is_file() {
            return Ok(std::fs::canonicalize(path)?);
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
        if !cfg!(target_os = "linux") {
            bail!("sandboxed compilation currently requires Linux");
        }
        let clang = resolve("clang")?;
        let llc = resolve("llc")?;
        let bwrap = resolve("bwrap")?;
        let runner = std::env::current_exe()?;
        let mut libraries = BTreeMap::new();
        for binary in [&clang, &llc, &runner] {
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
                libraries.insert(dest.clone(), std::fs::canonicalize(dest)?);
            }
        }
        Ok(Self {
            clang_version: version(&clang)?,
            llc_version: version(&llc)?,
            clang,
            llc,
            bwrap,
            libraries,
        })
    }
    pub fn digest(&self) -> String {
        crate::sha256(&serde_json::to_vec(self).expect("serializable toolchain"))
    }
}
