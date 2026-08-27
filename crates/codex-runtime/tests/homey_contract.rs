use std::path::PathBuf;

use codex_runtime::{CodexRuntime, CodexRuntimeConfig};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

fn runtime() -> CodexRuntime {
    CodexRuntime::new(CodexRuntimeConfig {
        socket_path: PathBuf::from("/run/alice-codex/app-server.sock"),
        cwd_root: PathBuf::from("/srv/alice/houses"),
        permission_profile_prefix: "alice-house-".to_owned(),
        model: None,
        effort: None,
        homey_enabled: true,
    })
    .unwrap()
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

async fn begin_turn(
    reader: &mut BufReader<ReadHalf<DuplexStream>>,
    writer: &mut WriteHalf<DuplexStream>,
) {
    let initialize = read_message(reader).await;
    write_message(
        writer,
        json!({"id": initialize["id"], "result": {
            "userAgent": "codex-test", "platformFamily": "unix",
            "platformOs": "linux", "codexHome": "/srv/alice/codex-home"
        }}),
    )
    .await;
    assert_eq!(read_message(reader).await["method"], "initialized");
    let resume = read_message(reader).await;
    assert_eq!(resume["params"]["permissions"], "alice-house-1");
    assert_eq!(
        resume["params"]["config"]["mcp_servers"]["homey-1"]["enabled"],
        true
    );
    write_message(
        writer,
        json!({"id": resume["id"], "result": {
            "thread": {"id": "thread-1"},
            "activePermissionProfile": {"id": "alice-house-1"},
            "sandbox": {"type": "readOnly", "networkAccess": false}
        }}),
    )
    .await;
    let turn = read_message(reader).await;
    write_message(
        writer,
        json!({"id": turn["id"], "result": {"turn": {"id": "turn-1"}}}),
    )
    .await;
}

fn house() -> bridge_core::HouseContext {
    bridge_core::HouseContext {
        id: 1,
        name: "Дом".to_owned(),
        codex_thread_id: Some("thread-1".to_owned()),
        homey_connector_id: "homey-1".to_owned(),
    }
}

async fn tool_completed(writer: &mut WriteHalf<DuplexStream>, tool: &str, structured: Value) {
    write_message(
        writer,
        json!({"method": "item/completed", "params": {
            "threadId": "thread-1", "turnId": "turn-1", "completedAtMs": 1,
            "item": {
                "id": format!("item-{tool}"), "type": "mcpToolCall", "server": "homey-1",
                "tool": tool, "status": "completed", "arguments": {},
                "result": {"content": [], "structuredContent": structured}
            }
        }}),
    )
    .await;
}

async fn finish_turn(writer: &mut WriteHalf<DuplexStream>, speech: &str) {
    write_message(
        writer,
        json!({"method": "item/agentMessage/delta", "params": {
            "threadId": "thread-1", "turnId": "turn-1", "itemId": "answer", "delta": speech
        }}),
    )
    .await;
    write_message(
        writer,
        json!({"method": "turn/completed", "params": {
            "threadId": "thread-1", "turn": {"id": "turn-1", "status": "completed"}
        }}),
    )
    .await;
}

#[tokio::test]
async fn verified_mutation_can_confirm_state_with_only_one_attention_item() {
    let (client, server) = tokio::io::duplex(32 * 1024);
    let task = tokio::spawn(async move {
        runtime()
            .turn_on(client, &house(), "thread-1", "Включи кондиционер")
            .await
    });
    let (read, mut write) = tokio::io::split(server);
    let mut read = BufReader::new(read);
    begin_turn(&mut read, &mut write).await;
    tool_completed(
        &mut write,
        "set_device_capability",
        json!({"requested": true, "observed": true, "verified": true}),
    )
    .await;
    tool_completed(
        &mut write,
        "list_attention_items",
        json!({"items": [
            {"priority": 100, "message": "садится батарейка в датчике окна"},
            {"priority": 10, "message": "датчик кладовой недоступен"}
        ]}),
    )
    .await;
    finish_turn(
        &mut write,
        "Кондиционер включён. И обратите внимание: садится батарейка в датчике окна.",
    )
    .await;

    let speech = task.await.unwrap().unwrap();
    assert!(speech.starts_with("Кондиционер включён."));
    assert_eq!(speech.matches("И обратите внимание:").count(), 1);
    assert!(!speech.contains("кладовой"));
}

#[tokio::test]
async fn unverified_mutation_never_returns_model_success_claim() {
    let (client, server) = tokio::io::duplex(32 * 1024);
    let task = tokio::spawn(async move {
        runtime()
            .turn_on(client, &house(), "thread-1", "Включи кондиционер")
            .await
    });
    let (read, mut write) = tokio::io::split(server);
    let mut read = BufReader::new(read);
    begin_turn(&mut read, &mut write).await;
    tool_completed(
        &mut write,
        "set_device_capability",
        json!({"requested": true, "observed": false, "verified": false}),
    )
    .await;
    finish_turn(&mut write, "Кондиционер включён.").await;

    assert_eq!(
        task.await.unwrap().unwrap(),
        "Не получилось подтвердить изменение устройства."
    );
}

#[tokio::test]
async fn unknown_mcp_tool_fails_closed() {
    let (client, server) = tokio::io::duplex(32 * 1024);
    let task = tokio::spawn(async move {
        runtime()
            .turn_on(client, &house(), "thread-1", "Тест")
            .await
    });
    let (read, mut write) = tokio::io::split(server);
    let mut read = BufReader::new(read);
    begin_turn(&mut read, &mut write).await;
    tool_completed(&mut write, "unlock_front_door", json!({"verified": true})).await;
    finish_turn(&mut write, "Дверь открыта.").await;

    assert!(task.await.unwrap().is_err());
}

#[tokio::test]
async fn allowed_tool_from_another_house_gateway_fails_closed() {
    let (client, server) = tokio::io::duplex(32 * 1024);
    let task = tokio::spawn(async move {
        runtime()
            .turn_on(client, &house(), "thread-1", "Тест")
            .await
    });
    let (read, mut write) = tokio::io::split(server);
    let mut read = BufReader::new(read);
    begin_turn(&mut read, &mut write).await;
    write_message(
        &mut write,
        json!({"method": "item/completed", "params": {
            "threadId": "thread-1", "turnId": "turn-1", "completedAtMs": 1,
            "item": {
                "id": "cross-house", "type": "mcpToolCall", "server": "homey-2",
                "tool": "get_device_state", "status": "completed", "arguments": {},
                "result": {"content": [], "structuredContent": {}}
            }
        }}),
    )
    .await;
    finish_turn(&mut write, "Состояние получено.").await;

    assert!(task.await.unwrap().is_err());
}
