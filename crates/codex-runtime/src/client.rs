use std::collections::VecDeque;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf,
};
use tokio::net::UnixStream;
use tokio_tungstenite::{WebSocketStream, client_async, tungstenite::Message};

use crate::error::{CodexRuntimeError, Result};
use crate::protocol::{AGENT_MESSAGE_DELTA, ITEM_COMPLETED, TURN_COMPLETED};

const MAX_JSONL_BYTES: usize = 1024 * 1024;

#[async_trait]
pub(crate) trait JsonTransport: Send {
    async fn read_message(&mut self) -> Result<Value>;
    async fn write_message(&mut self, message: &Value) -> Result<()>;
}

pub(crate) struct JsonlTransport<R, W> {
    reader: BufReader<R>,
    writer: W,
}

pub(crate) struct WebSocketTransport<S> {
    socket: WebSocketStream<S>,
}

pub(crate) struct JsonRpcClient<T> {
    transport: T,
    next_id: u64,
    notifications: VecDeque<Value>,
}

impl<S> JsonRpcClient<JsonlTransport<ReadHalf<S>, WriteHalf<S>>>
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    pub(crate) fn jsonl(stream: S) -> Self {
        let (reader, writer) = tokio::io::split(stream);
        Self {
            transport: JsonlTransport {
                reader: BufReader::new(reader),
                writer,
            },
            next_id: 1,
            notifications: VecDeque::new(),
        }
    }
}

impl JsonRpcClient<WebSocketTransport<UnixStream>> {
    pub(crate) async fn unix_websocket(stream: UnixStream) -> Result<Self> {
        let (socket, _) = client_async("ws://localhost/", stream)
            .await
            .map_err(|_| CodexRuntimeError::Transport)?;
        Ok(Self {
            transport: WebSocketTransport { socket },
            next_id: 1,
            notifications: VecDeque::new(),
        })
    }
}

impl<T> JsonRpcClient<T>
where
    T: JsonTransport,
{
    pub(crate) async fn notify(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        let mut message = json!({"method": method});
        if let Some(params) = params {
            message["params"] = params;
        }
        self.transport.write_message(&message).await
    }

    pub(crate) async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.transport
            .write_message(&json!({"id": id, "method": method, "params": params}))
            .await?;

        loop {
            let message = self.transport.read_message().await?;
            if message.get("method").is_some() {
                if message.get("id").is_some() {
                    self.handle_server_request(&message).await?;
                } else {
                    self.notifications.push_back(message);
                }
                continue;
            }

            if message.get("id").and_then(Value::as_u64) != Some(id) {
                return Err(CodexRuntimeError::Protocol(
                    "response id does not match the request",
                ));
            }
            if let Some(error) = message.get("error") {
                let code = error.get("code").and_then(Value::as_i64).unwrap_or(-1);
                return Err(CodexRuntimeError::Server(code));
            }
            return message
                .get("result")
                .cloned()
                .ok_or(CodexRuntimeError::Protocol("response has no result"));
        }
    }

    pub(crate) async fn collect_turn(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        homey_connector: Option<&str>,
    ) -> Result<String> {
        let mut assistant_text = String::new();
        let mut unverified_homey_mutation = false;
        loop {
            let message = if let Some(message) = self.notifications.pop_front() {
                message
            } else {
                self.transport.read_message().await?
            };

            if message.get("method").is_some() && message.get("id").is_some() {
                self.handle_server_request(&message).await?;
                continue;
            }
            let Some(method) = message.get("method").and_then(Value::as_str) else {
                return Err(CodexRuntimeError::Protocol(
                    "unexpected response while waiting for turn",
                ));
            };
            match method {
                AGENT_MESSAGE_DELTA => {
                    require_turn(&message, thread_id, turn_id)?;
                    let delta = message
                        .pointer("/params/delta")
                        .and_then(Value::as_str)
                        .ok_or(CodexRuntimeError::Protocol("agent delta has no text"))?;
                    assistant_text.push_str(delta);
                }
                ITEM_COMPLETED => {
                    observe_homey_tool(
                        &message,
                        thread_id,
                        turn_id,
                        homey_connector,
                        &mut unverified_homey_mutation,
                    )?;
                }
                TURN_COMPLETED => {
                    require_completed_turn(&message, thread_id, turn_id)?;
                    if unverified_homey_mutation {
                        return Ok("Не получилось подтвердить изменение устройства.".to_owned());
                    }
                    if assistant_text.is_empty() {
                        return Err(CodexRuntimeError::Protocol(
                            "completed turn has no assistant text",
                        ));
                    }
                    return Ok(assistant_text);
                }
                _ => {
                    // App-server emits lifecycle notifications that are not needed by
                    // the voice bridge. Requests, unlike notifications, are never ignored.
                }
            }
        }
    }

    async fn handle_server_request(&mut self, message: &Value) -> Result<()> {
        let id = message
            .get("id")
            .cloned()
            .ok_or(CodexRuntimeError::Protocol("server request has no id"))?;
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .ok_or(CodexRuntimeError::Protocol("server request has no method"))?;
        match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                self.transport
                    .write_message(&json!({"id": id, "result": {"decision": "decline"}}))
                    .await
            }
            _ => {
                self.transport
                    .write_message(&json!({
                        "id": id,
                        "error": {"code": -32601, "message": "method not allowed"}
                    }))
                    .await?;
                Err(CodexRuntimeError::ForbiddenRequest)
            }
        }
    }
}

