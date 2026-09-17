//! Seatbelt confines each compiler directly: no compiler descendants are allowed.
use super::toolchain::Toolchain;
use anyhow::{Context, bail};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::Read,
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::Path,
    process::Stdio,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub struct Output {
    pub elf: Vec<u8>,
    pub diagnostics: String,
    pub truncated: bool,
}

// Linux uses a separate in-namespace worker. macOS supervises each tool directly.
pub fn launch(_: &Path) -> anyhow::Result<()> {
    bail!("internal launcher is Linux-only")
}
pub fn worker(_: &str) -> anyhow::Result<()> {
    bail!("internal worker is Linux-only")
}

fn quoted(path: &Path) -> anyhow::Result<String> {
    // JSON string escaping is also valid for the SBPL string literals used here.
    Ok(serde_json::to_string(
        path.to_str().context("non-UTF8 sandbox path")?,
    )?)
}
fn profile(
    toolchain: &Toolchain,
    tool: &Path,
    src: &Path,
    sdk: &Path,
    input: &Path,
    output: &Path,
) -> anyhow::Result<String> {
    let mut policy = format!(
        r#"(version 1)
(deny default)
(allow process-exec (literal {}))
(allow signal (target self))
(allow process-info* (target self))
(allow sysctl-read)
(allow file-read-metadata)
(allow file-read* (literal "/"))
(allow system-mac-syscall (mac-policy-name "vnguard"))
(allow system-mac-syscall
    (require-all (mac-policy-name "Sandbox") (mac-syscall-number 67)))
(allow file-map-executable (subpath "/System/Library") (subpath "/usr/lib"))
(allow file-read* file-map-executable
    (subpath "/System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld")
    (subpath "/private/var/db/dyld"))
(allow file-read* (subpath "/System/Library") (subpath "/usr/lib")
    (literal "/dev/null") (literal "/dev/random") (literal "/dev/urandom")
    (subpath {}) (subpath {}) (literal {}))
(allow file-write* (literal {}) (literal "/dev/null"))
"#,
        quoted(tool)?,
        quoted(src)?,
        quoted(sdk)?,
        quoted(input)?,
        quoted(output)?
    );
    policy.push_str(&format!(
        "(allow file-read* (literal {}))\n",
        quoted(output.parent().context("output has no parent")?)?
    ));
    for library in toolchain.libraries.values() {
        policy.push_str(&format!(
            "(allow file-read* file-map-executable (literal {}))
",
            quoted(library)?
        ));
    }
    // No network, process-fork, process-debug, other executables or host-file data access.
    Ok(policy)
}
async fn capture<R: tokio::io::AsyncRead + Unpin>(
    mut input: R,
) -> std::io::Result<(Vec<u8>, bool)> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut buffer = [0u8; 4096];
    loop {
        let n = input.read(&mut buffer).await?;
        if n == 0 {
            return Ok((bytes, truncated));
        }
        let take = n.min((64 * 1024usize).saturating_sub(bytes.len()));
        bytes.extend_from_slice(&buffer[..take]);
        truncated |= take < n;
    }
}
async fn stage(
    toolchain: &Toolchain,
    tool: &Path,
    policy: &str,
    args: &[&str],
    cwd: &Path,
    cancel: &CancellationToken,
    deadline: tokio::time::Instant,
) -> anyhow::Result<(String, bool)> {
    let mut cmd = std::process::Command::new(&toolchain.sandbox);
    cmd.args(["-p", policy])
        .arg(tool)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("TMPDIR", cwd)
        .env("LANG", "C")
        .env("SOURCE_DATE_EPOCH", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // SAFETY: this callback only invokes setrlimit on fixed, stack-resident data.
    unsafe {
        cmd.pre_exec(|| {
            for (resource, limit) in [
                (libc::RLIMIT_CORE, 0),
                (libc::RLIMIT_NOFILE, 64),
                (libc::RLIMIT_FSIZE, 16 * 1024 * 1024),
                (libc::RLIMIT_CPU, 5),
            ] {
                let r = libc::rlimit {
                    rlim_cur: limit,
                    rlim_max: limit,
                };
                if libc::setrlimit(resource, &r) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = tokio::process::Command::from(cmd)
        .kill_on_drop(true)
        .spawn()?;
    let pid = child.id().context("missing compiler pid")? as libc::pid_t;
    let stderr = capture(child.stderr.take().unwrap());
    // Darwin's large shared-cache reservations make a Linux-style 1 GiB
    // RLIMIT_AS inappropriate. Sample physical footprint instead, fail closed
    // on accounting errors, and document that this is a sampled rather than hard limit.
    let memory = async {
        loop {
            tokio::time::sleep(Duration::from_millis(25)).await;
            let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::uninit();
            let rc = unsafe {
                libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, info.as_mut_ptr().cast())
            };
            if rc != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    continue;
                }
                return Err::<(), _>(anyhow::anyhow!("compiler memory accounting: {error}"));
            }
            if unsafe { info.assume_init() }.ri_phys_footprint > 256 * 1024 * 1024 {
                bail!("compiler exceeded 256 MiB physical footprint");
            }
        }
    };
    let result = tokio::select! {
        _ = cancel.cancelled() => Err(anyhow::anyhow!("build cancelled")),
        _ = tokio::time::sleep_until(deadline) => Err(anyhow::anyhow!("build wall deadline exceeded")),
        result = memory => result.and_then(|_| Err(anyhow::anyhow!("memory monitor stopped"))),
        result = async { tokio::try_join!(child.wait(), stderr) } => result.map_err(Into::into),
    };
    // sandbox-exec execs the tool in place and Seatbelt denies process-fork.
    let _ = child.kill().await;
    let _ = child.wait().await;
    let (status, (diagnostics, truncated)) = result?;
    let diagnostics = String::from_utf8_lossy(&diagnostics).into_owned();
    if !status.success() {
        bail!("compiler failed ({status}): {diagnostics}");
    }
    Ok((diagnostics, truncated))
}
pub async fn run(
    toolchain: &Toolchain,
    files: &BTreeMap<String, String>,
    entry: &str,
    cancel: &CancellationToken,
) -> anyhow::Result<Output> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let src = root.join("src");
    let sdk = root.join("sdk");
    let work = root.join("work");
    for dir in [&src, &sdk, &work] {
        std::fs::create_dir(dir)?;
    }
    for (name, contents) in files {
        let path = src.join(name);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, contents)?;
    }
    std::fs::write(sdk.join("spinfoam.h"), crate::SDK)?;
    let source = src.join(entry);
    let bitcode = work.join("main.bc");
    let elf = work.join("main.o");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let includes = [
        format!("-I{}", sdk.display()),
        format!("-I{}", src.display()),
    ];
    let policy = profile(toolchain, &toolchain.clang, &src, &sdk, &source, &bitcode)?;
    let (mut diagnostics, mut truncated) = stage(
        toolchain,
        &toolchain.clang,
        &policy,
        &[
            "-O2",
            "-Wall",
            "-target",
            "bpfel",
            "-ffreestanding",
            "-fintegrated-cc1",
            "-fno-temp-file",
            "-fno-builtin",
            "-fno-zero-initialized-in-bss",
            "-nostdinc",
            &includes[0],
            &includes[1],
            "-emit-llvm",
            "-c",
            source.to_str().unwrap(),
            "-o",
            bitcode.to_str().unwrap(),
        ],
        &work,
        cancel,
        deadline,
    )
    .await?;
    let policy = profile(toolchain, &toolchain.llc, &src, &sdk, &bitcode, &elf)?;
    let (more, cut) = stage(
        toolchain,
        &toolchain.llc,
        &policy,
        &[
            "-march=bpf",
            "-mcpu=v3",
            "-bpf-stack-size=4096",
            "--nozero-initialized-in-bss",
            "-filetype=obj",
            bitcode.to_str().unwrap(),
            "-o",
            elf.to_str().unwrap(),
        ],
        &work,
        cancel,
        deadline,
    )
    .await?;
    diagnostics.push_str(&more);
    truncated |= cut;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&elf)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 64 * 1024 {
        bail!("invalid or oversized compiler output");
    }
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() > 64 * 1024 {
        bail!("invalid compiler output size");
    }
    Ok(Output {
        elf: bytes,
        diagnostics,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn seatbelt_restrictions_and_limits() {
        let cwd = std::env::current_dir().unwrap();
        let mode_file = cwd.parent().unwrap().join("src/mode");
        if let Ok(mode) = std::fs::read_to_string(&mode_file) {
            match mode.as_str() {
                "denials" => {
                    let secret =
                        std::fs::read_to_string(cwd.parent().unwrap().join("src/secret-path"))
                            .unwrap();
                    assert!(std::fs::read(&secret).is_err());
                    assert!(std::fs::write(&secret, b"changed").is_err());
                    assert!(std::fs::write(cwd.join("unapproved"), b"x").is_err());
                    assert!(std::net::TcpListener::bind(("127.0.0.1", 0)).is_err());
                    unsafe {
                        let pid = libc::fork();
                        if pid == 0 {
                            libc::_exit(99);
                        }
                        assert_eq!(pid, -1);
                        assert_eq!(
                            std::io::Error::last_os_error().raw_os_error(),
                            Some(libc::EPERM)
                        );
                    }
                }
                "cpu" => loop {
                    std::hint::spin_loop();
                },
                "wall" => std::thread::sleep(Duration::from_secs(20)),
                "memory" => {
                    let mut allocations = Vec::new();
                    for _ in 0..24 {
                        allocations.push(vec![1u8; 16 * 1024 * 1024]);
                        std::hint::black_box(&allocations);
                        std::thread::sleep(Duration::from_millis(25));
                    }
                    std::thread::sleep(Duration::from_secs(10));
                }
                _ => panic!("unknown child mode"),
            }
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let src = root.join("src");
        let sdk = root.join("sdk");
        let work = root.join("work");
        for dir in [&src, &sdk, &work] {
            std::fs::create_dir(dir).unwrap();
        }
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), b"secret").unwrap();
        std::fs::write(src.join("secret-path"), outside.path().to_str().unwrap()).unwrap();
        let exe = std::env::current_exe().unwrap();
        let tools = Toolchain {
            clang: exe.clone(),
            llc: exe.clone(),
            sandbox: "/usr/bin/sandbox-exec".into(),
            libraries: BTreeMap::from([(exe.clone(), exe.clone())]),
            clang_version: String::new(),
            llc_version: String::new(),
        };
        for mode in ["denials", "memory", "cpu", "wall"] {
            std::fs::write(src.join("mode"), mode).unwrap();
            let policy = profile(
                &tools,
                &exe,
                &src,
                &sdk,
                &src.join("mode"),
                &work.join("out"),
            )
            .unwrap();
            let timeout = if mode == "wall" {
                Duration::from_millis(500)
            } else {
                Duration::from_secs(15)
            };
            let result = stage(
                &tools,
                &exe,
                &policy,
                &[
                    "--exact",
                    "build::sandbox::tests::seatbelt_restrictions_and_limits",
                    "--nocapture",
                ],
                &work,
                &CancellationToken::new(),
                tokio::time::Instant::now() + timeout,
            )
            .await;
            match mode {
                "denials" => {
                    result.unwrap();
                }
                "memory" => assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("physical footprint")
                ),
                "cpu" => {
                    let error = result.unwrap_err().to_string();
                    // Darwin can terminate at the soft limit with SIGXCPU before
                    // reaching hard-limit SIGKILL; both enforce the CPU budget.
                    assert!(
                        error.contains("SIGXCPU") || error.contains("SIGKILL"),
                        "{error}"
                    );
                }
                "wall" => assert!(result.unwrap_err().to_string().contains("wall deadline")),
                _ => unreachable!(),
            }
        }
        assert_eq!(std::fs::read(outside.path()).unwrap(), b"secret");
    }
}
