use std::path::PathBuf;

use bridge_core::HouseContext;
use codex_runtime::{CodexRuntime, CodexRuntimeConfig};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

fn runtime() -> CodexRuntime {
    CodexRuntime::new(CodexRuntimeConfig {
        socket_path: PathBuf::from("/run/alice-codex/app-server.sock"),
        cwd_root: PathBuf::from("/srv/alice/houses"),
        permission_profile_prefix: "alice-house-".to_owned(),
    })
    .unwrap()
}

fn house() -> HouseContext {
    HouseContext {
        id: 1,
        name: "Дом мамы".to_owned(),
        codex_thread_id: None,
        homey_connector_id: "homey-mother".to_owned(),
    }
}

async fn read_message(reader: &mut BufReader<ReadHalf<DuplexStream>>) -> Value {
    let mut line = String::new();
    assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
    serde_json::from_str(&line).unwrap()
}

async fn write_message(writer: &mut WriteHalf<DuplexStream>, value: Value) {
    writer
        .write_all(value.to_string().as_bytes())
        .await
        .unwrap();
    writer.write_all(b"\n").await.unwrap();
    writer.flush().await.unwrap();
}

async fn initialize(
    reader: &mut BufReader<ReadHalf<DuplexStream>>,
    writer: &mut WriteHalf<DuplexStream>,
) {
    let request = read_message(reader).await;
    assert_eq!(request["method"], "initialize");
    assert_eq!(
        request["params"]["clientInfo"]["name"],
        "alice-household-bridge"
    );
    assert_eq!(request["params"]["capabilities"]["experimentalApi"], true);
    write_message(
        writer,
        json!({
            "id": request["id"],
            "result": {
                "userAgent": "codex-test",
                "platformFamily": "unix",
                "platformOs": "linux",
                "codexHome": "/srv/alice/codex-home"
            }
        }),
    )
    .await;
    let notification = read_message(reader).await;
    assert_eq!(notification["method"], "initialized");
}

#[tokio::test]
async fn start_thread_uses_house_cwd_permission_profile_and_instructions() {
    let (client, server) = tokio::io::duplex(32 * 1024);
    let runtime = runtime();
    let task = tokio::spawn(async move {
        runtime
            .start_thread_on(client, &house(), "HOUSE INSTRUCTIONS")
            .await
    });
    let (read, mut write) = tokio::io::split(server);
    let mut read = BufReader::new(read);
    initialize(&mut read, &mut write).await;

    let request = read_message(&mut read).await;
    assert_eq!(request["method"], "thread/start");
    assert_eq!(request["params"]["cwd"], "/srv/alice/houses/1");
    assert_eq!(request["params"]["permissions"], "alice-house-1");
    assert_eq!(
        request["params"]["developerInstructions"],
        "HOUSE INSTRUCTIONS"
    );
    assert_eq!(request["params"]["ephemeral"], false);
    assert_eq!(request["params"]["approvalPolicy"], "never");
    assert!(request["params"].get("sandbox").is_none());
    assert_eq!(request["params"]["config"]["features"]["shell_tool"], false);
    assert_eq!(
        request["params"]["config"]["features"]["unified_exec"],
        false
    );
    assert_eq!(request["params"]["config"]["tools"]["web_search"], false);
    assert_eq!(request["params"]["config"]["tools"]["view_image"], false);
    assert_eq!(
        request["params"]["config"]["apps"]["_default"]["enabled"],
        false
    );
    assert_eq!(
        request["params"]["config"]["mcp_servers"]["homey-mother"]["enabled"],
        true
    );
    write_message(
        &mut write,
        json!({"id": request["id"], "result": {
            "thread": {"id": "thread-1"},
            "activePermissionProfile": {"id": "alice-house-1"},
            "sandbox": {"type": "readOnly", "networkAccess": false}
        }}),
    )
    .await;

    assert_eq!(task.await.unwrap().unwrap(), "thread-1");
}

#[tokio::test]
async fn start_thread_rejects_wrong_or_writable_permission_profile() {
    let (client, server) = tokio::io::duplex(32 * 1024);
    let runtime = runtime();
    let task = tokio::spawn(async move {
        runtime
            .start_thread_on(client, &house(), "HOUSE INSTRUCTIONS")
            .await
    });
    let (read, mut write) = tokio::io::split(server);
    let mut read = BufReader::new(read);
    initialize(&mut read, &mut write).await;
    let request = read_message(&mut read).await;
    write_message(
        &mut write,
        json!({"id": request["id"], "result": {
            "thread": {"id": "thread-1"},
            "activePermissionProfile": {"id": "alice-house-99"},
            "sandbox": {"type": "workspaceWrite", "networkAccess": false}
        }}),
    )
    .await;

    assert!(task.await.unwrap().is_err());
}

