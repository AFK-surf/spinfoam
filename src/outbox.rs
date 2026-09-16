//! Bounded output with reserved control capacity and round-robin object queues.
use serde_json::Value;
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    rc::Rc,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

pub const MAX_FRAME: usize = 256 * 1024;
const CONTROL_SLOTS: usize = 64;
const DATA_BYTES: usize = 4 * 1024 * 1024;
const OBJECT_BYTES: usize = 64 * 1024;
struct DataFrame {
    bytes: Vec<u8>,
    request_id: Option<String>,
}

#[derive(Default)]
struct Queues {
    control: VecDeque<Vec<u8>>,
    data: BTreeMap<String, VecDeque<DataFrame>>,
    ready: VecDeque<String>,
    bytes: usize,
    control_burst: usize,
}
#[derive(Debug)]
pub enum SendError {
    Closed,
    FrameTooLarge,
}

#[derive(Clone)]
pub struct Outbox {
    queues: Rc<RefCell<Queues>>,
    changed: Rc<Notify>,
    space: Rc<Notify>,
    pub closed: CancellationToken,
    dropped: Rc<Cell<u64>>,
}
impl Default for Outbox {
    fn default() -> Self {
        Self {
            queues: Default::default(),
            changed: Default::default(),
            space: Default::default(),
            closed: CancellationToken::new(),
            dropped: Default::default(),
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
    pub async fn control(&self, value: Value) -> Result<(), SendError> {
        let bytes = match Self::encode(&value) {
            Ok(bytes) => bytes,
            Err(()) => {
                let id = value.get("id").cloned().ok_or(SendError::FrameTooLarge)?;
                Self::encode(&serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32003,"message":"response exceeds frame limit","data":{"kind":"RESPONSE_TOO_LARGE"}}})).map_err(|_|SendError::FrameTooLarge)?
            }
        };
        loop {
            let space = self.space.notified();
            if self.closed.is_cancelled() {
                return Err(SendError::Closed);
            }
            if self.queues.borrow().control.len() < CONTROL_SLOTS {
                self.queues.borrow_mut().control.push_back(bytes);
                self.changed.notify_one();
                return Ok(());
            }
            tokio::select! { _ = space => {}, _ = self.closed.cancelled() => return Err(SendError::Closed) }
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
        let request_id = value.get("id").and_then(Value::as_str).map(str::to_owned);
        let accepted = self.push_data(object, bytes, request_id).is_ok();
        if !accepted {
            self.dropped.set(self.dropped.get().saturating_add(1));
        }
        accepted
    }
    fn push_data(
        &self,
        object: &str,
        bytes: Vec<u8>,
        request_id: Option<String>,
    ) -> Result<(), Vec<u8>> {
        let mut q = self.queues.borrow_mut();
        let used: usize = q
            .data
            .get(object)
            .map(|v| v.iter().map(|frame| frame.bytes.len()).sum())
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
            .push_back(DataFrame { bytes, request_id });
        self.changed.notify_one();
        Ok(())
    }
    pub async fn data(&self, object: &str, value: Value) -> Result<(), SendError> {
        let mut bytes = Self::encode(&value).map_err(|_| SendError::FrameTooLarge)?;
        let request_id = value.get("id").and_then(Value::as_str).map(str::to_owned);
        if bytes.len() > OBJECT_BYTES {
            return Err(SendError::FrameTooLarge);
        }
        loop {
            let space = self.space.notified();
            if self.closed.is_cancelled() {
                return Err(SendError::Closed);
            }
            match self.push_data(object, bytes, request_id.clone()) {
                Ok(()) => return Ok(()),
                Err(b) => bytes = b,
            }
            tokio::select! { _ = space => {}, _ = self.closed.cancelled() => return Err(SendError::Closed) }
        }
    }
    pub fn stats(&self) -> Value {
        let q = self.queues.borrow();
        serde_json::json!({"queued_control_frames":q.control.len(),"queued_object_bytes":q.bytes,"dropped_data_notifications":self.dropped.get()})
    }
    pub fn remove_request(&self, object: &str, id: &str) {
        let mut q = self.queues.borrow_mut();
        if let Some(frames) = q.data.get_mut(object) {
            let mut removed = 0;
            frames.retain(|frame| {
                if frame.request_id.as_deref() == Some(id) {
                    removed += frame.bytes.len();
                    false
                } else {
                    true
                }
            });
            let empty = frames.is_empty();
            q.bytes -= removed;
            if empty {
                q.data.remove(object);
                q.ready.retain(|key| key != object);
            }
            self.space.notify_waiters();
        }
    }
    pub fn remove_object(&self, object: &str) {
        let mut q = self.queues.borrow_mut();
        if let Some(items) = q.data.remove(object) {
            q.bytes -= items.iter().map(|frame| frame.bytes.len()).sum::<usize>();
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
                    let bytes = queue.pop_front().expect("nonempty queue").bytes;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn bounded_data_does_not_block_control_and_objects_are_fair() {
        let out = Outbox::default();
        for _ in 0..3 {
            assert!(out.try_data("a", json!({"a":"x".repeat(16000)})));
        }
        assert!(!out.try_data("a", json!({"a":"x".repeat(20000)})));
        assert!(out.try_data("b", json!({"b":true})));
        out.control(json!({"control":true})).await.unwrap();
        assert!(
            String::from_utf8(out.next().await.unwrap())
                .unwrap()
                .contains("control")
        );
        assert!(
            String::from_utf8(out.next().await.unwrap())
                .unwrap()
                .contains("\"a\"")
        );
        assert!(
            String::from_utf8(out.next().await.unwrap())
                .unwrap()
                .contains("\"b\"")
        );
        out.remove_object("a");
        assert_eq!(out.stats()["queued_object_bytes"], 0);
        assert_eq!(out.stats()["dropped_data_notifications"], 1);
    }
    #[tokio::test]
    async fn oversize_response_returns_error_instead_of_losing_request() {
        let out = Outbox::default();
        out.control(json!({"jsonrpc":"2.0","id":"test","result":"x".repeat(MAX_FRAME)}))
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&out.next().await.unwrap()).unwrap();
        assert_eq!(value["id"], "test");
        assert_eq!(value["error"]["data"]["kind"], "RESPONSE_TOO_LARGE");
    }
}
