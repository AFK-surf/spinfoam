mod common;
use common::Client;
use serde_json::{Value, json};
async fn reply(c: &mut Client, cap: &str, result: Value) -> Value {
    let call = c.method("host.call").await;
    assert_eq!(call["params"]["capability"], cap);
    c.write(json!({"jsonrpc":"2.0","id":call["id"],"result":result}))
        .await;
    call
}
#[tokio::test]
async fn github_example_notifies_on_completion() {
    let mut c = Client::new().await;
    let id = c
        .load(
            include_str!("../examples/github_actions.c"),
            json!({"run_id":123,"deduplication_key":"run:123"}),
            json!([{"name":"github.run.read","arguments":{"run_id":123}},{"name":"agent.notify"}]),
        )
        .await;
    c.start(&id).await;
    reply(
        &mut c,
        "github.run.read",
        json!({"status":"completed","conclusion":"success"}),
    )
    .await;
    let call = reply(&mut c, "agent.notify", json!({"accepted":true})).await;
    assert_eq!(call["params"]["arguments"]["deduplication_key"], "run:123");
    assert_eq!(c.terminal(&id).await["outcome"]["exit_code"], 0);
    c.shutdown().await;
}
#[tokio::test]
async fn device_example_notifies_only_on_transition() {
    let mut c = Client::new().await;
    let id = c
        .load(
            include_str!("../examples/homeassistant.c"),
            json!({"desired_state":"on"}),
            json!([{"name":"agent.notify"}]),
        )
        .await;
    c.start(&id).await;
    c.call(
        "sf.event.deliver",
        json!({"object_id":id,"event_id":"1","topic":"device","payload":{"state":"on"}}),
    )
    .await;
    reply(&mut c, "agent.notify", json!({})).await;
    c.call(
        "sf.event.deliver",
        json!({"object_id":id,"event_id":"2","topic":"device","payload":{"state":"on"}}),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let status = c.call("sf.object.get", json!({"object_id":id})).await;
    assert_eq!(status["wait"], "event");
    assert_eq!(
        c.call("sf.stats", json!({})).await["host"]["pending_host_calls"],
        0
    );
    c.shutdown().await;
}
#[tokio::test]
async fn keyword_example_matches_across_chunks() {
    let mut c = Client::new().await;
    let id=c.load(include_str!("../examples/web_keyword.c"),json!({"url":"https://example.invalid","keyword":"needle","deduplication_key":"page:1"}),json!([{"name":"web.read_chunk"},{"name":"agent.notify"}])).await;
    c.start(&id).await;
    reply(
        &mut c,
        "web.read_chunk",
        json!({"data":"haystack nee","next_offset":12,"done":false,"snapshot":"v1"}),
    )
    .await;
    let call = reply(
        &mut c,
        "web.read_chunk",
        json!({"data":"dle more","next_offset":20,"done":true,"snapshot":"v1"}),
    )
    .await;
    assert_eq!(call["params"]["arguments"]["offset"], 12);
    assert_eq!(call["params"]["arguments"]["snapshot"], "v1");
    reply(&mut c, "agent.notify", json!({})).await;
    assert_eq!(c.terminal(&id).await["outcome"]["exit_code"], 0);
    c.shutdown().await;
}
#[tokio::test]
async fn webhook_example_processes_and_acknowledges() {
    let mut c = Client::new().await;
    let id = c
        .load(
            include_str!("../examples/webhook.c"),
            json!({}),
            json!([{"name":"agent.notify"},{"name":"webhook.ack"}]),
        )
        .await;
    c.start(&id).await;
    c.call("sf.event.deliver",json!({"object_id":id,"event_id":"delivery1","topic":"webhook","payload":{"kind":"deploy","arbitrary":{"nested":[1,true,null]}}})).await;
    reply(&mut c, "agent.notify", json!({})).await;
    let call = reply(&mut c, "webhook.ack", json!({})).await;
    assert_eq!(call["params"]["arguments"]["event_id"], "delivery1");
    c.shutdown().await;
}
