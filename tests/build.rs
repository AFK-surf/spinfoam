mod common;
use common::Client;
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn disabled_builds_fail_closed() {
    let mut client = Client::new().await;
    assert_eq!(client.info["compiler"]["available"], false);
    let response=client.request("sf.build.compile",json!({"sdk_version":1,"entry":"main.c","files":{"main.c":"int main(void){return 0;}"}})).await;
    assert_eq!(response["error"]["data"]["kind"], "SANDBOX_UNAVAILABLE");
    client.shutdown().await;
}
#[tokio::test]
async fn embedded_compiler_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let mut client = Client::with_args(&["--enable-builds"]).await;
    assert_eq!(
        client.info["compiler"]["available"], true,
        "{}",
        client.info
    );
    let request = json!({"sdk_version":1,"entry":"main.c","files":{"main.c":"#include \"spinfoam.h\"\nvolatile sf_i64 n; SF_MAIN int main(void){sf_sleep_ms(1);return ++n+41;}"}});
    let build = client.call("sf.build.compile", request.clone()).await;
    let status = build;
    assert_eq!(status["state"], "succeeded", "{status}");
    assert!(status["result"]["elf"].as_str().unwrap().len() > 100);
    assert!(status.get("build_id").is_none());
    assert!(status["result"].get("artifact_id").is_none());
    assert!(status["result"].get("cached").is_none());
    let object = client
        .call("sf.object.load", json!({"elf":status["result"]["elf"]}))
        .await;
    let id = object["object_id"].as_str().unwrap();
    client.start(id).await;
    assert_eq!(client.terminal(id).await["outcome"]["exit_code"], 42);
    let rebuilt = client.call("sf.build.compile", request).await;
    assert_eq!(rebuilt["state"], "succeeded");
    assert_eq!(rebuilt["result"]["sha256"], status["result"]["sha256"]);
    assert!(rebuilt["result"].get("cached").is_none());
    for method in [
        "sf.artifact.get",
        "sf.build.submit",
        "sf.build.status",
        "sf.build.cancel",
    ] {
        let response = client.request(method, json!({})).await;
        assert_eq!(response["error"]["code"], -32601);
    }
    let response = client
        .request("sf.object.load", json!({"artifact_id":"ab1"}))
        .await;
    assert_eq!(response["error"]["code"], -32602);
    for entry in [
        "../main.c",
        "/main.c",
        "spinfoam.h",
        "a/../main.c",
        "a//main.c",
    ] {
        let response = client
            .request(
                "sf.build.compile",
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
            "sf.build.compile",
            json!({"sdk_version":1,"entry":"main.c","files":{"main.c":source}}),
        )
        .await;
    let status = build;
    assert_eq!(status["state"], "failed", "{status}");
    assert!(
        status["result"]["error"]
            .as_str()
            .unwrap()
            .contains("not found"),
        "{status}"
    );
    // Invalid C reports diagnostics and leaves compilation usable.
    let build = client
        .call(
            "sf.build.compile",
            json!({"sdk_version":1,"entry":"main.c","files":{"main.c":"this is not C"}}),
        )
        .await;
    assert_eq!(build["state"], "failed");
    // Exponential preprocessing exceeds the sandbox's resource budget without affecting the host.
    let mut bomb = String::from("#include \"spinfoam.h\"\n#define A0 1\n");
    for n in 1..28 {
        bomb.push_str(&format!("#define A{n} A{}+A{}\n", n - 1, n - 1));
    }
    bomb.push_str("SF_MAIN int main(void){return A27;}\n");
    let build = client
        .call(
            "sf.build.compile",
            json!({"sdk_version":1,"entry":"main.c","files":{"main.c":bomb}}),
        )
        .await;
    assert_eq!(build["state"], "failed");
    // All distributed examples also pass through the actual sandboxed compiler profile.
    for source in [
        include_str!("../examples/github_actions.c"),
        include_str!("../examples/homeassistant.c"),
        include_str!("../examples/web_keyword.c"),
        include_str!("../examples/webhook.c"),
    ] {
        let build = client
            .call(
                "sf.build.compile",
                json!({"sdk_version":1,"entry":"main.c","files":{"main.c":source}}),
            )
            .await;
        let result = build;
        assert_eq!(result["state"], "succeeded", "{result}");
    }
    client.shutdown().await;
    let mut client = Client::with_args(&["--enable-builds"]).await;
    // Other RPCs and shutdown overtake an outstanding compilation.
    client
        .write(
            json!({"jsonrpc":"2.0","id":"pending-build","method":"sf.build.compile",
        "params":{"sdk_version":1,"entry":"cancel.c","files":{"cancel.c":bomb}}}),
        )
        .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    client
        .write(json!({"jsonrpc":"2.0","id":"stats-during-build",
        "method":"sf.stats","params":{}}))
        .await;
    let reply = tokio::time::timeout(Duration::from_secs(2), client.read())
        .await
        .expect("compilation blocked other requests");
    // No build response has arrived: stats must overtake the active compilation.
    assert_eq!(reply["id"], "stats-during-build", "{reply}");
    assert!(reply.get("result").is_some(), "{reply}");
    // Independent compiler instances can finish while the first is still running.
    // Identical virtual paths and conflicting macros must remain request-local.
    for answer in [41, 42] {
        client.write(json!({"jsonrpc":"2.0","id":format!("compile-{answer}"),
            "method":"sf.build.compile","params":{"sdk_version":1,"entry":"main.c",
            "files":{"main.c":"#include <spinfoam.h>\n#include \"value.h\"\nSF_MAIN int main(void){return ANSWER;}",
                     "value.h":format!("#define ANSWER {answer}\n")}}})).await;
    }
    let mut outputs = Vec::new();
    for _ in 0..2 {
        let reply = tokio::time::timeout(Duration::from_secs(5), client.read())
            .await
            .expect("one compilation serialized another");
        let expected = match reply["id"].as_str() {
            Some("compile-41") => 41,
            Some("compile-42") => 42,
            _ => panic!("expected a short compilation to overtake the slow one: {reply}"),
        };
        assert_eq!(reply["result"]["state"], "succeeded", "{reply}");
        outputs.push((reply["result"]["result"]["elf"].clone(), expected));
    }
    for (elf, expected) in outputs {
        let object = client.call("sf.object.load", json!({"elf":elf})).await;
        let id = object["object_id"].as_str().unwrap();
        client.start(id).await;
        assert_eq!(client.terminal(id).await["outcome"]["exit_code"], expected);
    }
    client.shutdown().await;
}

