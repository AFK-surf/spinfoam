use crate::{
    error::{RpcError, RpcResult},
    outbox::{MAX_FRAME, Outbox},
    runtime::{Capability, Runtime},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{cell::Cell, cell::RefCell, collections::HashSet, rc::Rc, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    sync::Semaphore,
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

pub struct Service {
    pub runtime: Rc<Runtime>,
    pub builds: Rc<crate::build::Builds>,
    initialized: Cell<bool>,
    pub shutdown: CancellationToken,
}
impl Service {
    pub async fn new(out: Outbox, config: Option<crate::build::Config>) -> Rc<Self> {
        let runtime = Runtime::new(out.clone());
        let builds = crate::build::Builds::new(config, runtime.clone()).await;
        Rc::new(Self {
            runtime,
            builds,
            initialized: Cell::new(false),
            shutdown: CancellationToken::new(),
        })
    }
    pub async fn handle(self: &Rc<Self>, method: &str, params: Value) -> RpcResult {
        if method == "sf.initialize" {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Init {
                protocol_version: u32,
            }
            let init: Init = decode(params)?;
            if init.protocol_version != 1 {
                return Err(RpcError::params("supported protocol_version is 1"));
            }
            if self.initialized.replace(true) {
                return Err(RpcError::busy("already initialized"));
            }
            return Ok(
                json!({"protocol_version":1,"sdk_version":1,"session_id":self.runtime.host.session,
                "target":format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),"async_ebpf_revision":crate::ASYNC_EBPF_REVISION,
                "compiler":self.builds.info(),
                "limits":{"frame_bytes":MAX_FRAME,"value_bytes":crate::helpers::MAX_VALUE_BYTES,"mailbox_entries":32,"mailbox_bytes":32768,"handles":128},
                "memory_target_bytes":1_000_000,"memory_target_enforced":false}),
            );
        }
        if !self.initialized.get() {
            return Err(RpcError::new(
                -32000,
                "NOT_INITIALIZED",
                "call sf.initialize first",
            ));
        }
        match method {
            "sf.object.load" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Load {
                    elf: String,
                    #[serde(default)]
                    config: Value,
                    #[serde(default)]
                    capabilities: Vec<Capability>,
                }
                let load: Load = decode(params)?;
                let bytes = STANDARD.decode(load.elf).map_err(RpcError::params)?;
                self.runtime
                    .load(bytes, load.config, load.capabilities)
                    .await
            }
            "sf.object.start" => self.runtime.start(&object_id(params)?),
            "sf.object.stop" => self.runtime.stop(&object_id(params)?, false).await,
            "sf.object.unload" => self.runtime.stop(&object_id(params)?, true).await,
            "sf.object.get" => Ok(self.runtime.get(&object_id(params)?)?.status()),
            "sf.object.list" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct List {
                    #[serde(default)]
                    after: Option<String>,
                    #[serde(default = "page_size")]
                    limit: usize,
                }
                let list: List = decode(params)?;
                if list.limit == 0 || list.limit > 256 {
                    return Err(RpcError::params("limit must be 1..256"));
                }
                let objects = self.runtime.objects.borrow();
                let mut items = objects
                    .iter()
                    .filter(|(id, _)| list.after.as_ref().is_none_or(|after| *id > after))
                    .map(|(_, o)| o.status());
                let page: Vec<_> = items.by_ref().take(list.limit).collect();
                let next = if items.next().is_some() {
                    page.last().map(|v| v["object_id"].clone())
                } else {
                    None
                };
                Ok(json!({"objects":page,"next":next}))
            }
            "sf.event.deliver" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Event {
                    object_id: String,
                    event_id: String,
                    topic: String,
                    payload: Value,
                }
                let event: Event = decode(params)?;
                if event.topic.len() > 128 || !crate::helpers::valid_value(&event.payload) {
                    return Err(RpcError::params("event exceeds limits"));
                }
                self.runtime.get(&event.object_id)?.deliver(
                    json!({"event_id":event.event_id,"topic":event.topic,"payload":event.payload}),
                )
            }
            "sf.stats" => {
                empty(params)?;
                Ok(
                    json!({"objects":self.runtime.objects.borrow().len(),"host":self.runtime.host.stats(),"output":self.runtime.out.stats(),"uptime_ms":self.runtime.started.elapsed().as_millis(),"execution_threads":1}),
                )
            }
            "sf.shutdown" => {
                empty(params)?;
                Ok(json!({"shutdown":true}))
            }
            "sf.build.compile" => self.builds.compile(params).await,
            _ => Err(RpcError::new(-32601, "METHOD_NOT_FOUND", "unknown method")),
        }
    }
}
fn page_size() -> usize {
    100
}
fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, RpcError> {
    serde_json::from_value(value).map_err(RpcError::params)
}
fn empty(value: Value) -> Result<(), RpcError> {
    if value.as_object().is_some_and(|o| o.is_empty()) {
        Ok(())
    } else {
        Err(RpcError::params("expected empty parameters"))
    }
}
fn object_id(params: Value) -> Result<String, RpcError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Id {
        object_id: String,
    }
    Ok(decode::<Id>(params)?.object_id)
}
fn response(id: Value, result: RpcResult) -> Value {
    match result {
        Ok(v) => json!({"jsonrpc":"2.0","id":id,"result":v}),
        Err(e) => json!({"jsonrpc":"2.0","id":id,"error":e}),
    }
}

