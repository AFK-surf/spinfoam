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
    pub sandbox: PathBuf,
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
        let clang = resolve("clang")?;
        let llc = resolve("llc")?;
        #[cfg(target_os = "linux")]
        let sandbox = resolve("bwrap")?;
        #[cfg(target_os = "macos")]
        let sandbox = std::fs::canonicalize("/usr/bin/sandbox-exec")?;
        #[cfg(target_os = "linux")]
        let runner = std::env::current_exe()?;
        #[cfg(target_os = "linux")]
        let mut libraries = BTreeMap::new();
        #[cfg(target_os = "linux")]
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
        #[cfg(target_os = "macos")]
        let libraries = macos_libraries(&[&clang, &llc])?;
        Ok(Self {
            clang_version: version(&clang)?,
            llc_version: version(&llc)?,
            clang,
            llc,
            sandbox,
            libraries,
        })
    }
    pub fn digest(&self) -> String {
        crate::sha256(&serde_json::to_vec(self).expect("serializable toolchain"))
    }
}

#[cfg(target_os = "macos")]
fn macos_libraries(tools: &[&PathBuf]) -> anyhow::Result<BTreeMap<PathBuf, PathBuf>> {
    use anyhow::Context;
    let mut found = BTreeMap::new();
    let mut pending: Vec<PathBuf> = tools.iter().map(|p| (*p).clone()).collect();
    while let Some(binary) = pending.pop() {
        if found.contains_key(&binary) {
            continue;
        }
        if found.len() >= 256 {
            bail!("toolchain dependency graph exceeds limit");
        }
        found.insert(binary.clone(), binary.clone());
        let loader = binary.parent().context("missing tool directory")?;
        let output = Command::new("/usr/bin/otool")
            .arg("-L")
            .arg(&binary)
            .output()?;
        if !output.status.success() {
            bail!("otool failed for {}", binary.display());
        }
        let commands = Command::new("/usr/bin/otool")
            .arg("-l")
            .arg(&binary)
            .output()?;
        let commands = String::from_utf8(commands.stdout)?;
        let mut rpaths: Vec<PathBuf> = tools
            .iter()
            .filter_map(|p| p.parent()?.parent().map(|p| p.join("lib")))
            .collect();
        let mut is_rpath = false;
        for line in commands.lines().map(str::trim) {
            if line == "cmd LC_RPATH" {
                is_rpath = true;
            } else if is_rpath && line.starts_with("path ") {
                if let Some(path) = line
                    .strip_prefix("path ")
                    .and_then(|s| s.split(" (offset ").next())
                {
                    rpaths.push(PathBuf::from(
                        path.replace("@loader_path", &loader.to_string_lossy()),
                    ));
                }
                is_rpath = false;
            }
        }
        for line in std::str::from_utf8(&output.stdout)?.lines().skip(1) {
            let dependency = line.trim().split(" (compatibility version").next().unwrap();
            if dependency.starts_with("/usr/lib/") || dependency.starts_with("/System/Library/") {
                continue; // macOS also resolves these from its dyld shared cache.
            }
            let path = if let Some(name) = dependency.strip_prefix("@rpath/") {
                rpaths
                    .iter()
                    .map(|p| p.join(name))
                    .find(|p| p.is_file())
                    .with_context(|| {
                        format!("cannot resolve {dependency} for {}", binary.display())
                    })?
            } else if let Some(name) = dependency.strip_prefix("@loader_path/") {
                loader.join(name)
            } else if let Some(name) = dependency.strip_prefix("@executable_path/") {
                tools[0].parent().unwrap().join(name)
            } else {
                PathBuf::from(dependency)
            };
            pending.push(
                std::fs::canonicalize(&path)
                    .with_context(|| format!("resolve library {}", path.display()))?,
            );
        }
    }
    Ok(found)
}