#[tokio::test]
async fn builds_need_no_host_tools() {
    let mut client = Client::with_env(&["--enable-builds"], &[("PATH", "")]).await;
    assert_eq!(client.info["compiler"]["available"], true);
    let build = client.call("sf.build.compile", json!({"sdk_version":1,"entry":"main.c","files":{"main.c":"#include <spinfoam.h>\nSF_MAIN int main(void){return 7;}"}})).await;
    let status = build;
    assert_eq!(status["state"], "succeeded", "{status}");
    client.shutdown().await;
}

#[tokio::test]
async fn virtual_headers_diagnostics_and_output_bounds() {
    let mut client = Client::with_args(&["--enable-builds"]).await;
    let request = json!({"sdk_version":1,"entry":"nested/main.c","files":{
        "nested/main.c":"#include <spinfoam.h>\n#include \"../include/value.h\"\n#warning sample diagnostic\nSF_MAIN sf_i64 main(void){return compute(19);}",
        "include/value.h":"static unsigned long compute(unsigned long x){ unsigned long y=x/4; unsigned long z=x%4; return (y==4 && z==3) ? 42 : 99;}\n"
    }});
    let build = client.call("sf.build.compile", request).await;
    let status = build;
    assert_eq!(status["state"], "succeeded", "{status}");
    assert!(
        status["result"]["diagnostics"]
            .as_str()
            .unwrap()
            .contains("sample diagnostic"),
        "{status}"
    );
    let loaded = client
        .call("sf.object.load", json!({"elf":status["result"]["elf"]}))
        .await;
    let id = loaded["object_id"].as_str().unwrap();
    client.start(id).await;
    assert_eq!(client.terminal(id).await["outcome"]["exit_code"], 42);
    // Initialized output too large to retain, including materialized zero globals.
    let source = "#include <spinfoam.h>\nvolatile char storage[70000]; SF_MAIN int main(void){return storage[0];}";
    let build = client
        .call(
            "sf.build.compile",
            json!({"sdk_version":1,"entry":"big.c","files":{"big.c":source}}),
        )
        .await;
    let status = build;
    assert_eq!(status["state"], "failed", "{status}");
    assert!(
        status["result"]["error"]
            .as_str()
            .unwrap()
            .contains("64 KiB"),
        "{status}"
    );
    // Unsupported backend operations fail explicitly rather than emitting native code.
    let source =
        "#include <spinfoam.h>\nvolatile long n = -19; SF_MAIN long main(void){return n/4;}";
    let build = client
        .call(
            "sf.build.compile",
            json!({"sdk_version":1,"entry":"signed.c","files":{"signed.c":source}}),
        )
        .await;
    let status = build;
    assert_eq!(status["state"], "failed", "{status}");
    assert!(
        status["result"]["error"]
            .as_str()
            .unwrap()
            .contains("signed division"),
        "{status}"
    );
    // Neither a prior build's files nor preprocessor definitions survive.
    let build = client.call("sf.build.compile",json!({"sdk_version":1,"entry":"next.c","files":{"next.c":"#include \"include/value.h\""}})).await;
    assert_eq!(build["state"], "failed");
    let id = client
        .build_load(
            "#include <spinfoam.h>\nSF_MAIN int main(void){return 9;}",
            json!({}),
            json!([]),
        )
        .await;
    client.start(&id).await;
    assert_eq!(client.terminal(&id).await["outcome"]["exit_code"], 9);
    client.shutdown().await;
}
