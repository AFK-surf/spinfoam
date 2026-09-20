use crate::{
    error::{RpcError, RpcResult},
    helpers::{GuestContext, HELPERS, MAX_VALUE_BYTES},
    host::Host,
    outbox::Outbox,
};
use async_ebpf::program::{
    DummyProgramEventListener, GlobalEnv, PreemptionEnabled, Program, ProgramLoader, ThreadEnv,
    TimesliceConfig, Timeslicer,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{Notify, Semaphore},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct TokioTimeslicer {
    pub workers: Arc<Semaphore>,
}
impl Timeslicer for TokioTimeslicer {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await
    }
    async fn yield_now(&self) {
        tokio::task::yield_now().await
    }
    async fn run_blocking<T: Send + 'static>(&self, f: impl FnOnce() -> T + Send + 'static) -> T {
        let permit = self
            .workers
            .clone()
            .acquire_owned()
            .await
            .expect("worker semaphore stays open");
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            f()
        })
        .await
        .expect("JIT worker panicked")
    }
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub name: String,
    /// Top-level argument fields whose values must match exactly.
    #[serde(default)]
    pub arguments: BTreeMap<String, Value>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Loaded,
    Starting,
    Running,
    Stopping,
    Stopped,
    Exited,
    Failed,
}
#[derive(Default)]
pub struct Mailbox {
    entries: RefCell<VecDeque<(Value, usize)>>,
    bytes: Cell<usize>,
    recent: RefCell<VecDeque<String>>,
    pub changed: Notify,
}
impl Mailbox {
    pub fn deliver(&self, event: Value) -> RpcResult {
        let event_id = event["event_id"]
            .as_str()
            .ok_or_else(|| RpcError::params("event_id must be a string"))?;
        if event_id.len() > 128 {
            return Err(RpcError::params("event_id too long"));
        }
        if self.recent.borrow().iter().any(|x| x == event_id) {
            return Ok(json!({"accepted":true,"duplicate":true}));
        }
        let bytes = serde_json::to_vec(&event).map_err(RpcError::params)?.len();
        if bytes > MAX_VALUE_BYTES {
            return Err(RpcError::params("event exceeds 16 KiB"));
        }
        if self.entries.borrow().len() >= 32 || self.bytes.get() + bytes > 32 * 1024 {
            return Err(RpcError::new(-32002, "MAILBOX_FULL", "mailbox is full"));
        }
        self.entries.borrow_mut().push_back((event.clone(), bytes));
        self.bytes.set(self.bytes.get() + bytes);
        let mut recent = self.recent.borrow_mut();
        if recent.len() == 128 {
            recent.pop_front();
        }
        recent.push_back(event_id.to_owned());
        self.changed.notify_one();
        Ok(json!({"accepted":true,"duplicate":false}))
    }
    pub async fn receive(&self) -> Value {
        loop {
            let ready = self.changed.notified();
            if let Some((value, size)) = self.entries.borrow_mut().pop_front() {
                self.bytes.set(self.bytes.get() - size);
                return value;
            }
            ready.await;
        }
    }
    fn clear(&self) {
        self.entries.borrow_mut().clear();
        self.bytes.set(0);
        self.recent.borrow_mut().clear();
    }
    pub fn len(&self) -> usize {
        self.entries.borrow().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
pub struct Object {
    pub id: String,
    pub hash: String,
    pub state: Cell<State>,
    program: RefCell<Option<Rc<Program>>>,
    config: RefCell<Value>,
    capabilities: Vec<Capability>,
    pub mailbox: Rc<Mailbox>,
    pub wait: Rc<Cell<&'static str>>,
    pub cancel: CancellationToken,
    done: Notify,
    task: RefCell<Option<JoinHandle<()>>>,
    outcome: RefCell<Option<Value>>,
}
impl Object {
    pub fn status(&self) -> Value {
        json!({"object_id":self.id,"sha256":self.hash,"state":self.state.get(),"wait":self.wait.get(),"mailbox_entries":self.mailbox.len(),"outcome":*self.outcome.borrow()})
    }
    pub fn deliver(&self, value: Value) -> RpcResult {
        if !matches!(
            self.state.get(),
            State::Loaded | State::Starting | State::Running
        ) {
            return Err(RpcError::busy("object no longer accepts events"));
        }
        self.mailbox.deliver(value)
    }
    pub async fn stop(&self) {
        if self.state.get() == State::Loaded {
            self.state.set(State::Stopped);
        }
        if matches!(
            self.state.get(),
            State::Starting | State::Running | State::Stopping
        ) {
            self.state.set(State::Stopping);
            self.cancel.cancel();
            loop {
                let done = self.done.notified();
                if self.state.get() != State::Stopping {
                    break;
                }
                done.await;
            }
        }
        // This future must not keep a RefMut across await: concurrent stops are valid.
        let task = self.task.borrow_mut().take();
        if let Some(task) = task {
            let _ = task.await;
        }
        self.program.borrow_mut().take();
        self.config.replace(Value::Null);
        self.mailbox.clear();
    }
}
pub struct Runtime {
    pub objects: RefCell<BTreeMap<String, Rc<Object>>>,
    pub host: Host,
    pub out: Outbox,
    pub timeslicer: TokioTimeslicer,
    pub(crate) thread: ThreadEnv,
    next: Cell<u64>,
    pub started: Instant,
}
impl Runtime {
    pub fn new(out: Outbox) -> Rc<Self> {
        // SAFETY: spinfoam is a standalone executable; these signals are reserved for async-ebpf.
        let global = unsafe { GlobalEnv::new() };
        let thread = global.init_thread(Duration::from_millis(1));
        Rc::new(Self {
            objects: Default::default(),
            host: Host::new(out.clone()),
            out,
            timeslicer: TokioTimeslicer {
                workers: Arc::new(Semaphore::new(2)),
            },
            thread,
            next: Cell::new(0),
            started: Instant::now(),
        })
    }
    pub async fn load(
        &self,
        bytes: Vec<u8>,
        config: Value,
        capabilities: Vec<Capability>,
    ) -> RpcResult {
        if !crate::helpers::valid_value(&config) {
            return Err(RpcError::params("config exceeds JSON limits"));
        }
        if capabilities.iter().any(|c| {
            c.name.is_empty()
                || c.name.len() > 128
                || !crate::helpers::valid_value(&json!(c.arguments))
        }) {
            return Err(RpcError::params("invalid capabilities"));
        }
        let serial = self
            .next
            .get()
            .checked_add(1)
            .ok_or_else(|| RpcError::busy("object IDs exhausted"))?;
        self.next.set(serial);
        let hash = crate::sha256(&bytes);
        let loaded = self
            .timeslicer
            .run_blocking(move || {
                let loader = ProgramLoader::new(
                    &mut rand::thread_rng(),
                    Arc::new(DummyProgramEventListener),
                    &[HELPERS],
                );
                loader.load(&mut rand::thread_rng(), &bytes)
            })
            .await
            .map_err(|e| RpcError::new(-32010, "INVALID_OBJECT", e))?;
        let program = loaded.pin_to_current_thread(self.thread);
        if !program.has_section("spinfoam.main") {
            return Err(RpcError::params("missing spinfoam.main section"));
        }
        let id = format!("o{serial}");
        let object = Rc::new(Object {
            id: id.clone(),
            hash,
            state: Cell::new(State::Loaded),
            program: RefCell::new(Some(Rc::new(program))),
            config: RefCell::new(config),
            capabilities,
            mailbox: Default::default(),
            wait: Rc::new(Cell::new("none")),
            cancel: CancellationToken::new(),
            done: Notify::new(),
            task: RefCell::new(None),
            outcome: RefCell::new(None),
        });
        let result = object.status();
        self.objects.borrow_mut().insert(id, object);
        Ok(result)
    }
    pub fn get(&self, id: &str) -> Result<Rc<Object>, RpcError> {
        self.objects
            .borrow()
            .get(id)
            .cloned()
            .ok_or_else(RpcError::missing)
    }
    pub fn start(self: &Rc<Self>, id: &str) -> RpcResult {
        let object = self.get(id)?;
        if object.state.get() != State::Loaded {
            return Err(RpcError::busy("object can only start once"));
        }
        let program = object
            .program
            .borrow()
            .as_ref()
            .expect("loaded program")
            .clone();
        object.state.set(State::Starting);
        let rt = self.clone();
        let task_object = object.clone();
        let task = tokio::task::spawn_local(async move {
            let obj = task_object;
            let mut ctx = GuestContext::new(
                obj.id.clone(),
                obj.config.replace(Value::Null),
                obj.capabilities.clone(),
                obj.mailbox.clone(),
                obj.wait.clone(),
                rt.host.clone(),
                rt.out.clone(),
            );
            let mut resources: [&mut dyn Any; 1] = [&mut ctx];
            let timeslice = TimesliceConfig {
                max_run_time_before_yield: Duration::from_millis(1),
                max_run_time_before_throttle: Duration::from_millis(20),
                throttle_duration: Duration::from_millis(20),
            };
            let preemption = PreemptionEnabled::new(rt.thread);
            obj.state.set(State::Running);
            let result = tokio::select! {
                biased;
                _ = obj.cancel.cancelled() => None,
                result = program.run_mut(&timeslice, &rt.timeslicer, "spinfoam.main", &mut resources, &[], &preemption) => Some(result),
            };
            match result {
                None => {
                    obj.state.set(State::Stopped);
                }
                Some(Ok(code)) => {
                    obj.state.set(State::Exited);
                    obj.outcome.replace(Some(json!({"exit_code":code})));
                }
                Some(Err(error)) => {
                    obj.state.set(State::Failed);
                    obj.outcome.replace(Some(
                        json!({"error":error.to_string(),"kind":"PROGRAM_FAULT"}),
                    ));
                }
            }
            obj.wait.set("none");
            rt.host.cancel_object(&obj.id);
            rt.out.remove_object(&obj.id);
            obj.mailbox.clear();
            rt.out.try_control(
                json!({"jsonrpc":"2.0","method":"sf.object.state","params":obj.status()}),
            );
            obj.done.notify_waiters();
        });
        object.task.replace(Some(task));
        Ok(object.status())
    }
    pub async fn stop(&self, id: &str, unload: bool) -> RpcResult {
        let object = match self.get(id) {
            Ok(o) => o,
            Err(_) if unload => return Ok(json!({"unloaded":true})),
            Err(e) => return Err(e),
        };
        object.stop().await;
        self.host.cancel_object(id);
        self.out.remove_object(id);
        if unload {
            self.objects.borrow_mut().remove(id);
            Ok(json!({"unloaded":true}))
        } else {
            Ok(object.status())
        }
    }
    pub async fn shutdown(&self) {
        let objects: Vec<_> = self.objects.borrow().values().cloned().collect();
        for o in &objects {
            o.cancel.cancel();
        }
        for o in objects {
            o.stop().await;
        }
        self.objects.borrow_mut().clear();
    }
}
