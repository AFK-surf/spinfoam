//! The compiler is a guest with a memory-only filesystem and no host capabilities.
use crate::runtime::Runtime;
use async_ebpf::{
    helpers::Helper,
    program::{
        DummyProgramEventListener, HelperScope, PreemptionEnabled, ProgramLoader, TimesliceConfig,
        Timeslicer,
    },
};
use std::{any::Any, collections::BTreeMap, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub const OBJECT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/compiler.bpf"));
pub const REVISION: &str = "120e1619c7b3913e9ec2a10d06e431d17b23648e";
const STACK: usize = 8 * 1024 * 1024;
const MAX_OUTPUT: usize = 64 * 1024;
#[derive(Default)]
struct Context {
    input: Vec<u8>,
    files: BTreeMap<String, Vec<u8>>,
    open: BTreeMap<u64, (String, usize)>,
    next: u64,
    output: Vec<u8>,
    error: String,
    diagnostics: String,
    truncated: bool,
}
pub struct Output {
    pub elf: Vec<u8>,
    pub diagnostics: String,
    pub truncated: bool,
}
fn input(scope: &HelperScope, ptr: u64, len: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    let mut dst = scope.user_memory_mut(ptr, len)?;
    scope.with_resource_mut::<Context, _>(|ctx| {
        let ctx = ctx?;
        if dst.len() != ctx.input.len() {
            return Err(());
        }
        dst.copy_from_slice(&ctx.input);
        Ok(len)
    })
}
fn write(scope: &HelperScope, ptr: u64, len: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    let bytes = scope.user_memory(ptr, len)?;
    scope.with_resource_mut::<Context, _>(|ctx| {
        let ctx = ctx?;
        if len > MAX_OUTPUT.saturating_sub(ctx.output.len()) as u64 {
            ctx.error = "compiler output exceeds 64 KiB".into();
            return Err(());
        }
        ctx.output.extend_from_slice(bytes);
        Ok(len)
    })
}
fn fatal(scope: &HelperScope, ptr: u64, len: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    let bytes = scope.user_memory(ptr, len.min(4096))?;
    scope.with_resource_mut::<Context, _>(|ctx| {
        ctx?.error = String::from_utf8_lossy(bytes).into_owned();
        Err(())
    })
}
fn diagnostic(scope: &HelperScope, ptr: u64, len: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    let bytes = scope.user_memory(ptr, len.min(16 * 1024))?;
    scope.with_resource_mut::<Context, _>(|ctx| {
        let ctx = ctx?;
        let mut message = String::from_utf8_lossy(bytes).into_owned();
        message.push('\n');
        let remaining = (16 * 1024usize).saturating_sub(ctx.diagnostics.len());
        ctx.truncated |= len > 16 * 1024 || message.len() > remaining;
        super::truncate(&mut message, remaining);
        ctx.diagnostics.push_str(&message);
        Ok(len)
    })
}
fn virtual_path(name: &str) -> Option<String> {
    if !name.starts_with('/') {
        return None;
    }
    let mut parts = Vec::new();
    for part in name.split('/') {
        match part {
            "" | "." => (),
            ".." => {
                if parts.len() <= 1 {
                    return None;
                }
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    (parts.first() == Some(&"src")).then(|| format!("/{}", parts.join("/")))
}
fn open(scope: &HelperScope, ptr: u64, len: u64, flags: u64, _: u64, _: u64) -> Result<u64, ()> {
    if len > 256 || flags != 0 {
        return Ok(u64::MAX);
    }
    let bytes = scope.user_memory(ptr, len)?;
    let Some(name) = virtual_path(std::str::from_utf8(bytes).map_err(|_| ())?) else {
        return Ok(u64::MAX);
    };
    scope.with_resource_mut::<Context, _>(|ctx| {
        let ctx = ctx?;
        if !ctx.files.contains_key(&name) || ctx.open.len() >= 64 {
            return Ok(u64::MAX);
        }
        ctx.next = ctx
            .next
            .checked_add(1)
            .filter(|n| *n <= i32::MAX as u64 - 2)
            .ok_or(())?;
        let fd = ctx.next + 2;
        ctx.open.insert(fd, (name.to_owned(), 0));
        Ok(fd)
    })
}
fn read(scope: &HelperScope, fd: u64, ptr: u64, len: u64, _: u64, _: u64) -> Result<u64, ()> {
    let mut dst = scope.user_memory_mut(ptr, len)?;
    scope.with_resource_mut::<Context, _>(|ctx| {
        let ctx = ctx?;
        let Some((name, offset)) = ctx.open.get_mut(&fd) else {
            return Ok(u64::MAX);
        };
        let bytes = &ctx.files[name];
        let n = dst.len().min(bytes.len() - *offset);
        dst[..n].copy_from_slice(&bytes[*offset..*offset + n]);
        *offset += n;
        Ok(n as u64)
    })
}
fn close(scope: &HelperScope, fd: u64, _: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    scope.with_resource_mut::<Context, _>(|ctx| {
        ctx?.open.remove(&fd);
        Ok(0)
    })
}
const HELPERS: &[(&str, Helper)] = &[
    ("tcc_ebpf_input_copy", input),
    ("tcc_ebpf_write", write),
    ("tcc_ebpf_fatal", fatal),
    ("tcc_ebpf_open_file", open),
    ("tcc_ebpf_read_file", read),
    ("tcc_ebpf_close_file", close),
    ("tcc_ebpf_diagnostic", diagnostic),
];
// Compilation admission belongs to the embedder, including loader/JIT work.
struct CompilerTimeslicer;
impl Timeslicer for CompilerTimeslicer {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
    async fn yield_now(&self) {
        tokio::task::yield_now().await;
    }
    async fn run_blocking<T: Send + 'static>(&self, f: impl FnOnce() -> T + Send + 'static) -> T {
        tokio::task::spawn_blocking(f)
            .await
            .expect("compiler worker panicked")
    }
}
pub async fn run(
    runtime: &Runtime,
    files: &BTreeMap<String, String>,
    entry: &str,
    cancel: &CancellationToken,
) -> anyhow::Result<Output> {
    let operation = async {
        // Share only immutable ELF bytes. Each request loads a new program:
        // async-ebpf forbids concurrent calls on a program with writable data.
        let timeslicer = CompilerTimeslicer;
        let loaded = timeslicer
            .run_blocking(|| {
                ProgramLoader::new(
                    &mut rand::thread_rng(),
                    Arc::new(DummyProgramEventListener),
                    &[HELPERS],
                )
                .with_guarded_stack_frames(false)
                .with_guest_stack_size(STACK)
                .with_instruction_limit(1_000_000)
                .with_code_size_limit(64 * 1024 * 1024)
                .load(&mut rand::thread_rng(), OBJECT)
            })
            .await?;
        let program = loaded.pin_to_current_thread(runtime.thread);
        let mut ctx = Context {
            input: format!("/src/{entry}").into_bytes(),
            files: files
                .iter()
                .map(|(k, v)| (format!("/src/{k}"), v.as_bytes().to_vec()))
                .collect(),
            ..Default::default()
        };
        ctx.input.push(0);
        ctx.files
            .insert("/src/spinfoam.h".into(), crate::SDK.as_bytes().to_vec());
        let mut calldata = [0u8; 512];
        calldata[..8].copy_from_slice(&(ctx.input.len() as u64).to_le_bytes());
        let preemption = PreemptionEnabled::new(runtime.thread);
        let mut resources: [&mut dyn Any; 1] = [&mut ctx];
        let result = program
            .run_mut(
                &TimesliceConfig {
                    max_run_time_before_yield: Duration::from_millis(1),
                    max_run_time_before_throttle: Duration::from_millis(20),
                    throttle_duration: Duration::from_millis(20),
                },
                &timeslicer,
                ".text",
                &mut resources,
                &calldata,
                &preemption,
            )
            .await;
        anyhow::ensure!(ctx.error.is_empty(), "{}", ctx.error);
        let code = result?;
        anyhow::ensure!(
            code == 1 && ctx.output.starts_with(b"\x7fELF"),
            "compiler failed ({code:#x})"
        );
        Ok(Output {
            elf: ctx.output,
            diagnostics: ctx.diagnostics,
            truncated: ctx.truncated,
        })
    };
    tokio::select! {biased;_=cancel.cancelled()=>anyhow::bail!("build cancelled"),result=tokio::time::timeout(Duration::from_secs(15),operation)=>result.map_err(|_|anyhow::anyhow!("compiler exceeded 15 second deadline"))?}
}