#[async_trait]
impl<R, W> JsonTransport for JsonlTransport<R, W>
where
    R: AsyncRead + Send + Unpin,
    W: AsyncWrite + Send + Unpin,
{
    async fn read_message(&mut self) -> Result<Value> {
        let mut line = String::new();
        let bytes = self
            .reader
            .read_line(&mut line)
            .await
            .map_err(|_| CodexRuntimeError::Transport)?;
        if bytes == 0 {
            return Err(CodexRuntimeError::EndOfStream);
        }
        if bytes > MAX_JSONL_BYTES {
            return Err(CodexRuntimeError::MessageTooLarge);
        }
        serde_json::from_str(&line).map_err(|_| CodexRuntimeError::InvalidMessage)
    }

    async fn write_message(&mut self, message: &Value) -> Result<()> {
        let encoded = serde_json::to_vec(message).map_err(|_| CodexRuntimeError::InvalidMessage)?;
        self.writer
            .write_all(&encoded)
            .await
            .map_err(|_| CodexRuntimeError::Transport)?;
        self.writer
            .write_all(b"\n")
            .await
            .map_err(|_| CodexRuntimeError::Transport)?;
        self.writer
            .flush()
            .await
            .map_err(|_| CodexRuntimeError::Transport)
    }
}

#[async_trait]
impl<S> JsonTransport for WebSocketTransport<S>
where
    S: AsyncRead + AsyncWrite + Send + Unpin,
{
    async fn read_message(&mut self) -> Result<Value> {
        loop {
            let message = self
                .socket
                .next()
                .await
                .ok_or(CodexRuntimeError::EndOfStream)?
                .map_err(|_| CodexRuntimeError::Transport)?;
            match message {
                Message::Text(text) => {
                    if text.len() > MAX_JSONL_BYTES {
                        return Err(CodexRuntimeError::MessageTooLarge);
                    }
                    return serde_json::from_str(&text)
                        .map_err(|_| CodexRuntimeError::InvalidMessage);
                }
                Message::Ping(payload) => self
                    .socket
                    .send(Message::Pong(payload))
                    .await
                    .map_err(|_| CodexRuntimeError::Transport)?,
                Message::Pong(_) => {}
                Message::Close(_) => return Err(CodexRuntimeError::EndOfStream),
                Message::Binary(_) | Message::Frame(_) => {
                    return Err(CodexRuntimeError::InvalidMessage);
                }
            }
        }
    }

    async fn write_message(&mut self, message: &Value) -> Result<()> {
        let encoded =
            serde_json::to_string(message).map_err(|_| CodexRuntimeError::InvalidMessage)?;
        if encoded.len() > MAX_JSONL_BYTES {
            return Err(CodexRuntimeError::MessageTooLarge);
        }
        self.socket
            .send(Message::Text(encoded.into()))
            .await
            .map_err(|_| CodexRuntimeError::Transport)
    }
}

fn require_turn(message: &Value, thread_id: &str, turn_id: &str) -> Result<()> {
    if message.pointer("/params/threadId").and_then(Value::as_str) != Some(thread_id)
        || message.pointer("/params/turnId").and_then(Value::as_str) != Some(turn_id)
    {
        return Err(CodexRuntimeError::Protocol(
            "notification belongs to another thread or turn",
        ));
    }
    Ok(())
}

fn require_completed_turn(message: &Value, thread_id: &str, turn_id: &str) -> Result<()> {
    if message.pointer("/params/threadId").and_then(Value::as_str) != Some(thread_id)
        || message.pointer("/params/turn/id").and_then(Value::as_str) != Some(turn_id)
        || message
            .pointer("/params/turn/status")
            .and_then(Value::as_str)
            != Some("completed")
    {
        return Err(CodexRuntimeError::Protocol(
            "turn completed with mismatched identity or status",
        ));
    }
    Ok(())
}

fn observe_homey_tool(
    message: &Value,
    thread_id: &str,
    turn_id: &str,
    homey_connector: Option<&str>,
    unverified_mutation: &mut bool,
) -> Result<()> {
    require_turn(message, thread_id, turn_id)?;
    if message.pointer("/params/item/type").and_then(Value::as_str) != Some("mcpToolCall") {
        return Ok(());
    }

    if message
        .pointer("/params/item/server")
        .and_then(Value::as_str)
        != homey_connector
    {
        return Err(CodexRuntimeError::ForbiddenRequest);
    }

    let tool = message
        .pointer("/params/item/tool")
        .and_then(Value::as_str)
        .ok_or(CodexRuntimeError::Protocol("MCP item has no tool name"))?;
    match tool {
        "list_attention_items" | "get_device_state" => Ok(()),
        "set_device_capability" => {
            let completed = message
                .pointer("/params/item/status")
                .and_then(Value::as_str)
                == Some("completed");
            let verified = message
                .pointer("/params/item/result/structuredContent/verified")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !completed || !verified {
                *unverified_mutation = true;
            }
            Ok(())
        }
        _ => Err(CodexRuntimeError::ForbiddenRequest),
    }
}
