//! Bounded output with reserved control capacity and round-robin object queues.
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    rc::Rc,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

pub const MAX_FRAME: usize = 256 * 1024;
const CONTROL_SLOTS: usize = 64;
const DATA_BYTES: usize = 4 * 1024 * 1024;
const OBJECT_BYTES: usize = 64 * 1024;
#[derive(Default)]
struct Queues {
    control: VecDeque<Vec<u8>>,
    data: BTreeMap<String, VecDeque<Vec<u8>>>,
    ready: VecDeque<String>,
    bytes: usize,
    control_burst: usize,
}
#[derive(Clone)]
pub struct Outbox {
    queues: Rc<RefCell<Queues>>,
    changed: Rc<Notify>,
    space: Rc<Notify>,
    pub closed: CancellationToken,
}
impl Default for Outbox {
    fn default() -> Self {
        Self {
            queues: Default::default(),
            changed: Default::default(),
            space: Default::default(),
            closed: CancellationToken::new(),
        }
    }
}
impl Outbox {
    fn encode(value: &Value) -> Result<Vec<u8>, ()> {
        let mut bytes = serde_json::to_vec(value).map_err(|_| ())?;
        if bytes.len() >= MAX_FRAME {
            return Err(());
        }
        bytes.push(b'\n');
        Ok(bytes)
    }
    pub async fn control(&self, value: Value) -> Result<(), ()> {
        let bytes = Self::encode(&value)?;
        loop {
            let space = self.space.notified();
            if self.closed.is_cancelled() {
                return Err(());
            }
            if self.queues.borrow().control.len() < CONTROL_SLOTS {
                self.queues.borrow_mut().control.push_back(bytes);
                self.changed.notify_one();
                return Ok(());
            }
            tokio::select! { _ = space => {}, _ = self.closed.cancelled() => return Err(()) }
        }
    }
    pub fn try_control(&self, value: Value) -> bool {
        let Ok(bytes) = Self::encode(&value) else {
            return false;
        };
        let mut q = self.queues.borrow_mut();
        if self.closed.is_cancelled() || q.control.len() >= CONTROL_SLOTS {
            return false;
        }
        q.control.push_back(bytes);
        self.changed.notify_one();
        true
    }
    pub fn try_data(&self, object: &str, value: Value) -> bool {
        let Ok(bytes) = Self::encode(&value) else {
            return false;
        };
        self.push_data(object, bytes).is_ok()
    }
    fn push_data(&self, object: &str, bytes: Vec<u8>) -> Result<(), Vec<u8>> {
        let mut q = self.queues.borrow_mut();
        let used: usize = q
            .data
            .get(object)
            .map(|v| v.iter().map(Vec::len).sum())
            .unwrap_or(0);
        if self.closed.is_cancelled()
            || used + bytes.len() > OBJECT_BYTES
            || q.bytes + bytes.len() > DATA_BYTES
        {
            return Err(bytes);
        }
        if !q.data.contains_key(object) {
            q.ready.push_back(object.to_owned());
        }
        q.bytes += bytes.len();
        q.data
            .entry(object.to_owned())
            .or_default()
            .push_back(bytes);
        self.changed.notify_one();
        Ok(())
    }
    pub async fn data(&self, object: &str, value: Value) -> Result<(), ()> {
        let mut bytes = Self::encode(&value)?;
        if bytes.len() > OBJECT_BYTES {
            return Err(());
        }
        loop {
            let space = self.space.notified();
            if self.closed.is_cancelled() {
                return Err(());
            }
            match self.push_data(object, bytes) {
                Ok(()) => return Ok(()),
                Err(b) => bytes = b,
            }
            tokio::select! { _ = space => {}, _ = self.closed.cancelled() => return Err(()) }
        }
    }
    pub fn remove_object(&self, object: &str) {
        let mut q = self.queues.borrow_mut();
        if let Some(items) = q.data.remove(object) {
            q.bytes -= items.iter().map(Vec::len).sum::<usize>();
        }
        q.ready.retain(|id| id != object);
        self.space.notify_waiters();
    }
    pub async fn next(&self) -> Option<Vec<u8>> {
        loop {
            let changed = self.changed.notified();
            {
                let mut q = self.queues.borrow_mut();
                if !q.control.is_empty() && (q.control_burst < 8 || q.ready.is_empty()) {
                    q.control_burst += 1;
                    let item = q.control.pop_front();
                    self.space.notify_waiters();
                    return item;
                }
                if let Some(id) = q.ready.pop_front() {
                    let queue = q.data.get_mut(&id).expect("ready queue invariant");
                    let bytes = queue.pop_front().expect("nonempty queue");
                    if queue.is_empty() {
                        q.data.remove(&id);
                    } else {
                        q.ready.push_back(id);
                    }
                    q.bytes -= bytes.len();
                    q.control_burst = 0;
                    self.space.notify_waiters();
                    return Some(bytes);
                }
            }
            if self.closed.is_cancelled() {
                return None;
            }
            tokio::select! { _ = changed => {}, _ = self.closed.cancelled() => return None }
        }
    }
}
