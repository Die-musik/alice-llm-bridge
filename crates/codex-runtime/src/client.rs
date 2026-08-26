use std::collections::VecDeque;

use serde_json::{Value, json};
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf,
};

use crate::error::{CodexRuntimeError, Result};
use crate::protocol::{AGENT_MESSAGE_DELTA, TURN_COMPLETED};

const MAX_JSONL_BYTES: usize = 1024 * 1024;

pub(crate) struct JsonlClient<R, W> {
    reader: BufReader<R>,
    writer: W,
    next_id: u64,
    notifications: VecDeque<Value>,
}

impl<S> JsonlClient<ReadHalf<S>, WriteHalf<S>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    pub(crate) fn new(stream: S) -> Self {
        let (reader, writer) = tokio::io::split(stream);
        Self {
            reader: BufReader::new(reader),
            writer,
            next_id: 1,
            notifications: VecDeque::new(),
        }
    }
}

impl<R, W> JsonlClient<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub(crate) async fn notify(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        let mut message = json!({"method": method});
        if let Some(params) = params {
            message["params"] = params;
        }
        self.write_message(&message).await
    }

    pub(crate) async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_message(&json!({"id": id, "method": method, "params": params}))
            .await?;

        loop {
            let message = self.read_message().await?;
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

    pub(crate) async fn collect_turn(&mut self, thread_id: &str, turn_id: &str) -> Result<String> {
        let mut assistant_text = String::new();
        loop {
            let message = if let Some(message) = self.notifications.pop_front() {
                message
            } else {
                self.read_message().await?
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
                TURN_COMPLETED => {
                    require_completed_turn(&message, thread_id, turn_id)?;
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
                self.write_message(&json!({"id": id, "result": {"decision": "decline"}}))
                    .await
            }
            _ => {
                self.write_message(&json!({
                    "id": id,
                    "error": {"code": -32601, "message": "method not allowed"}
                }))
                .await?;
                Err(CodexRuntimeError::ForbiddenRequest)
            }
        }
    }

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
