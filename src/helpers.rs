//! Guest arguments are integers, never host pointers. Async helpers own their input.
use crate::{
    error::*,
    host::Host,
    outbox::Outbox,
    runtime::{Capability, Mailbox},
};
use async_ebpf::{helpers::Helper, program::HelperScope};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    collections::HashMap,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const MAX_VALUE_BYTES: usize = 16 * 1024;
const MAX_HANDLES: usize = 128;
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);
#[derive(Clone)]
enum Handle {
    Json(Value),
    Bytes(Vec<u8>),
}
pub struct GuestContext {
    id: String,
    config: Value,
    capabilities: Vec<Capability>,
    handles: HashMap<u64, Handle>,
    mailbox: Rc<Mailbox>,
    wait: Rc<Cell<&'static str>>,
    host: Host,
    out: Outbox,
    started: Instant,
    log_window: Instant,
    logs: u32,
}
impl GuestContext {
    pub fn new(
        id: String,
        config: Value,
        capabilities: Vec<Capability>,
        mailbox: Rc<Mailbox>,
        wait: Rc<Cell<&'static str>>,
        host: Host,
        out: Outbox,
    ) -> Self {
        Self {
            id,
            config,
            capabilities,
            handles: HashMap::new(),
            mailbox,
            wait,
            host,
            out,
            started: Instant::now(),
            log_window: Instant::now(),
            logs: 0,
        }
    }
    fn insert(&mut self, handle: Handle) -> u64 {
        if self.handles.len() >= MAX_HANDLES {
            return status(LIMIT);
        }
        let Ok(id) = NEXT_HANDLE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
            (n < i64::MAX as u64).then_some(n + 1)
        }) else {
            return status(LIMIT);
        };
        self.handles.insert(id, handle);
        id
    }
    fn json(&self, h: u64) -> Option<&Value> {
        match self.handles.get(&h) {
            Some(Handle::Json(v)) => Some(v),
            _ => None,
        }
    }
}
pub fn valid_value(value: &Value) -> bool {
    fn visit(v: &Value, depth: usize, count: &mut usize) -> bool {
        *count += 1;
        if depth > 32 || *count > 4096 {
            return false;
        }
        match v {
            Value::Array(a) => a.iter().all(|v| visit(v, depth + 1, count)),
            Value::Object(o) => o.values().all(|v| visit(v, depth + 1, count)),
            _ => true,
        }
    }
    visit(value, 0, &mut 0) && serde_json::to_vec(value).is_ok_and(|s| s.len() <= MAX_VALUE_BYTES)
}
fn status(value: i64) -> u64 {
    value as u64
}
fn ctx<T>(scope: &HelperScope, f: impl FnOnce(&mut GuestContext) -> T) -> Result<T, ()> {
    scope.with_resource_mut::<GuestContext, _>(|c| c.map(f))
}
fn text(scope: &HelperScope, p: u64, n: u64) -> Result<String, ()> {
    if n > MAX_VALUE_BYTES as u64 {
        return Err(());
    }
    let bytes = scope.user_memory(p, n)?;
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| ())
}
struct WaitGuard(Rc<Cell<&'static str>>);
impl Drop for WaitGuard {
    fn drop(&mut self) {
        self.0.set("none");
    }
}
fn wait(ctx: &GuestContext, reason: &'static str) -> WaitGuard {
    ctx.wait.set(reason);
    WaitGuard(ctx.wait.clone())
}
fn timeout_ms(n: u64) -> u64 {
    n.min(86_400_000)
}
fn h_sleep(s: &HelperScope, ms: u64, _: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    let guard = ctx(s, |c| wait(c, "timer"))?;
    s.post_task(async move {
        tokio::time::sleep(Duration::from_millis(timeout_ms(ms))).await;
        drop(guard);
        |_: &HelperScope| Ok(0)
    });
    Ok(0)
}
fn h_yield(s: &HelperScope, _: u64, _: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    s.post_task(async {
        tokio::task::yield_now().await;
        |_: &HelperScope| Ok(0)
    });
    Ok(0)
}
fn h_mono(s: &HelperScope, _: u64, _: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    ctx(s, |c| c.started.elapsed().as_millis() as u64)
}
fn h_unix(_: &HelperScope, _: u64, _: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64)
}
fn h_config(s: &HelperScope, _: u64, _: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    ctx(s, |c| c.insert(Handle::Json(c.config.clone())))
}
fn h_drop(s: &HelperScope, h: u64, _: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    ctx(s, |c| {
        if c.handles.remove(&h).is_some() {
            0
        } else {
            status(INVALID)
        }
    })
}
fn h_event(s: &HelperScope, ms: u64, _: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    let (mailbox, guard) = ctx(s, |c| (c.mailbox.clone(), wait(c, "event")))?;
    s.post_task(async move {
        let result =
            tokio::time::timeout(Duration::from_millis(timeout_ms(ms)), mailbox.receive()).await;
        drop(guard);
        move |s: &HelperScope| {
            ctx(s, |c| match result {
                Ok(v) => c.insert(Handle::Json(v)),
                Err(_) => status(TIMEOUT),
            })
        }
    });
    Ok(0)
}
fn h_call(s: &HelperScope, p: u64, n: u64, h: u64, ms: u64, _: u64) -> Result<u64, ()> {
    let capability = text(s, p, n)?;
    let setup = ctx(s, |c| {
        let Some(args) = c.json(h).cloned() else {
            return Err(INVALID);
        };
        if !c.capabilities.iter().any(|cap| {
            cap.name == capability && cap.arguments.iter().all(|(k, v)| args.get(k) == Some(v))
        }) {
            return Err(DENIED);
        }
        Ok((c.id.clone(), c.host.clone(), args, wait(c, "host_rpc")))
    })?;
    let (id, host, args, guard) = match setup {
        Ok(v) => v,
        Err(e) => return Ok(status(e)),
    };
    s.post_task(async move {
        let result = host.call(&id, capability, args, timeout_ms(ms)).await;
        drop(guard);
        move |s: &HelperScope| {
            ctx(s, |c| match result {
                Ok(v) => c.insert(Handle::Json(v)),
                Err(e) => status(e),
            })
        }
    });
    Ok(0)
}
fn h_log(s: &HelperScope, p: u64, n: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    if n > 1024 {
        return Ok(status(LIMIT));
    }
    let message = text(s, p, n)?;
    ctx(s, |c| {
        if c.log_window.elapsed() >= Duration::from_secs(1) {
            c.log_window = Instant::now();
            c.logs = 0;
        }
        if c.logs >= 10 {
            return status(LIMIT);
        }
        c.logs += 1;
        if c.out.try_data(&c.id,json!({"jsonrpc":"2.0","method":"sf.log","params":{"object_id":c.id,"message":message}})) {0} else {status(LIMIT)}
    })
}
fn h_parse(s: &HelperScope, p: u64, n: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    if n > MAX_VALUE_BYTES as u64 {
        return Ok(status(LIMIT));
    }
    let input = text(s, p, n)?;
    let Ok(value) = serde_json::from_str::<Value>(&input) else {
        return Ok(status(INVALID));
    };
    if !valid_value(&value) {
        return Ok(status(LIMIT));
    }
    ctx(s, |c| c.insert(Handle::Json(value)))
}
fn h_get(s: &HelperScope, h: u64, p: u64, n: u64, _: u64, _: u64) -> Result<u64, ()> {
    let key = text(s, p, n)?;
    ctx(s, |c| match c.json(h).and_then(|v| v.get(&key)).cloned() {
        Some(v) => c.insert(Handle::Json(v)),
        None => status(INVALID),
    })
}
fn h_at(s: &HelperScope, h: u64, i: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    ctx(s, |c| {
        match usize::try_from(i)
            .ok()
            .and_then(|i| c.json(h)?.get(i))
            .cloned()
        {
            Some(v) => c.insert(Handle::Json(v)),
            None => status(INVALID),
        }
    })
}
fn h_kind(s: &HelperScope, h: u64, _: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    ctx(s, |c| match c.json(h) {
        Some(Value::Null) => 0,
        Some(Value::Bool(_)) => 1,
        Some(Value::Number(_)) => 2,
        Some(Value::String(_)) => 3,
        Some(Value::Array(_)) => 4,
        Some(Value::Object(_)) => 5,
        None => status(INVALID),
    })
}
fn h_i64(s: &HelperScope, h: u64, p: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    let value = ctx(s, |c| c.json(h).and_then(Value::as_i64))?;
    let Some(value) = value else {
        return Ok(status(INVALID));
    };
    s.user_memory_mut(p, 8)?
        .copy_from_slice(&value.to_le_bytes());
    Ok(0)
}
fn h_bool(s: &HelperScope, h: u64, _: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    ctx(s, |c| {
        c.json(h)
            .and_then(Value::as_bool)
            .map(|v| v as u64)
            .unwrap_or(status(INVALID))
    })
}
fn h_string_read(s: &HelperScope, h: u64, p: u64, n: u64, _: u64, _: u64) -> Result<u64, ()> {
    let value = ctx(s, |c| c.json(h).and_then(Value::as_str).map(str::to_owned))?;
    let Some(value) = value else {
        return Ok(status(INVALID));
    };
    if n > MAX_VALUE_BYTES as u64 {
        return Ok(status(LIMIT));
    }
    let mut out = s.user_memory_mut(p, n)?;
    let len = out.len().min(value.len());
    out[..len].copy_from_slice(&value.as_bytes()[..len]);
    Ok(value.len() as u64)
}
fn h_equals(s: &HelperScope, h: u64, kp: u64, kn: u64, vp: u64, vn: u64) -> Result<u64, ()> {
    let key = text(s, kp, kn)?;
    let value = text(s, vp, vn)?;
    ctx(s, |c| {
        c.json(h)
            .and_then(|v| v.get(key))
            .and_then(Value::as_str)
            .map(|v| (v == value) as u64)
            .unwrap_or(status(INVALID))
    })
}
fn h_object(s: &HelperScope, _: u64, _: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    ctx(s, |c| c.insert(Handle::Json(json!({}))))
}
fn h_array(s: &HelperScope, _: u64, _: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    ctx(s, |c| c.insert(Handle::Json(json!([]))))
}
fn h_null(s: &HelperScope, _: u64, _: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    ctx(s, |c| c.insert(Handle::Json(Value::Null)))
}
fn h_string(s: &HelperScope, p: u64, n: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    let value = Value::String(text(s, p, n)?);
    if !valid_value(&value) {
        return Ok(status(LIMIT));
    }
    ctx(s, |c| c.insert(Handle::Json(value)))
}
fn h_number(s: &HelperScope, n: u64, _: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    ctx(s, |c| c.insert(Handle::Json(json!(n as i64))))
}
fn h_boolean(s: &HelperScope, n: u64, _: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    ctx(s, |c| c.insert(Handle::Json(json!(n != 0))))
}
fn h_set(s: &HelperScope, h: u64, p: u64, n: u64, value: u64, _: u64) -> Result<u64, ()> {
    let key = text(s, p, n)?;
    ctx(s, |c| {
        let Some(value) = c.json(value).cloned() else {
            return status(INVALID);
        };
        let Some(mut target) = c.json(h).cloned() else {
            return status(INVALID);
        };
        let Some(map) = target.as_object_mut() else {
            return status(INVALID);
        };
        map.insert(key, value);
        if !valid_value(&target) {
            return status(LIMIT);
        }
        c.handles.insert(h, Handle::Json(target));
        0
    })
}
fn h_push(s: &HelperScope, h: u64, value: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    ctx(s, |c| {
        let Some(value) = c.json(value).cloned() else {
            return status(INVALID);
        };
        let Some(mut target) = c.json(h).cloned() else {
            return status(INVALID);
        };
        let Some(array) = target.as_array_mut() else {
            return status(INVALID);
        };
        array.push(value);
        if !valid_value(&target) {
            return status(LIMIT);
        }
        c.handles.insert(h, Handle::Json(target));
        0
    })
}
fn h_dump(s: &HelperScope, h: u64, _: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    ctx(s, |c| {
        match c.json(h).and_then(|v| serde_json::to_vec(v).ok()) {
            Some(bytes) => c.insert(Handle::Bytes(bytes)),
            None => status(INVALID),
        }
    })
}
fn h_bytes(s: &HelperScope, p: u64, n: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    if n > MAX_VALUE_BYTES as u64 {
        return Ok(status(LIMIT));
    }
    let bytes = s.user_memory(p, n)?.to_vec();
    ctx(s, |c| c.insert(Handle::Bytes(bytes)))
}
fn h_len(s: &HelperScope, h: u64, _: u64, _: u64, _: u64, _: u64) -> Result<u64, ()> {
    ctx(s, |c| match c.handles.get(&h) {
        Some(Handle::Bytes(b)) => b.len() as u64,
        Some(Handle::Json(Value::String(v))) => v.len() as u64,
        Some(Handle::Json(Value::Array(v))) => v.len() as u64,
        Some(Handle::Json(Value::Object(v))) => v.len() as u64,
        _ => status(INVALID),
    })
}
fn h_read(s: &HelperScope, h: u64, offset: u64, p: u64, n: u64, _: u64) -> Result<u64, ()> {
    if n > MAX_VALUE_BYTES as u64 {
        return Ok(status(LIMIT));
    }
    let bytes = ctx(s, |c| match c.handles.get(&h) {
        Some(Handle::Bytes(b)) => Some(b.clone()),
        _ => None,
    })?;
    let Some(bytes) = bytes else {
        return Ok(status(INVALID));
    };
    let Ok(offset) = usize::try_from(offset) else {
        return Ok(status(INVALID));
    };
    if offset > bytes.len() {
        return Ok(status(INVALID));
    }
    let len = (n as usize).min(bytes.len() - offset);
    s.user_memory_mut(p, len as u64)?
        .copy_from_slice(&bytes[offset..offset + len]);
    Ok(len as u64)
}
fn h_memcpy(s: &HelperScope, d: u64, p: u64, n: u64, _: u64, _: u64) -> Result<u64, ()> {
    if n > MAX_VALUE_BYTES as u64 {
        return Err(());
    }
    let bytes = s.user_memory(p, n)?.to_vec();
    s.user_memory_mut(d, n)?.copy_from_slice(&bytes);
    Ok(d)
}
fn h_memset(s: &HelperScope, p: u64, v: u64, n: u64, _: u64, _: u64) -> Result<u64, ()> {
    if n > MAX_VALUE_BYTES as u64 {
        return Err(());
    }
    s.user_memory_mut(p, n)?.fill(v as u8);
    Ok(p)
}
fn h_memcmp(s: &HelperScope, a: u64, b: u64, n: u64, _: u64, _: u64) -> Result<u64, ()> {
    if n > MAX_VALUE_BYTES as u64 {
        return Err(());
    }
    Ok(status(
        match s.user_memory(a, n)?.cmp(s.user_memory(b, n)?) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        },
    ))
}
pub static HELPERS: &[(&str, Helper)] = &[
    ("sf_sleep_ms", h_sleep),
    ("sf_yield", h_yield),
    ("sf_now_mono_ms", h_mono),
    ("sf_now_unix_ms", h_unix),
    ("sf_config", h_config),
    ("sf_drop", h_drop),
    ("sf_event_next", h_event),
    ("sf_host_call_raw", h_call),
    ("sf_log_raw", h_log),
    ("sf_json_parse", h_parse),
    ("sf_json_get_raw", h_get),
    ("sf_json_at", h_at),
    ("sf_json_kind", h_kind),
    ("sf_json_i64", h_i64),
    ("sf_json_bool", h_bool),
    ("sf_json_read_string", h_string_read),
    ("sf_json_string_equals_raw", h_equals),
    ("sf_json_object", h_object),
    ("sf_json_array", h_array),
    ("sf_json_null", h_null),
    ("sf_json_string_raw", h_string),
    ("sf_json_number", h_number),
    ("sf_json_boolean", h_boolean),
    ("sf_json_set_raw", h_set),
    ("sf_json_push", h_push),
    ("sf_json_dump", h_dump),
    ("sf_bytes", h_bytes),
    ("sf_bytes_len", h_len),
    ("sf_bytes_read", h_read),
    ("memcpy", h_memcpy),
    ("memset", h_memset),
    ("memcmp", h_memcmp),
];
