use clap::Parser;
use std::path::PathBuf;
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Print the self-contained C SDK and exit.
    #[arg(long)]
    dump_sdk: bool,
    /// Discover local clang/llc/bwrap and write a manifest pinning their files and libraries.
    #[arg(long)]
    write_toolchain_manifest: Option<PathBuf>,
    /// Pinned compiler toolchain manifest. Both compiler options are required to enable builds.
    #[arg(long, requires = "compiler_cgroup")]
    toolchain_manifest: Option<PathBuf>,
    /// Empty, delegated cgroup v2 subtree with cpu, memory and pids controllers enabled.
    #[arg(long, requires = "toolchain_manifest")]
    compiler_cgroup: Option<PathBuf>,
    #[arg(long, hide = true)]
    sandbox_launch: Option<PathBuf>,
    #[arg(long, hide = true)]
    compiler_worker: Option<String>,
}
fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if let Some(path) = args.sandbox_launch {
        return spinfoam::build::sandbox::launch(&path);
    }
    if let Some(entry) = args.compiler_worker {
        return spinfoam::build::sandbox::worker(&entry);
    }
    if args.dump_sdk {
        print!("{}", spinfoam::SDK);
        return Ok(());
    }
    if let Some(path) = args.write_toolchain_manifest {
        let manifest = spinfoam::build::toolchain::Toolchain::discover()?;
        std::fs::write(path, serde_json::to_vec_pretty(&manifest)?)?;
        return Ok(());
    }
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        anyhow::bail!("spinfoam currently qualifies Linux x86_64 only");
    }
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let config = args
        .toolchain_manifest
        .zip(args.compiler_cgroup)
        .map(|(manifest, cgroup)| spinfoam::build::Config { manifest, cgroup });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()?;
    let local = tokio::task::LocalSet::new();
    let result = local.block_on(&runtime, async {
        let input = spinfoam::stdio::Stdio::new(0)?;
        let output = spinfoam::stdio::Stdio::new(1)?;
        spinfoam::protocol::serve_with_config(input, output, config).await
    });
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    result
}