#[tokio::test]
async fn turn_resumes_thread_and_concatenates_only_matching_deltas() {
    let (client, server) = tokio::io::duplex(32 * 1024);
    let runtime = runtime();
    let task = tokio::spawn(async move {
        runtime
            .turn_on(client, &house(), "thread-1", "Привет")
            .await
    });
    let (read, mut write) = tokio::io::split(server);
    let mut read = BufReader::new(read);
    initialize(&mut read, &mut write).await;

    let resume = read_message(&mut read).await;
    assert_eq!(resume["method"], "thread/resume");
    assert_eq!(resume["params"]["threadId"], "thread-1");
    assert_eq!(resume["params"]["permissions"], "alice-house-1");
    assert_eq!(
        resume["params"]["config"]["mcp_servers"]["homey-mother"]["enabled"],
        true
    );
    write_message(
        &mut write,
        json!({"id": resume["id"], "result": {
            "thread": {"id": "thread-1"},
            "activePermissionProfile": {"id": "alice-house-1"},
            "sandbox": {"type": "readOnly", "networkAccess": false}
        }}),
    )
    .await;

    let turn = read_message(&mut read).await;
    assert_eq!(turn["method"], "turn/start");
    assert_eq!(turn["params"]["threadId"], "thread-1");
    assert_eq!(
        turn["params"]["input"],
        json!([{"type": "text", "text": "Привет"}])
    );
    write_message(
        &mut write,
        json!({"id": turn["id"], "result": {"turn": {"id": "turn-1"}}}),
    )
    .await;
    write_message(
        &mut write,
        json!({"method": "item/agentMessage/delta", "params": {
            "threadId": "thread-1", "turnId": "turn-1", "itemId": "item-1", "delta": "Кондиционер "
        }}),
    )
    .await;
    write_message(
        &mut write,
        json!({"method": "item/agentMessage/delta", "params": {
            "threadId": "thread-1", "turnId": "turn-1", "itemId": "item-1", "delta": "включён."
        }}),
    )
    .await;
    write_message(
        &mut write,
        json!({"method": "turn/completed", "params": {
            "threadId": "thread-1", "turn": {"id": "turn-1", "status": "completed"}
        }}),
    )
    .await;

    assert_eq!(task.await.unwrap().unwrap(), "Кондиционер включён.");
}

#[tokio::test]
async fn turn_rejects_resume_outside_household_permission_profiles() {
    let (client, server) = tokio::io::duplex(32 * 1024);
    let runtime = runtime();
    let task = tokio::spawn(async move {
        runtime
            .turn_on(client, &house(), "thread-1", "Привет")
            .await
    });
    let (read, mut write) = tokio::io::split(server);
    let mut read = BufReader::new(read);
    initialize(&mut read, &mut write).await;
    let resume = read_message(&mut read).await;
    write_message(
        &mut write,
        json!({"id": resume["id"], "result": {
            "thread": {"id": "thread-1"},
            "activePermissionProfile": {"id": "alice-house-2"},
            "sandbox": {"type": "readOnly", "networkAccess": false}
        }}),
    )
    .await;

    let result = tokio::time::timeout(std::time::Duration::from_millis(100), task).await;
    assert!(matches!(result, Ok(Ok(Err(_)))));
}

#[tokio::test]
async fn command_and_file_approvals_are_declined() {
    let (client, server) = tokio::io::duplex(32 * 1024);
    let runtime = runtime();
    let task =
        tokio::spawn(async move { runtime.turn_on(client, &house(), "thread-1", "Тест").await });
    let (read, mut write) = tokio::io::split(server);
    let mut read = BufReader::new(read);
    initialize(&mut read, &mut write).await;
    let resume = read_message(&mut read).await;
    write_message(
        &mut write,
        json!({"id": resume["id"], "result": {
            "thread": {"id": "thread-1"},
            "activePermissionProfile": {"id": "alice-house-1"},
            "sandbox": {"type": "readOnly", "networkAccess": false}
        }}),
    )
    .await;
    let turn = read_message(&mut read).await;
    write_message(
        &mut write,
        json!({"id": turn["id"], "result": {"turn": {"id": "turn-1"}}}),
    )
    .await;

    for (id, method) in [
        (91, "item/commandExecution/requestApproval"),
        (92, "item/fileChange/requestApproval"),
    ] {
        write_message(
            &mut write,
            json!({"id": id, "method": method, "params": {
                "threadId": "thread-1", "turnId": "turn-1", "itemId": "item-1", "startedAtMs": 1
            }}),
        )
        .await;
        let denial = read_message(&mut read).await;
        assert_eq!(denial, json!({"id": id, "result": {"decision": "decline"}}));
    }
    write_message(
        &mut write,
        json!({"method": "item/agentMessage/delta", "params": {
            "threadId": "thread-1", "turnId": "turn-1", "itemId": "item-1", "delta": "Готово"
        }}),
    )
    .await;
    write_message(
        &mut write,
        json!({"method": "turn/completed", "params": {
            "threadId": "thread-1", "turn": {"id": "turn-1", "status": "completed"}
        }}),
    )
    .await;
    assert_eq!(task.await.unwrap().unwrap(), "Готово");
}

#[tokio::test]
async fn unknown_server_request_is_rejected_and_fails_turn() {
    let (client, server) = tokio::io::duplex(32 * 1024);
    let runtime = runtime();
    let task =
        tokio::spawn(async move { runtime.turn_on(client, &house(), "thread-1", "Тест").await });
    let (read, mut write) = tokio::io::split(server);
    let mut read = BufReader::new(read);
    initialize(&mut read, &mut write).await;
    let resume = read_message(&mut read).await;
    write_message(
        &mut write,
        json!({"id": resume["id"], "result": {
            "thread": {"id": "thread-1"},
            "activePermissionProfile": {"id": "alice-house-1"},
            "sandbox": {"type": "readOnly", "networkAccess": false}
        }}),
    )
    .await;
    let turn = read_message(&mut read).await;
    write_message(
        &mut write,
        json!({"id": turn["id"], "result": {"turn": {"id": "turn-1"}}}),
    )
    .await;
    write_message(
        &mut write,
        json!({"id": 99, "method": "process/spawn", "params": {}}),
    )
    .await;
    let rejection = read_message(&mut read).await;
    assert_eq!(rejection["id"], 99);
    assert_eq!(rejection["error"]["code"], -32601);
    assert!(task.await.unwrap().is_err());
}
