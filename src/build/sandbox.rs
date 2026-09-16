use super::toolchain::Toolchain;
use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{fs::OpenOptionsExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub const MAX_ELF: usize = 64 * 1024;
pub const MAX_DIAGNOSTICS: usize = 64 * 1024;
#[derive(Serialize, Deserialize)]
pub struct Launch {
    pub toolchain: Toolchain,
    pub cgroup: PathBuf,
    pub sources: PathBuf,
    pub sdk: PathBuf,
    pub runner: PathBuf,
    pub entry: String,
    pub filter: PathBuf,
}
pub struct Cgroup {
    pub path: PathBuf,
}
impl Cgroup {
    pub fn new(root: &Path) -> anyhow::Result<Self> {
        let root = std::fs::canonicalize(root)?;
        let path = root.join(format!("spinfoam-{:032x}", rand::random::<u128>()));
        std::fs::create_dir(&path).context("create delegated build cgroup")?;
        let group = Self { path };
        for (file, value) in [
            ("memory.max", "268435456"),
            ("memory.swap.max", "0"),
            ("memory.oom.group", "1"),
            ("pids.max", "16"),
            ("cpu.max", "100000 100000"),
        ] {
            std::fs::write(group.path.join(file), value).with_context(|| format!("set {file}"))?;
        }
        // Require whole-job kill support before launching anything.
        OpenOptions::new()
            .write(true)
            .open(group.path.join("cgroup.kill"))?;
        Ok(group)
    }
    pub fn kill(&self) {
        let _ = std::fs::write(self.path.join("cgroup.kill"), "1");
    }
    pub fn cpu_used(&self) -> anyhow::Result<u64> {
        let stats = std::fs::read_to_string(self.path.join("cpu.stat"))?;
        stats
            .lines()
            .find_map(|l| l.strip_prefix("usage_usec "))
            .context("missing CPU accounting")?
            .parse()
            .context("invalid CPU accounting")
    }
}
impl Drop for Cgroup {
    fn drop(&mut self) {
        self.kill();
        let _ = std::fs::remove_dir(&self.path);
    }
}

/// A classic BPF seccomp allowlist, installed by bubblewrap after namespace setup.
/// Other architectures are killed; clone3 gets ENOSYS so libc uses clone/vfork.
pub fn filter() -> Vec<u8> {
    const ALLOW: u32 = 0x7fff0000;
    const KILL: u32 = 0x80000000;
    const ERRNO: u32 = 0x00050000;
    let mut p: Vec<libc::sock_filter> = Vec::new();
    let mut ins = |code: u16, jt: u8, jf: u8, k: u32| p.push(libc::sock_filter { code, jt, jf, k });
    ins(0x20, 0, 0, 4);
    ins(0x15, 1, 0, 0xc000003e);
    ins(0x06, 0, 0, KILL);
    ins(0x20, 0, 0, 0);
    ins(0x15, 0, 1, libc::SYS_clone3 as u32);
    ins(0x06, 0, 0, ERRNO | libc::ENOSYS as u32);
    let allowed = [
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_readv,
        libc::SYS_writev,
        libc::SYS_open,
        libc::SYS_openat,
        libc::SYS_close,
        libc::SYS_close_range,
        libc::SYS_stat,
        libc::SYS_lstat,
        libc::SYS_fstat,
        libc::SYS_newfstatat,
        libc::SYS_statx,
        libc::SYS_lseek,
        libc::SYS_pread64,
        libc::SYS_pwrite64,
        libc::SYS_mmap,
        libc::SYS_mprotect,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_madvise,
        libc::SYS_brk,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_rt_sigsuspend,
        libc::SYS_rt_sigtimedwait,
        libc::SYS_sigaltstack,
        libc::SYS_access,
        libc::SYS_faccessat,
        libc::SYS_faccessat2,
        libc::SYS_getcwd,
        libc::SYS_chdir,
        libc::SYS_readlink,
        libc::SYS_readlinkat,
        libc::SYS_uname,
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_getppid,
        libc::SYS_getuid,
        libc::SYS_geteuid,
        libc::SYS_getgid,
        libc::SYS_getegid,
        libc::SYS_arch_prctl,
        libc::SYS_set_tid_address,
        libc::SYS_set_robust_list,
        libc::SYS_rseq,
        libc::SYS_prlimit64,
        libc::SYS_getrlimit,
        libc::SYS_setrlimit,
        libc::SYS_getrandom,
        libc::SYS_futex,
        libc::SYS_clock_gettime,
        libc::SYS_gettimeofday,
        libc::SYS_time,
        libc::SYS_sched_getaffinity,
        libc::SYS_sched_yield,
        libc::SYS_ioctl,
        libc::SYS_fcntl,
        libc::SYS_dup,
        libc::SYS_dup2,
        libc::SYS_dup3,
        libc::SYS_pipe,
        libc::SYS_pipe2,
        libc::SYS_clone,
        libc::SYS_vfork,
        libc::SYS_fork,
        libc::SYS_wait4,
        libc::SYS_waitid,
        libc::SYS_exit,
        libc::SYS_exit_group,
        libc::SYS_execve,
        libc::SYS_execveat,
        libc::SYS_kill,
        libc::SYS_tgkill,
        libc::SYS_nanosleep,
        libc::SYS_clock_nanosleep,
        libc::SYS_restart_syscall,
        libc::SYS_getdents64,
        libc::SYS_mkdir,
        libc::SYS_mkdirat,
        libc::SYS_unlink,
        libc::SYS_unlinkat,
        libc::SYS_rename,
        libc::SYS_renameat,
        libc::SYS_renameat2,
        libc::SYS_truncate,
        libc::SYS_ftruncate,
        libc::SYS_fchmod,
        libc::SYS_chmod,
        libc::SYS_umask,
        libc::SYS_fsync,
        libc::SYS_fdatasync,
        libc::SYS_statfs,
        libc::SYS_fstatfs,
        libc::SYS_getrusage,
        libc::SYS_sysinfo,
        libc::SYS_prctl,
        libc::SYS_poll,
        libc::SYS_ppoll,
        libc::SYS_select,
        libc::SYS_pselect6,
    ];
    for nr in allowed {
        ins(0x15, 0, 1, nr as u32);
        ins(0x06, 0, 0, ALLOW);
    }
    ins(0x06, 0, 0, ERRNO | libc::EPERM as u32);
    // sock_filter is the Linux kernel's plain fixed-layout BPF instruction format.
    unsafe {
        std::slice::from_raw_parts(
            p.as_ptr().cast::<u8>(),
            p.len() * std::mem::size_of::<libc::sock_filter>(),
        )
        .to_vec()
    }
}

/// Runs only in a freshly exec'd launcher, never in a post-fork callback.
pub fn launch(path: &Path) -> anyhow::Result<()> {
    let cfg: Launch = serde_json::from_slice(&std::fs::read(path)?)?;
    std::fs::write(
        cfg.cgroup.join("cgroup.procs"),
        std::process::id().to_string(),
    )
    .context("join build cgroup (launcher must already be inside its delegated subtree)")?;
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            bail!("no_new_privs: {}", std::io::Error::last_os_error());
        }
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
                bail!("setrlimit: {}", std::io::Error::last_os_error());
            }
        }
        libc::umask(0o077);
    }
    let filter = File::open(&cfg.filter)?;
    // The only inherited non-stdio descriptor is the seccomp program consumed by bwrap.
    if unsafe { libc::fcntl(filter.as_raw_fd(), libc::F_SETFD, 0) } < 0 {
        bail!("fcntl: {}", std::io::Error::last_os_error());
    }
    let mut cmd = Command::new(&cfg.toolchain.bwrap.path);
    cmd.args([
        "--unshare-all",
        "--unshare-user",
        "--unshare-cgroup",
        "--disable-userns",
        "--new-session",
        "--die-with-parent",
        "--cap-drop",
        "ALL",
        "--clearenv",
        "--setenv",
        "PATH",
        "/toolchain",
        "--setenv",
        "TMPDIR",
        "/work",
        "--setenv",
        "LANG",
        "C",
        "--setenv",
        "SOURCE_DATE_EPOCH",
        "0",
    ]);
    let search = cfg
        .toolchain
        .libraries
        .keys()
        .filter_map(|p| p.parent())
        .collect::<std::collections::BTreeSet<_>>();
    let search = std::env::join_paths(search)?;
    cmd.args(["--setenv", "LD_LIBRARY_PATH"]).arg(search);
    for (dest, file) in &cfg.toolchain.libraries {
        cmd.arg("--ro-bind").arg(&file.path).arg(dest);
    }
    cmd.arg("--ro-bind")
        .arg(&cfg.runner)
        .arg("/runner")
        .arg("--ro-bind")
        .arg(&cfg.toolchain.clang.path)
        .arg("/toolchain/clang")
        .arg("--ro-bind")
        .arg(&cfg.toolchain.llc.path)
        .arg("/toolchain/llc")
        .arg("--ro-bind")
        .arg(&cfg.sources)
        .arg("/src")
        .arg("--ro-bind")
        .arg(&cfg.sdk)
        .arg("/sdk")
        .args([
            "--size",
            "16777216",
            "--tmpfs",
            "/work",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--symlink",
            "/work",
            "/tmp",
            "--chdir",
            "/work",
            "--seccomp",
        ])
        .arg(filter.as_raw_fd().to_string())
        .args(["--", "/runner", "--compiler-worker"])
        .arg(&cfg.entry);
    Err(cmd.exec().into())
}
/// The same binary, inside the completed sandbox, executes only fixed compiler commands.
pub fn worker(entry: &str) -> anyhow::Result<()> {
    if !super::valid_path(entry) {
        bail!("invalid source entry");
    }
    let source = format!("/src/{entry}");
    let status = Command::new("/toolchain/clang")
        .args([
            "-O2",
            "-Wall",
            "-target",
            "bpfel",
            "-ffreestanding",
            "-fno-builtin",
            "-fno-zero-initialized-in-bss",
            "-nostdinc",
            "-I/sdk",
            "-I/src",
            "-emit-llvm",
            "-c",
            &source,
            "-o",
            "/work/main.bc",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()?;
    if !status.success() {
        bail!("clang failed: {status}");
    }
    let status = Command::new("/toolchain/llc")
        .args([
            "-march=bpf",
            "-mcpu=v3",
            "-bpf-stack-size=4096",
            "--nozero-initialized-in-bss",
            "-filetype=obj",
            "/work/main.bc",
            "-o",
            "/work/main.o",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()?;
    if !status.success() {
        bail!("llc failed: {status}");
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open("/work/main.o")?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_ELF as u64 {
        bail!("invalid or oversized compiler output");
    }
    let mut bytes = Vec::new();
    file.take((MAX_ELF + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() > MAX_ELF {
        bail!("invalid compiler output size");
    }
    std::io::stdout().write_all(&bytes)?;
    Ok(())
}
async fn capture<R: tokio::io::AsyncRead + Unpin>(
    mut input: R,
    limit: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    use tokio::io::AsyncReadExt;
    let mut captured = Vec::new();
    let mut truncated = false;
    let mut buf = [0u8; 4096];
    loop {
        let n = input.read(&mut buf).await?;
        if n == 0 {
            return Ok((captured, truncated));
        }
        let take = n.min(limit.saturating_sub(captured.len()));
        captured.extend_from_slice(&buf[..take]);
        truncated |= take < n;
    }
}
pub struct Output {
    pub elf: Vec<u8>,
    pub diagnostics: String,
    pub truncated: bool,
}
pub async fn run(
    toolchain: &Toolchain,
    root: &Path,
    files: &std::collections::BTreeMap<String, String>,
    entry: &str,
    cancel: &CancellationToken,
) -> anyhow::Result<Output> {
    let work = tempfile::tempdir()?;
    let sources = work.path().join("src");
    let sdk = work.path().join("sdk");
    std::fs::create_dir(&sources)?;
    std::fs::create_dir(&sdk)?;
    for (name, contents) in files {
        let path = sources.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)?;
    }
    std::fs::write(sdk.join("spinfoam.h"), crate::SDK)?;
    let filter_path = work.path().join("seccomp.bpf");
    std::fs::write(&filter_path, filter())?;
    let group = Cgroup::new(root)?;
    let cfg = Launch {
        toolchain: toolchain.clone(),
        cgroup: group.path.clone(),
        sources,
        sdk,
        runner: std::env::current_exe()?,
        entry: entry.to_owned(),
        filter: filter_path,
    };
    let config_path = work.path().join("launch.json");
    std::fs::write(&config_path, serde_json::to_vec(&cfg)?)?;
    let mut child = tokio::process::Command::new(&cfg.runner)
        .arg("--sandbox-launch")
        .arg(config_path)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let out = capture(stdout, MAX_ELF);
    let err = capture(stderr, MAX_DIAGNOSTICS);
    let monitor = async {
        loop {
            tokio::time::sleep(Duration::from_millis(25)).await;
            if group.cpu_used()? > 5_000_000 {
                bail!("build exceeded 5 CPU seconds");
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };
    let result = tokio::select! {
        _=cancel.cancelled()=>Err(anyhow::anyhow!("build cancelled")),
        _=tokio::time::sleep(Duration::from_secs(15))=>Err(anyhow::anyhow!("build wall deadline exceeded")),
        result=monitor=>result.and_then(|_|Err(anyhow::anyhow!("build monitor stopped"))),
        result=async {tokio::try_join!(child.wait(),out,err)}=>result.map_err(Into::into),
    };
    group.kill();
    // Kill and reap even if a descendant held the compiler pipes open.
    let _ = child.kill().await;
    let _ = child.wait().await;
    // The kernel removes descendants asynchronously after cgroup.kill.
    for _ in 0..100 {
        if std::fs::read_to_string(group.path.join("cgroup.events"))
            .is_ok_and(|s| s.lines().any(|l| l == "populated 0"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (status, (elf, elf_truncated), (diagnostics, truncated)) = result?;
    let diagnostics = String::from_utf8_lossy(&diagnostics).into_owned();
    if !status.success() {
        bail!("compiler failed ({status}): {diagnostics}");
    }
    if elf_truncated || elf.is_empty() {
        bail!("compiler output exceeds limit or is empty");
    }
    Ok(Output {
        elf,
        diagnostics,
        truncated,
    })
}
