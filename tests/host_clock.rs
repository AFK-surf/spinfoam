mod common;
use common::Client;
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn guest_time_moves_only_with_the_host_clock() {
    let mut c = Client::with_init(
        &[],
        &[],
        json!({"protocol_version":1,"clock":{"monotonic_ms":1_000,"unix_ms":5_000}}),
    )
    .await;
    let id = c
        .load(
            "SF_MAIN int main(void){sf_sleep_ms(60000);\
             return (int)(sf_now_mono_ms() / 1000) * 100 + (int)((sf_now_unix_ms() - 5000) / 1000);}",
            json!({}),
            json!([]),
        )
        .await;
    c.start(&id).await;

    let next = c.method("sf.clock.next").await;
    assert_eq!(next["params"]["due_monotonic_ms"], 61_000);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let status = c.call("sf.object.get", json!({"object_id":id})).await;
    assert_eq!(status["state"], "running");
    assert_eq!(status["wait"], "timer");

    let reply = c
        .call(
            "sf.clock.advance",
            json!({"monotonic_ms":61_000,"unix_ms":65_000}),
        )
        .await;
    assert_eq!(reply["next_due_monotonic_ms"], json!(null));
    // The reply came once the guest had run on: it has exited.
    let status = c.call("sf.object.get", json!({"object_id":id})).await;
    assert_eq!(status["state"], "exited");
    assert_eq!(status["outcome"]["exit_code"], 6_060);

    let back = c
        .request("sf.clock.advance", json!({"monotonic_ms":0,"unix_ms":0}))
        .await;
    assert!(back.get("error").is_some());
    c.shutdown().await;
}

#[tokio::test]
async fn an_event_wait_times_out_on_the_host_clock() {
    let mut c = Client::with_init(
        &[],
        &[],
        json!({"protocol_version":1,"clock":{"monotonic_ms":0,"unix_ms":0}}),
    )
    .await;
    let id = c
        .load(
            "SF_MAIN int main(void){sf_handle e = sf_event_next(30000); return e < 0 ? 7 : 1;}",
            json!({}),
            json!([]),
        )
        .await;
    c.start(&id).await;
    assert_eq!(
        c.method("sf.clock.next").await["params"]["due_monotonic_ms"],
        30_000
    );
    c.call(
        "sf.clock.advance",
        json!({"monotonic_ms":29_999,"unix_ms":29_999}),
    )
    .await;
    let status = c.call("sf.object.get", json!({"object_id":id})).await;
    assert_eq!(status["wait"], "event");
    c.call(
        "sf.clock.advance",
        json!({"monotonic_ms":30_000,"unix_ms":30_000}),
    )
    .await;
    let status = c.call("sf.object.get", json!({"object_id":id})).await;
    assert_eq!(status["outcome"]["exit_code"], 7);
    c.shutdown().await;
}

#[tokio::test]
async fn an_event_is_answered_once_the_guest_has_acted_on_it() {
    let mut c = Client::with_init(
        &[],
        &[],
        json!({"protocol_version":1,"clock":{"monotonic_ms":0,"unix_ms":0}}),
    )
    .await;
    let id = c
        .load(
            "SF_MAIN int main(void){sf_handle e = sf_event_next(86400000); \
             for (int i = 0; i < 2000; i++) sf_yield(); return e > 0 ? 9 : 1;}",
            json!({}),
            json!([]),
        )
        .await;
    c.start(&id).await;
    c.method("sf.clock.next").await;
    c.call(
        "sf.event.deliver",
        json!({"object_id":id,"event_id":"e1","topic":"t","payload":{}}),
    )
    .await;
    let status = c.call("sf.object.get", json!({"object_id":id})).await;
    assert_eq!(status["outcome"]["exit_code"], 9);
    c.shutdown().await;
}
