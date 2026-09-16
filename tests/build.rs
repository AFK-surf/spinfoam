mod common;
use common::Client;
use serde_json::{Value, json};
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
#[ignore = "requires SPINFOAM_TEST_CGROUP pointing to a delegated cpu/memory/pids subtree"]
async fn sandboxed_compiler_end_to_end() {
    let root = std::env::var("SPINFOAM_TEST_CGROUP").expect("set SPINFOAM_TEST_CGROUP");
    let dir = tempfile::tempdir().unwrap();
    let manifest = dir.path().join("toolchain.json");
    assert!(
        tokio::process::Command::new(env!("CARGO_BIN_EXE_spinfoam"))
            .arg("--write-toolchain-manifest")
            .arg(&manifest)
            .status()
            .await
            .unwrap()
            .success()
    );
    let mut client = Client::with_args(&[
        "--toolchain-manifest",
        manifest.to_str().unwrap(),
        "--compiler-cgroup",
        &root,
    ])
    .await;
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
            .contains("file not found")
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
    // Cancellation is acknowledged only after the launcher and cgroup have been reaped.
    let source = format!(
        "#include \"spinfoam.h\"\nSF_MAIN int main(void){{return {};}}",
        (0..10000)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("+")
    );
    let build = client
        .call(
            "sf.build.submit",
            json!({"sdk_version":1,"entry":"main.c","files":{"main.c":source}}),
        )
        .await;
    let status = client
        .call("sf.build.cancel", json!({"build_id":build["build_id"]}))
        .await;
    assert_eq!(status["state"], "cancelled");
    client.shutdown().await;
    assert_eq!(
        std::fs::read_dir(&root)
            .unwrap()
            .filter(|e| e
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("spinfoam-"))
            .count(),
        0,
        "build cgroups leaked"
    );
    // A changed pin disables builds, rather than silently running a different compiler.
    let mut manifest_value: Value =
        serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    manifest_value["clang"]["sha256"] = json!("bad");
    std::fs::write(&manifest, serde_json::to_vec(&manifest_value).unwrap()).unwrap();
    let client = Client::with_args(&[
        "--toolchain-manifest",
        manifest.to_str().unwrap(),
        "--compiler-cgroup",
        &root,
    ])
    .await;
    assert_eq!(client.info["compiler"]["available"], false);
    assert!(
        client.info["compiler"]["reason"]
            .as_str()
            .unwrap()
            .contains("digest changed")
    );
    client.shutdown().await;
}
