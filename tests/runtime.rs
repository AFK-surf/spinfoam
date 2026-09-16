mod common;
use common::Client;
use serde_json::json;

#[tokio::test]
async fn independent_globals_sleep_and_lifecycle() {
    let mut c = Client::new().await;
    let code = "volatile sf_i64 counter=40; SF_MAIN int main(void){counter++;sf_sleep_ms(10);return ++counter;}";
    let elf = common::compile(code).await;
    let a = c.load_bytes(&elf, json!({}), json!([])).await;
    let b = c.load_bytes(&elf, json!({}), json!([])).await;
    assert_ne!(a, b);
    c.start(&a).await;
    c.start(&b).await;
    for id in [&a, &b] {
        let result = c.terminal(id).await;
        assert_eq!(result["outcome"]["exit_code"], 42);
    }
    assert!(
        c.request("sf.object.start", json!({"object_id":a}))
            .await
            .get("error")
            .is_some()
    );
    c.call("sf.object.unload", json!({"object_id":a})).await;
    c.call("sf.object.unload", json!({"object_id":a})).await;
    assert_eq!(c.call("sf.stats", json!({})).await["objects"], 1);
    c.shutdown().await;
}
#[tokio::test]
async fn events_host_rpc_and_capability_scope() {
    let mut c = Client::new().await;
    let id = c
        .load(
            r#"SF_MAIN int main(void){
        sf_handle event=sf_event_next(5000); if(event<0)return 1;
        sf_handle payload=sf_json_get(event,"payload");
        sf_handle reply=sf_host_call("notify",payload,5000);
        sf_drop(payload);sf_drop(event);
        if(reply<0)return 2;
        int ok=sf_json_string_equals(reply,"answer","yes");sf_drop(reply);return ok==1?42:3;
    }"#,
            json!({}),
            json!([{"name":"notify","arguments":{"run_id":123}}]),
        )
        .await;
    c.start(&id).await;
    let event = json!({"object_id":id,"event_id":"e1","topic":"webhook","payload":{"run_id":123}});
    assert_eq!(
        c.call("sf.event.deliver", event.clone()).await["duplicate"],
        false
    );
    assert_eq!(c.call("sf.event.deliver", event).await["duplicate"], true);
    let call = c.method("host.call").await;
    assert_eq!(call["params"]["object_id"], id);
    assert_eq!(call["params"]["arguments"], json!({"run_id":123}));
    c.write(json!({"jsonrpc":"2.0","id":call["id"],"result":{"answer":"yes"}}))
        .await;
    assert_eq!(c.terminal(&id).await["outcome"]["exit_code"], 42);
    let denied=c.load("SF_MAIN int main(void){sf_handle h=sf_config();return sf_host_call(\"notify\",h,1000);}",json!({"run_id":456}),json!([{"name":"notify","arguments":{"run_id":123}}])).await;
    c.start(&denied).await;
    assert_eq!(c.terminal(&denied).await["outcome"]["exit_code"], -4);
    c.shutdown().await;
}
#[tokio::test]
async fn spin_is_preemptible_and_fault_is_contained() {
    let mut c = Client::new().await;
    let spin = c
        .load(
            "volatile sf_u64 n; SF_MAIN int main(void){for(;;)n++;}",
            json!({}),
            json!([]),
        )
        .await;
    c.start(&spin).await;
    let fault = c
        .load(
            "SF_MAIN int main(void){volatile sf_u64 *p=(void*)1;return *p;}",
            json!({}),
            json!([]),
        )
        .await;
    c.start(&fault).await;
    assert_eq!(c.terminal(&fault).await["state"], "failed");
    let before = std::time::Instant::now();
    assert_eq!(
        c.call("sf.object.stop", json!({"object_id":spin})).await["state"],
        "stopped"
    );
    assert!(before.elapsed() < std::time::Duration::from_secs(2));
    c.shutdown().await;
}
#[tokio::test]
async fn pending_rpc_cancel_timeout_and_late_response() {
    let mut c = Client::new().await;
    let code =
        "SF_MAIN int main(void){sf_handle h=sf_config();return sf_host_call(\"wait\",h,30000);}";
    let id = c.load(code, json!({}), json!([{"name":"wait"}])).await;
    c.start(&id).await;
    let call = c.method("host.call").await;
    c.call("sf.object.unload", json!({"object_id":id})).await;
    assert_eq!(c.method("host.cancel").await["params"]["id"], call["id"]);
    c.write(json!({"jsonrpc":"2.0","id":call["id"],"result":{}}))
        .await;
    let stats = c.call("sf.stats", json!({})).await;
    assert_eq!(stats["host"]["pending_host_calls"], 0);
    assert_eq!(stats["host"]["ignored_host_responses"], 1);
    let id = c
        .load(
            "SF_MAIN int main(void){sf_handle h=sf_config();return sf_host_call(\"wait\",h,20);}",
            json!({}),
            json!([{"name":"wait"}]),
        )
        .await;
    c.start(&id).await;
    assert_eq!(c.terminal(&id).await["outcome"]["exit_code"], -3);
    c.shutdown().await;
}
#[tokio::test]
async fn protocol_errors_and_mailbox_backpressure() {
    let mut c = Client::new().await;
    c.raw("not json\n").await;
    assert_eq!(c.read().await["error"]["code"], -32700);
    assert_eq!(
        c.request("missing", json!({})).await["error"]["code"],
        -32601
    );
    assert!(
        c.request("sf.object.load", json!({"elf":"not-base64"}))
            .await
            .get("error")
            .is_some()
    );
    let id = c
        .load("SF_MAIN int main(void){return 0;}", json!({}), json!([]))
        .await;
    for n in 0..32 {
        c.call(
            "sf.event.deliver",
            json!({"object_id":id,"event_id":format!("e{n}"),"topic":"x","payload":null}),
        )
        .await;
    }
    assert_eq!(
        c.request(
            "sf.event.deliver",
            json!({"object_id":id,"event_id":"overflow","topic":"x","payload":null})
        )
        .await["error"]["data"]["kind"],
        "MAILBOX_FULL"
    );
    c.shutdown().await;
}
