use crate::{
    error::{CLOSED, HOST_ERROR, LIMIT, TIMEOUT},
    outbox::Outbox,
};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    time::Duration,
};
use tokio::sync::oneshot;

struct Pending {
    object: String,
    tx: oneshot::Sender<Result<Value, i64>>,
}
#[derive(Clone)]
pub struct Host {
    pending: Rc<RefCell<HashMap<String, Pending>>>,
    next: Rc<Cell<u64>>,
    pub session: String,
    out: Outbox,
    ignored: Rc<Cell<u64>>,
}
struct CallGuard {
    host: Host,
    id: String,
    sent: bool,
    object: String,
}
impl Drop for CallGuard {
    fn drop(&mut self) {
        self.host.out.remove_request(&self.object, &self.id);
        if self.host.pending.borrow_mut().remove(&self.id).is_some() && self.sent {
            self.host.out.try_control(
                json!({"jsonrpc":"2.0","method":"host.cancel","params":{"id":self.id}}),
            );
        }
    }
}
impl Host {
    pub fn new(out: Outbox) -> Self {
        Self {
            pending: Default::default(),
            next: Rc::new(Cell::new(0)),
            session: format!("{:032x}", rand::random::<u128>()),
            out,
            ignored: Default::default(),
        }
    }
    pub fn stats(&self) -> Value {
        json!({"pending_host_calls":self.pending.borrow().len(),"ignored_host_responses":self.ignored.get()})
    }
    pub async fn call(
        &self,
        object: &str,
        capability: String,
        arguments: Value,
        timeout_ms: u64,
    ) -> Result<Value, i64> {
        if self.pending.borrow().len() >= 16384 {
            return Err(LIMIT);
        }
        let serial = self.next.get().checked_add(1).ok_or(LIMIT)?;
        self.next.set(serial);
        let id = format!("sf:{}:{serial}", self.session);
        let (tx, rx) = oneshot::channel();
        self.pending.borrow_mut().insert(
            id.clone(),
            Pending {
                object: object.to_owned(),
                tx,
            },
        );
        let mut guard = CallGuard {
            host: self.clone(),
            id: id.clone(),
            sent: false,
            object: object.to_owned(),
        };
        let request = json!({"jsonrpc":"2.0","id":id,"method":"host.call","params":{
            "object_id":object,"capability":capability,"arguments":arguments,
            "timeout_ms":timeout_ms,"deadline_unix_ms":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() + timeout_ms as u128,"max_result_bytes":crate::helpers::MAX_VALUE_BYTES
        }});
        let operation = async {
            self.out.data(object, request).await.map_err(|_| CLOSED)?;
            guard.sent = true;
            rx.await.map_err(|_| CLOSED)?
        };
        tokio::time::timeout(Duration::from_millis(timeout_ms), operation)
            .await
            .map_err(|_| TIMEOUT)?
    }
    pub fn response(&self, frame: &Value) {
        let Some(id) = frame.get("id").and_then(Value::as_str) else {
            self.ignore();
            return;
        };
        let Some(pending) = self.pending.borrow_mut().remove(id) else {
            self.ignore();
            return;
        };
        let result = if frame.get("result").is_some() && frame.get("error").is_none() {
            let value = &frame["result"];
            if crate::helpers::valid_value(value) {
                Ok(value.clone())
            } else {
                Err(LIMIT)
            }
        } else {
            Err(HOST_ERROR)
        };
        let _ = pending.tx.send(result);
    }
    fn ignore(&self) {
        self.ignored.set(self.ignored.get().saturating_add(1));
    }
    pub fn cancel_object(&self, object: &str) {
        let ids: Vec<_> = self
            .pending
            .borrow()
            .iter()
            .filter(|(_, p)| p.object == object)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(pending) = self.pending.borrow_mut().remove(&id) {
                let _ = pending.tx.send(Err(CLOSED));
                self.out.try_control(
                    json!({"jsonrpc":"2.0","method":"host.cancel","params":{"id":id}}),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn timeout_removes_a_request_that_has_not_reached_the_writer() {
        let out = Outbox::default();
        let host = Host::new(out.clone());
        assert_eq!(
            host.call("o1", "read".into(), json!({}), 1).await,
            Err(TIMEOUT)
        );
        assert_eq!(out.stats()["queued_object_bytes"], 0);
        assert_eq!(host.stats()["pending_host_calls"], 0);
        let notice: Value = serde_json::from_slice(&out.next().await.unwrap()).unwrap();
        assert_eq!(notice["method"], "host.cancel");
    }
}
