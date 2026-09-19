pub mod compiler;
use crate::error::{RpcError, RpcResult};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Component, Path},
    rc::Rc,
};
use tokio_util::sync::CancellationToken;

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
pub struct Builds {
    runtime: Rc<crate::runtime::Runtime>,
    unavailable: Option<String>,
    fingerprint: String,
    cancel: CancellationToken,
}
impl Builds {
    pub async fn new(config: Option<Config>, runtime: Rc<crate::runtime::Runtime>) -> Rc<Self> {
        Rc::new(Self {
            runtime,
            unavailable: config
                .is_none()
                .then(|| "pass --enable-builds to enable compilation".to_owned()),
            fingerprint: crate::sha256(
                &[
                    compiler::OBJECT,
                    crate::SDK.as_bytes(),
                    crate::ASYNC_EBPF_REVISION.as_bytes(),
                ]
                .concat(),
            ),
            cancel: CancellationToken::new(),
        })
    }
    pub fn info(&self) -> Value {
        json!({"available":self.unavailable.is_none(),"reason":self.unavailable,"fingerprint":self.fingerprint,"name":"tinycc-in-ebpf","revision":compiler::REVISION,"target":"bpfel","embedded":true})
    }
    pub async fn compile(&self, params: Value) -> RpcResult {
        let request: Request = serde_json::from_value(params).map_err(RpcError::params)?;
        request.validate()?;
        if let Some(reason) = &self.unavailable {
            return Err(RpcError::new(-32020, "SANDBOX_UNAVAILABLE", reason));
        }
        let output =
            compiler::run(&self.runtime, &request.files, &request.entry, &self.cancel).await;
        if self.cancel.is_cancelled() {
            return Ok(json!({"state":"cancelled","result":null}));
        }
        match output {
            Ok(output) => Ok(json!({"state":"succeeded","result":{
                "sha256":crate::sha256(&output.elf),
                "elf":STANDARD.encode(output.elf),
                "sdk_version":1,
                "toolchain":self.fingerprint,
                "diagnostics":output.diagnostics,
                "diagnostics_truncated":output.truncated
            }})),
            Err(e) => {
                let mut error = format!("{e:#}");
                truncate(&mut error, 16 * 1024);
                Ok(json!({"state":"failed","result":{"kind":"BUILD_FAILED","error":error}}))
            }
        }
    }
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        // The protocol aborts and joins request tasks before shutdown.
    }
}
fn truncate(text: &mut String, max: usize) {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
}
