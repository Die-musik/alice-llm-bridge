use std::path::PathBuf;

use bridge_core::{HouseContext, HouseRuntime};
use codex_runtime::{CodexRuntime, CodexRuntimeConfig};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::UnixListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};

fn socket_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "alice-codex-runtime-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

async fn receive_json<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>) -> Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
        panic!("expected a text WebSocket frame")
    };
    serde_json::from_str(&text).unwrap()
}

async fn send_json<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>, value: Value)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}

#[tokio::test]
async fn real_runtime_upgrades_the_unix_socket_to_websocket() {
    let socket_path = socket_path();
    let listener = UnixListener::bind(&socket_path).unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(stream).await.unwrap();
        let initialize = receive_json(&mut socket).await;
        send_json(
            &mut socket,
            json!({"id": initialize["id"], "result": {
                "userAgent": "codex-test", "platformFamily": "unix",
                "platformOs": "linux", "codexHome": "/srv/alice/codex-home"
            }}),
        )
        .await;
        assert_eq!(receive_json(&mut socket).await["method"], "initialized");
        let start = receive_json(&mut socket).await;
        assert_eq!(start["method"], "thread/start");
        send_json(
            &mut socket,
            json!({"id": start["id"], "result": {
                "thread": {"id": "thread-ws"},
                "activePermissionProfile": {"id": "alice-house-1"},
                "sandbox": {"type": "readOnly", "networkAccess": false}
            }}),
        )
        .await;
    });

    let runtime = CodexRuntime::new(CodexRuntimeConfig {
        socket_path: socket_path.clone(),
        cwd_root: PathBuf::from("/srv/alice/houses"),
        permission_profile_prefix: "alice-house-".to_owned(),
    })
    .unwrap();
    let house = HouseContext {
        id: 1,
        name: "Дом".to_owned(),
        codex_thread_id: None,
        homey_connector_id: "homey-1".to_owned(),
    };

    assert_eq!(
        runtime
            .start_thread(&house, "HOUSE INSTRUCTIONS")
            .await
            .unwrap(),
        "thread-ws"
    );
    server.await.unwrap();
    std::fs::remove_file(socket_path).unwrap();
}
