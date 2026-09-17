#![allow(dead_code)]
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{collections::VecDeque, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};
pub struct Client {
    pub child: Child,
    pub info: Value,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    next: u64,
    saved: VecDeque<Value>,
    read_timeout: Duration,
}
impl Client {
    pub async fn new() -> Self {
        Self::with_args(&[]).await
    }
    pub async fn with_args(args: &[&str]) -> Self {
        Self::with_env(args, &[]).await
    }
    pub async fn with_env(args: &[&str], env: &[(&str, &str)]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_spinfoam"))
            .args(args)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut client = Self {
            child,
            info: Value::Null,
            input: Some(input),
            output,
            next: 0,
            saved: VecDeque::new(),
            // Allow startup under heavily loaded CI runners.
            read_timeout: Duration::from_secs(90),
        };
        let init = client
            .call("sf.initialize", json!({"protocol_version":1}))
            .await;
        assert_eq!(init["protocol_version"], 1);
        client.info = init;
        client.read_timeout = Duration::from_secs(20);
        client
    }
    // Run identical application behavior checks against both compiler outputs in CI.
    pub async fn load_example(
        &mut self,
        source: &str,
        config: Value,
        capabilities: Value,
    ) -> String {
        match std::env::var("SPINFOAM_EXAMPLE_COMPILER")
            .as_deref()
            .unwrap_or("tinycc")
        {
            "tinycc" => self.build_load(source, config, capabilities).await,
            "clang" => self.load(source, config, capabilities).await,
            other => panic!("unknown example compiler: {other}"),
        }
    }
    pub async fn build_load(&mut self, source: &str, config: Value, capabilities: Value) -> String {
        let build = self
            .call(
                "sf.build.submit",
                json!({"sdk_version":1,"entry":"main.c","files":{"main.c":source}}),
            )
            .await;
        let status = loop {
            let status = self
                .call("sf.build.status", json!({"build_id":build["build_id"]}))
                .await;
            if !matches!(status["state"].as_str(), Some("queued" | "running")) {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(status["state"], "succeeded", "{status}");
        self.call("sf.object.load",json!({"artifact_id":status["result"]["artifact_id"],"config":config,"capabilities":capabilities})).await["object_id"].as_str().unwrap().to_owned()
    }
    pub async fn close_input(&mut self) {
        self.input.take();
    }
    pub async fn write(&mut self, value: Value) {
        self.raw(&(serde_json::to_string(&value).unwrap() + "\n"))
            .await;
    }
    pub async fn raw(&mut self, value: &str) {
        self.input
            .as_mut()
            .unwrap()
            .write_all(value.as_bytes())
            .await
            .unwrap();
    }
    pub async fn read(&mut self) -> Value {
        let mut line = String::new();
        let n = tokio::time::timeout(self.read_timeout, self.output.read_line(&mut line))
            .await
            .expect("protocol timeout")
            .unwrap();
        assert!(n > 0, "unexpected EOF: {:?}", self.child.try_wait());
        serde_json::from_str(&line).unwrap()
    }
    pub async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = format!("test:{}", self.next);
        self.write(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await;
        loop {
            let value = self.read().await;
            if value["id"] == id {
                return value;
            }
            self.saved.push_back(value);
        }
    }
    pub async fn call(&mut self, method: &str, params: Value) -> Value {
        let response = self.request(method, params).await;
        assert!(response.get("error").is_none(), "{response}");
        response["result"].clone()
    }
    pub async fn method(&mut self, method: &str) -> Value {
        if let Some(i) = self.saved.iter().position(|v| v["method"] == method) {
            return self.saved.remove(i).unwrap();
        }
        loop {
            let value = self.read().await;
            if value["method"] == method {
                return value;
            }
            self.saved.push_back(value);
        }
    }
    pub async fn load(&mut self, code: &str, config: Value, capabilities: Value) -> String {
        let elf = compile(code).await;
        self.load_bytes(&elf, config, capabilities).await
    }
    pub async fn load_bytes(&mut self, elf: &[u8], config: Value, capabilities: Value) -> String {
        self.call(
            "sf.object.load",
            json!({"elf":STANDARD.encode(elf),"config":config,"capabilities":capabilities}),
        )
        .await["object_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    pub async fn start(&mut self, id: &str) {
        self.call("sf.object.start", json!({"object_id":id})).await;
    }
    pub async fn terminal(&mut self, id: &str) -> Value {
        for _ in 0..200 {
            let status = self.call("sf.object.get", json!({"object_id":id})).await;
            if ["failed", "exited", "stopped"].contains(&status["state"].as_str().unwrap()) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("object never terminated")
    }
    pub async fn shutdown(mut self) {
        self.call("sf.shutdown", json!({})).await;
        assert!(
            tokio::time::timeout(Duration::from_secs(5), self.child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }
}
pub async fn compile(code: &str) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("main.c"),
        format!("#include \"spinfoam.h\"\n{code}"),
    )
    .unwrap();
    std::fs::write(dir.path().join("spinfoam.h"), spinfoam::SDK).unwrap();
    for (cmd, args) in [
        (
            "clang",
            vec![
                "-O2",
                "-target",
                "bpfel",
                "-ffreestanding",
                "-fno-zero-initialized-in-bss",
                "-fno-builtin",
                "-nostdinc",
                "-I.",
                "-emit-llvm",
                "-c",
                "main.c",
                "-o",
                "main.bc",
            ],
        ),
        (
            "llc",
            vec![
                "-march=bpf",
                "--nozero-initialized-in-bss",
                "-mcpu=v3",
                "-bpf-stack-size=4096",
                "-filetype=obj",
                "main.bc",
                "-o",
                "main.o",
            ],
        ),
    ] {
        let result = Command::new(cmd)
            .args(args)
            .current_dir(dir.path())
            .output()
            .await
            .unwrap();
        assert!(
            result.status.success(),
            "{cmd}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    std::fs::read(dir.path().join("main.o")).unwrap()
}
