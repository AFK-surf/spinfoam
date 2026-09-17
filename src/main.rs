use clap::Parser;
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Print the self-contained C SDK and exit.
    #[arg(long)]
    dump_sdk: bool,
    /// Enable C builds using the embedded TinyCC eBPF compiler.
    #[arg(long)]
    enable_builds: bool,
}
fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.dump_sdk {
        print!("{}", spinfoam::SDK);
        return Ok(());
    }
    if !cfg!(all(
        any(target_os = "linux", target_os = "macos"),
        any(target_arch = "x86_64", target_arch = "aarch64")
    )) {
        anyhow::bail!("spinfoam requires Linux or macOS on x86_64 or aarch64");
    }
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let config = args.enable_builds.then_some(spinfoam::build::Config);
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
