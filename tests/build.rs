mod common;
use common::Client;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;

async fn finish(client: &mut Client, id: &str) -> Value {
    for _ in 0..1000 {
        let status = client.call("sf.build.status", json!({"build_id":id})).await;
        if ["succeeded", "failed", "cancelled"].contains(&status["state"].as_str().unwrap()) {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("build never completed")
}
#[tokio::test]
async fn disabled_builds_fail_closed() {
    let mut client = Client::new().await;
    assert_eq!(client.info["compiler"]["available"], false);
    let response=client.request("sf.build.submit",json!({"sdk_version":1,"entry":"main.c","files":{"main.c":"int main(void){return 0;}"}})).await;
    assert_eq!(response["error"]["data"]["kind"], "SANDBOX_UNAVAILABLE");
    client.shutdown().await;
}
#[tokio::test]
#[ignore = "requires LLVM and a working platform sandbox"]
async fn sandboxed_compiler_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::with_args(&["--enable-builds"]).await;
    assert_eq!(
        client.info["compiler"]["available"], true,
        "{}",
        client.info
    );
    let request = json!({"sdk_version":1,"entry":"main.c","files":{"main.c":"#include \"spinfoam.h\"\nvolatile sf_i64 n; SF_MAIN int main(void){sf_sleep_ms(1);return ++n+41;}"}});
    let build = client.call("sf.build.submit", request.clone()).await;
    let status = finish(&mut client, build["build_id"].as_str().unwrap()).await;
    assert_eq!(status["state"], "succeeded", "{status}");
    let artifact = client
        .call(
            "sf.artifact.get",
            json!({"artifact_id":status["result"]["artifact_id"]}),
        )
        .await;
    assert!(artifact["elf"].as_str().unwrap().len() > 100);
    let object = client
        .call(
            "sf.object.load",
            json!({"artifact_id":status["result"]["artifact_id"]}),
        )
        .await;
    let id = object["object_id"].as_str().unwrap();
    client.start(id).await;
    assert_eq!(client.terminal(id).await["outcome"]["exit_code"], 42);
    let cached = client.call("sf.build.submit", request).await;
    assert_eq!(cached["result"]["cached"], true);
    for entry in [
        "../main.c",
        "/main.c",
        "spinfoam.h",
        "a/../main.c",
        "a//main.c",
    ] {
        let response = client
            .request(
                "sf.build.submit",
                json!({"sdk_version":1,"entry":entry,"files":{entry:"hello"}}),
            )
            .await;
        assert_eq!(response["error"]["code"], -32602);
    }
    // A host file containing valid C is unavailable even through an absolute include.
    let secret = dir.path().join("host-only.h");
    std::fs::write(&secret, "#define ANSWER 42\n").unwrap();
    let source = format!(
        "#include \"{}\"\n#include \"spinfoam.h\"\nSF_MAIN int main(void){{return ANSWER;}}",
        secret.display()
    );
    let build = client
        .call(
            "sf.build.submit",
            json!({"sdk_version":1,"entry":"main.c","files":{"main.c":source}}),
        )
        .await;
    let status = finish(&mut client, build["build_id"].as_str().unwrap()).await;
    assert_eq!(status["state"], "failed", "{status}");
    assert!(
        status["result"]["error"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("file not found")
            || status["result"]["error"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains("operation not permitted")
    );
    // Invalid C reports diagnostics and leaves compilation usable.
    let build = client
        .call(
            "sf.build.submit",
            json!({"sdk_version":1,"entry":"main.c","files":{"main.c":"this is not C"}}),
        )
        .await;
    assert_eq!(
        finish(&mut client, build["build_id"].as_str().unwrap()).await["state"],
        "failed"
    );
    // Exponential preprocessing exceeds the sandbox's resource budget without affecting the host.
    let mut bomb = String::from("#include \"spinfoam.h\"\n#define A0 1\n");
    for n in 1..28 {
        bomb.push_str(&format!("#define A{n} A{}+A{}\n", n - 1, n - 1));
    }
    bomb.push_str("SF_MAIN int main(void){return A27;}\n");
    let build = client
        .call(
            "sf.build.submit",
            json!({"sdk_version":1,"entry":"main.c","files":{"main.c":bomb}}),
        )
        .await;
    assert_eq!(
        finish(&mut client, build["build_id"].as_str().unwrap()).await["state"],
        "failed"
    );
    // All distributed examples also pass through the actual sandboxed compiler profile.
    for source in [
        include_str!("../examples/github_actions.c"),
        include_str!("../examples/homeassistant.c"),
        include_str!("../examples/web_keyword.c"),
        include_str!("../examples/webhook.c"),
    ] {
        let build = client
            .call(
                "sf.build.submit",
                json!({"sdk_version":1,"entry":"main.c","files":{"main.c":source}}),
            )
            .await;
        let result = finish(&mut client, build["build_id"].as_str().unwrap()).await;
        assert_eq!(result["state"], "succeeded", "{result}");
    }
    // Cancel while a compiler is running and check its processes disappear.
    let minimum_children = if cfg!(target_os = "linux") { 3 } else { 1 };
    let build = client
        .call(
            "sf.build.submit",
            json!({"sdk_version":1,"entry":"cancel.c","files":{"cancel.c":bomb}}),
        )
        .await;
    let mut children = Vec::new();
    for _ in 0..200 {
        children = descendants(client.child.id().unwrap());
        if children.len() >= minimum_children {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(children.len() >= minimum_children, "compiler never started");
    let status = client
        .call("sf.build.cancel", json!({"build_id":build["build_id"]}))
        .await;
    assert_eq!(status["state"], "cancelled");
    for _ in 0..200 {
        if children.iter().all(|pid| !is_live(*pid)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        children.iter().all(|pid| !is_live(*pid)),
        "sandbox descendants survived cancellation: {children:?}"
    );
    client.shutdown().await;
}

#[tokio::test]
async fn missing_compiler_tools_fail_closed() {
    let client = Client::with_env(&["--enable-builds"], &[("PATH", "")]).await;
    assert_eq!(client.info["compiler"]["available"], false);
    assert!(
        client.info["compiler"]["reason"]
            .as_str()
            .unwrap()
            .contains("clang is not in PATH")
    );
    client.shutdown().await;
}

// /proc checks are confined to the sandbox integration test, never the runtime.
#[cfg(target_os = "linux")]
fn descendants(pid: u32) -> Vec<u32> {
    let children =
        std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap_or_default();
    let mut result = Vec::new();
    for child in children.split_whitespace().filter_map(|s| s.parse().ok()) {
        result.push(child);
        result.extend(descendants(child));
    }
    result
}
#[cfg(target_os = "linux")]
fn is_live(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        !stat
            .rsplit_once(") ")
            .is_some_and(|(_, tail)| tail.starts_with("Z ") || tail.starts_with("X "))
    })
}

#[cfg(target_os = "macos")]
fn descendants(pid: u32) -> Vec<u32> {
    let mut pids = [0i32; 128];
    let count = unsafe {
        libc::proc_listchildpids(
            pid as i32,
            pids.as_mut_ptr().cast(),
            std::mem::size_of_val(&pids) as i32,
        )
    };
    let mut result = Vec::new();
    for child in pids
        .iter()
        .take(count.max(0) as usize)
        .filter(|p| **p > 0)
    {
        result.push(*child as u32);
        result.extend(descendants(*child as u32));
    }
    result
}
#[cfg(target_os = "macos")]
fn is_live(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}
