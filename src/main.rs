use clap::Parser;
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Print the self-contained C SDK and exit.
    #[arg(long)]
    dump_sdk: bool,
}
fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.dump_sdk {
        print!("{}", spinfoam::SDK);
        return Ok(());
    }
    if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        anyhow::bail!("spinfoam currently qualifies Linux x86_64 only");
    }
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()?;
    let local = tokio::task::LocalSet::new();
    let result = local.block_on(&runtime, async {
        let input = spinfoam::stdio::Stdio::new(0)?;
        let output = spinfoam::stdio::Stdio::new(1)?;
        spinfoam::protocol::serve(input, output).await
    });
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    result
}