/// Incremental framing never grows the input buffer beyond the configured limit.
async fn frame<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut frame = Vec::new();
    loop {
        let bytes = reader.fill_buf().await?;
        if bytes.is_empty() {
            return if frame.is_empty() {
                Ok(None)
            } else {
                Err(std::io::Error::other("EOF inside JSON frame"))
            };
        }
        let end = bytes.iter().position(|b| *b == b'\n').map(|i| i + 1);
        let count = end.unwrap_or(bytes.len());
        if frame.len() + count > MAX_FRAME {
            return Err(std::io::Error::other("frame exceeds limit"));
        }
        frame.extend_from_slice(&bytes[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(Some(frame));
        }
    }
}
struct ActiveId {
    ids: Rc<RefCell<HashSet<String>>>,
    id: String,
}
impl Drop for ActiveId {
    fn drop(&mut self) {
        self.ids.borrow_mut().remove(&self.id);
    }
}

pub async fn serve<R: AsyncRead + Unpin, W: AsyncWrite + Unpin + 'static>(
    input: R,
    output: W,
) -> anyhow::Result<()> {
    serve_with_config(input, output, None).await
}
pub async fn serve_with_config<R: AsyncRead + Unpin, W: AsyncWrite + Unpin + 'static>(
    input: R,
    mut output: W,
    config: Option<crate::build::Config>,
) -> anyhow::Result<()> {
    let out = Outbox::default();
    let service = Service::new(out.clone(), config).await;
    let writer_out = out.clone();
    let writer = tokio::task::spawn_local(async move {
        while let Some(frame) = writer_out.next().await {
            if let Err(e) = output.write_all(&frame).await {
                writer_out.closed.cancel();
                return Err(e);
            }
        }
        output.flush().await
    });
    let mut reader = BufReader::new(input);
    let slots = Arc::new(Semaphore::new(48));
    let ids = Rc::new(RefCell::new(HashSet::new()));
    let mut tasks = JoinSet::new();
    let mut input_error = None;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    loop {
        tokio::select! {
            biased;
            _=service.shutdown.cancelled()=>break,
            _=terminate.recv()=>break,
            _=interrupt.recv()=>break,
            _=out.closed.cancelled()=>break,
            result=tasks.join_next(), if !tasks.is_empty()=>{
                if let Some(Err(e))=result {input_error=Some(anyhow::anyhow!("request task failed: {e}"));break;}
            },
            incoming=frame(&mut reader)=>{
                let bytes=match incoming{Ok(Some(v))=>v,Ok(None)=>break,Err(e)=>{input_error=Some(e.into());break;}};
                let value:Value=match serde_json::from_slice(&bytes){
                    Ok(v)=>v,
                    Err(e)=>{if !out.try_control(response(Value::Null,Err(RpcError::new(-32700,"PARSE_ERROR",e)))){break;}continue;}
                };
                if !value.is_object() || value.get("jsonrpc").and_then(Value::as_str)!=Some("2.0") {
                    if !out.try_control(response(Value::Null,Err(RpcError::new(-32600,"INVALID_REQUEST","expected JSON-RPC 2.0 object")))){break;}
                    continue;
                }
                if value.get("method").is_none() && value.get("id").is_some() && (value.get("result").is_some() || value.get("error").is_some()) {
                    service.runtime.host.response(&value);continue;
                }
                let Some(method)=value.get("method").and_then(Value::as_str) else {
                    if !out.try_control(response(Value::Null,Err(RpcError::new(-32600,"INVALID_REQUEST","method must be a string")))){break;}
                    continue;
                };
                let id=value.get("id").cloned();
                if id.as_ref().is_some_and(|id|id.as_str().is_none_or(|s|s.len()>128)) {
                    if !out.try_control(response(Value::Null,Err(RpcError::new(-32600,"INVALID_REQUEST","IDs must be strings up to 128 bytes")))){break;}
                    continue;
                }
                let params=value.get("params").cloned().unwrap_or(json!({}));
                // Control methods require a response-bearing request in this protocol profile.
                let Some(id)=id else {continue;};
                let id_string=id.as_str().unwrap().to_owned();
                if ids.borrow().contains(&id_string){
                    if !out.try_control(response(id,Err(RpcError::new(-32600,"DUPLICATE_ID","request ID already outstanding")))){break;}
                    continue;
                }
                // Compile concurrency is owned by the embedder; preserve control capacity.
                let permit=if method == "sf.build.compile" {None} else {
                    match slots.clone().try_acquire_owned(){Ok(p)=>Some(p),Err(_)=>{if !out.try_control(response(id,Err(RpcError::busy("too many requests")))){break;}continue;}}
                };
                ids.borrow_mut().insert(id_string.clone());
                let active=ActiveId{ids:ids.clone(),id:id_string};
                let service=service.clone();let out=out.clone();let method=method.to_owned();
                tasks.spawn_local(async move {
                    let (_permit,_active)=(permit,active);
                    let result=service.handle(&method,params).await;
                    let shutdown = method == "sf.shutdown" && result.is_ok();
                    let _=out.control(response(id,result)).await;
                    if shutdown { service.shutdown.cancel(); }
                });
            }
        }
    }
    // shutdown's own response was queued before its task yielded; other work is cancelled.
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    service.runtime.shutdown().await;
    service.builds.shutdown().await;
    out.closed.cancel();
    let mut writer = writer;
    match tokio::time::timeout(Duration::from_secs(2), &mut writer).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(e))) => {
            if input_error.is_none() {
                input_error = Some(e.into());
            }
        }
        Ok(Err(e)) => {
            if input_error.is_none() {
                input_error = Some(e.into());
            }
        }
        Err(_) => {
            writer.abort();
            let _ = writer.await;
        }
    }
    if let Some(e) = input_error {
        Err(e)
    } else {
        Ok(())
    }
}
