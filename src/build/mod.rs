#[cfg(target_os = "linux")]
pub mod sandbox;
#[cfg(not(target_os = "linux"))]
#[path = "sandbox_unavailable.rs"]
pub mod sandbox;
pub mod toolchain;
use crate::{
    error::{RpcError, RpcResult},
    outbox::Outbox,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    path::{Component, Path},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{Notify, Semaphore},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use toolchain::Toolchain;

#[derive(Clone)]
pub struct Config;
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub sdk_version: u32,
    pub files: BTreeMap<String, String>,
    pub entry: String,
}
pub fn valid_path(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.contains("//")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./-".contains(&b))
        && name
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && p != "spinfoam.h")
        && Path::new(name)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}
impl Request {
    fn validate(&self) -> Result<(), RpcError> {
        if self.sdk_version != 1 {
            return Err(RpcError::params("sdk_version must be 1"));
        }
        if self.files.is_empty()
            || self.files.len() > 32
            || self.files.values().map(String::len).sum::<usize>() > 128 * 1024
        {
            return Err(RpcError::params("source bundle exceeds limits"));
        }
        if !valid_path(&self.entry)
            || !self.files.contains_key(&self.entry)
            || self.files.keys().any(|p| !valid_path(p))
        {
            return Err(RpcError::params("invalid source path or entry"));
        }
        // Reject a file also being used as another file's parent directory.
        for name in self.files.keys() {
            for (i, _) in name.match_indices('/') {
                if self.files.contains_key(&name[..i]) {
                    return Err(RpcError::params("source file/directory conflict"));
                }
            }
        }
        Ok(())
    }
}
struct Artifact {
    bytes: Vec<u8>,
    created: Instant,
    key: String,
}
struct Job {
    id: String,
    state: Cell<&'static str>,
    result: RefCell<Value>,
    cancel: CancellationToken,
    task: RefCell<Option<JoinHandle<()>>>,
    done: Notify,
}
impl Job {
    async fn stop(&self) {
        self.cancel.cancel();
        loop {
            let done = self.done.notified();
            if !matches!(self.state.get(), "queued" | "running") {
                break;
            }
            done.await;
        }
        let task = self.task.borrow_mut().take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
    fn status(&self) -> Value {
        json!({"build_id":self.id,"state":self.state.get(),"result":*self.result.borrow()})
    }
}
pub struct Builds {
    toolchain: Option<Toolchain>,
    unavailable: Option<String>,
    fingerprint: String,
    jobs: RefCell<BTreeMap<String, Rc<Job>>>,
    artifacts: RefCell<BTreeMap<String, Artifact>>,
    next: Cell<u64>,
    slots: Arc<Semaphore>,
    out: Outbox,
}
impl Builds {
    pub async fn new(config: Option<Config>, out: Outbox) -> Rc<Self> {
        let setup = async {
            if !cfg!(target_os = "linux") {
                anyhow::bail!("sandboxed compilation currently requires Linux");
            }
            let _config = config
                .ok_or_else(|| anyhow::anyhow!("pass --enable-builds to enable compilation"))?;
            let toolchain = tokio::task::spawn_blocking(Toolchain::discover).await??;
            // A real compile probes the full sandbox, toolchain ABI and required controls.
            let files = BTreeMap::from([(
                "main.c".to_owned(),
                "#include \"spinfoam.h\"\nSF_MAIN int main(void){return 0;}".to_owned(),
            )]);
            sandbox::run(&toolchain, &files, "main.c", &CancellationToken::new()).await?;
            let fingerprint = crate::sha256(
                format!(
                    "{}:{}:{}:{}",
                    toolchain.digest(),
                    crate::ASYNC_EBPF_REVISION,
                    crate::SDK,
                    "bpfel-v3-frame4096-O2-nozero-bss-v1"
                )
                .as_bytes(),
            );
            Ok::<_, anyhow::Error>((toolchain, fingerprint))
        }
        .await;
        let (toolchain, fingerprint, unavailable) = match setup {
            Ok((t, f)) => (Some(t), f, None),
            Err(e) => {
                let reason = format!("{e:#}");
                tracing::info!(%reason,"compiler unavailable");
                (None, String::new(), Some(reason))
            }
        };
        Rc::new(Self {
            toolchain,
            unavailable,
            fingerprint,
            jobs: Default::default(),
            artifacts: Default::default(),
            next: Cell::new(0),
            slots: Arc::new(Semaphore::new(1)),
            out,
        })
    }
    pub fn info(&self) -> Value {
        json!({"available":self.toolchain.is_some(),"reason":self.unavailable,"fingerprint":self.fingerprint,"clang_version":self.toolchain.as_ref().map(|t|&t.clang_version),"llc_version":self.toolchain.as_ref().map(|t|&t.llc_version)})
    }
    pub fn submit(self: &Rc<Self>, params: Value) -> RpcResult {
        let request: Request = serde_json::from_value(params).map_err(RpcError::params)?;
        request.validate()?;
        if let Some(reason) = &self.unavailable {
            return Err(RpcError::new(-32020, "SANDBOX_UNAVAILABLE", reason));
        }
        self.expire();
        if self
            .jobs
            .borrow()
            .values()
            .filter(|j| matches!(j.state.get(), "queued" | "running"))
            .count()
            >= 16
        {
            return Err(RpcError::busy("build queue is full"));
        }
        let serial = self
            .next
            .get()
            .checked_add(1)
            .ok_or_else(|| RpcError::busy("build IDs exhausted"))?;
        self.next.set(serial);
        let id = format!("b{serial}");
        let key = crate::sha256(
            &serde_json::to_vec(&json!({"toolchain":self.fingerprint,"request":request})).unwrap(),
        );
        let hit = self
            .artifacts
            .borrow()
            .iter()
            .find(|(_, a)| a.key == key)
            .map(|(id, a)| (id.clone(), crate::sha256(&a.bytes)));
        let job = Rc::new(Job {
            id: id.clone(),
            state: Cell::new("queued"),
            result: RefCell::new(Value::Null),
            cancel: CancellationToken::new(),
            task: RefCell::new(None),
            done: Notify::new(),
        });
        if let Some((artifact, hash)) = hit {
            job.state.set("succeeded");
            job.result.replace(json!({"artifact_id":artifact,"sha256":hash,"cached":true,"sdk_version":1,"diagnostics":"","diagnostics_truncated":false}));
        } else {
            let this = self.clone();
            let task_job = job.clone();
            let task = tokio::task::spawn_local(async move {
                let job = task_job;
                let permit = tokio::select! {biased;_=job.cancel.cancelled()=>None,p=this.slots.clone().acquire_owned()=>p.ok()};
                if let Some(_permit) = permit {
                    job.state.set("running");
                    let result = sandbox::run(
                        this.toolchain.as_ref().unwrap(),
                        &request.files,
                        &request.entry,
                        &job.cancel,
                    )
                    .await;
                    if job.cancel.is_cancelled() {
                        job.state.set("cancelled");
                    } else {
                        match result {
                            Ok(mut output) => {
                                if output.diagnostics.len() > 16 * 1024 {
                                    truncate(&mut output.diagnostics, 16 * 1024);
                                    output.truncated = true;
                                }
                                let artifact_id = format!("a{}", job.id);
                                let hash = crate::sha256(&output.elf);
                                this.artifacts.borrow_mut().insert(
                                    artifact_id.clone(),
                                    Artifact {
                                        bytes: output.elf,
                                        created: Instant::now(),
                                        key,
                                    },
                                );
                                job.result.replace(json!({"artifact_id":artifact_id,"sha256":hash,"cached":false,"sdk_version":1,"diagnostics":output.diagnostics,"diagnostics_truncated":output.truncated}));
                                job.state.set("succeeded");
                                this.expire();
                            }
                            Err(e) => {
                                let mut error = format!("{e:#}");
                                truncate(&mut error, 16 * 1024);
                                job.result
                                    .replace(json!({"kind":"BUILD_FAILED","error":error}));
                                job.state.set("failed");
                            }
                        }
                    }
                } else {
                    job.state.set("cancelled");
                }
                job.done.notify_waiters();
                this.out.try_control(
                    json!({"jsonrpc":"2.0","method":"sf.build.finished","params":job.status()}),
                );
            });
            job.task.replace(Some(task));
        }
        let status = job.status();
        self.jobs.borrow_mut().insert(id, job);
        Ok(status)
    }
    fn expire(&self) {
        let mut artifacts = self.artifacts.borrow_mut();
        artifacts.retain(|_, a| a.created.elapsed() < Duration::from_secs(1800));
        while artifacts.len() > 128
            || artifacts.values().map(|a| a.bytes.len()).sum::<usize>() > 8 * 1024 * 1024
        {
            let oldest = artifacts
                .iter()
                .min_by_key(|(_, a)| a.created)
                .map(|(id, _)| id.clone())
                .unwrap();
            artifacts.remove(&oldest);
        }
        let mut jobs = self.jobs.borrow_mut();
        while jobs.len() >= 128 {
            let oldest = jobs
                .iter()
                .find(|(_, j)| !matches!(j.state.get(), "queued" | "running"))
                .map(|(id, _)| id.clone());
            if let Some(id) = oldest {
                jobs.remove(&id);
            } else {
                break;
            }
        }
    }
    pub fn status(&self, id: &str) -> RpcResult {
        Ok(self
            .jobs
            .borrow()
            .get(id)
            .ok_or_else(|| RpcError::new(-32021, "BUILD_NOT_FOUND", "unknown or expired build"))?
            .status())
    }
    pub async fn cancel(&self, id: &str) -> RpcResult {
        let job =
            self.jobs.borrow().get(id).cloned().ok_or_else(|| {
                RpcError::new(-32021, "BUILD_NOT_FOUND", "unknown or expired build")
            })?;
        job.stop().await;
        Ok(job.status())
    }
    pub fn bytes(&self, id: &str) -> Result<Vec<u8>, RpcError> {
        self.expire();
        self.artifacts
            .borrow()
            .get(id)
            .map(|a| a.bytes.clone())
            .ok_or_else(|| {
                RpcError::new(-32022, "ARTIFACT_NOT_FOUND", "unknown or expired artifact")
            })
    }
    pub fn artifact(&self, id: &str) -> RpcResult {
        let bytes = self.bytes(id)?;
        Ok(
            json!({"artifact_id":id,"sha256":crate::sha256(&bytes),"elf":STANDARD.encode(bytes),"sdk_version":1,"toolchain":self.fingerprint}),
        )
    }
    pub async fn shutdown(&self) {
        let jobs: Vec<_> = self.jobs.borrow().values().cloned().collect();
        for job in &jobs {
            job.cancel.cancel();
        }
        for job in jobs {
            job.stop().await;
        }
    }
}
fn truncate(text: &mut String, max: usize) {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}
