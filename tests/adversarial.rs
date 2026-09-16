mod common;
use common::Client;
use serde_json::json;
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn invalid_guest_pointers_and_handles_are_contained() {
    let mut c = Client::new().await;
    let pointer = c
        .load(
            "SF_MAIN sf_i64 main(void){return sf_log_raw((const char*)1,8);}",
            json!({}),
            json!([]),
        )
        .await;
    c.start(&pointer).await;
    assert_eq!(c.terminal(&pointer).await["state"], "failed");
    let stale = c
        .load(
            "SF_MAIN sf_i64 main(void){sf_handle h=sf_config();sf_drop(h);return sf_json_kind(h);}",
            json!({}),
            json!([]),
        )
        .await;
    c.start(&stale).await;
    assert_eq!(c.terminal(&stale).await["outcome"]["exit_code"], -1);
    let exhausted=c.load("SF_MAIN sf_i64 main(void){for(int i=0;i<128;i++){if(sf_json_object()<0)return 1;}return sf_json_object();}",json!({}),json!([])).await;
    c.start(&exhausted).await;
    assert_eq!(c.terminal(&exhausted).await["outcome"]["exit_code"], -2);
    let recycling=c.load("SF_MAIN sf_i64 main(void){for(int i=0;i<1000;i++){sf_handle h=sf_json_object();if(h<0)return 1;sf_drop(h);}return 42;}",json!({}),json!([])).await;
    c.start(&recycling).await;
    assert_eq!(c.terminal(&recycling).await["outcome"]["exit_code"], 42);
    c.shutdown().await;
}
#[tokio::test]
async fn oversized_host_result_returns_limit_and_duplicate_is_ignored() {
    let mut c = Client::new().await;
    let id=c.load("SF_MAIN sf_i64 main(void){sf_handle h=sf_config();return sf_host_call(\"read\",h,10000);}",json!({}),json!([{"name":"read"}])).await;
    c.start(&id).await;
    let request = c.method("host.call").await;
    let response = json!({"jsonrpc":"2.0","id":request["id"],"result":"x".repeat(17000)});
    c.write(response.clone()).await;
    c.write(response).await;
    assert_eq!(c.terminal(&id).await["outcome"]["exit_code"], -2);
    assert_eq!(
        c.call("sf.stats", json!({})).await["host"]["ignored_host_responses"],
        1
    );
    c.shutdown().await;
}
#[tokio::test]
async fn oversized_unterminated_frame_closes_session() {
    use std::process::Stdio;
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_spinfoam"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let _ = input
        .write_all(&vec![b' '; spinfoam::outbox::MAX_FRAME + 1])
        .await;
    assert!(
        !tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}
#[tokio::test]
async fn eof_cancels_a_live_spin() {
    let mut c = Client::new().await;
    let id = c
        .load(
            "volatile sf_u64 n=1;SF_MAIN sf_i64 main(void){for(;;)++n;}",
            json!({}),
            json!([]),
        )
        .await;
    c.start(&id).await;
    c.close_input().await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(3), c.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}
#[tokio::test]
async fn simultaneous_stop_requests_complete() {
    let mut c = Client::new().await;
    let id = c
        .load(
            "SF_MAIN sf_i64 main(void){sf_sleep_ms(60000);return 0;}",
            json!({}),
            json!([]),
        )
        .await;
    c.start(&id).await;
    for n in 0..8 {
        c.write(json!({"jsonrpc":"2.0","id":format!("stop{n}"),"method":"sf.object.stop","params":{"object_id":id}})).await;
    }
    let mut replies = 0;
    while replies < 8 {
        let value = c.read().await;
        if value.get("id").is_some() {
            assert_eq!(value["result"]["state"], "stopped");
            replies += 1;
        }
    }
    c.shutdown().await;
}

#[tokio::test]
async fn blocked_stdout_has_bounded_shutdown_and_does_not_deadlock_reader() {
    use std::process::Stdio;
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_spinfoam"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut frames = String::from(
        "{\"jsonrpc\":\"2.0\",\"id\":\"init\",\"method\":\"sf.initialize\",\"params\":{\"protocol_version\":1}}\n",
    );
    for n in 0..2000 {
        frames.push_str(&format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":\"{n}\",\"method\":\"sf.stats\"}}\n"
        ));
    }
    let write = async {
        let _ = input.write_all(frames.as_bytes()).await;
    };
    let (_, status) = tokio::time::timeout(std::time::Duration::from_secs(8), async {
        tokio::join!(write, child.wait())
    })
    .await
    .expect("blocked stdout prevented shutdown");
    assert!(status.unwrap().success());
}
#[tokio::test]
async fn recursive_local_calls_stop_at_guest_stack_boundary() {
    let mut c = Client::new().await;
    let id=c.load("__attribute__((noinline)) sf_i64 recurse(sf_i64 n){volatile sf_i64 keep=n;if(n)return recurse(n-1)+keep;return keep;} SF_MAIN sf_i64 main(void){return recurse(100);}",json!({}),json!([])).await;
    c.start(&id).await;
    let status = c.terminal(&id).await;
    assert_eq!(status["state"], "failed", "{status}");
    assert!(
        status["outcome"]["error"]
            .as_str()
            .unwrap()
            .contains("stack")
    );
    c.shutdown().await;
}
#[tokio::test]
async fn sigterm_cleans_up_live_objects() {
    let mut c = Client::new().await;
    let id = c
        .load(
            "SF_MAIN sf_i64 main(void){sf_sleep_ms(60000);return 0;}",
            json!({}),
            json!([]),
        )
        .await;
    c.start(&id).await;
    assert_eq!(
        unsafe { libc::kill(c.child.id().unwrap() as i32, libc::SIGTERM) },
        0
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(3), c.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}
