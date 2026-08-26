use serde_json::{Value, json};

pub(crate) const INITIALIZE: &str = "initialize";
pub(crate) const INITIALIZED: &str = "initialized";
pub(crate) const THREAD_START: &str = "thread/start";
pub(crate) const THREAD_RESUME: &str = "thread/resume";
pub(crate) const TURN_START: &str = "turn/start";
pub(crate) const AGENT_MESSAGE_DELTA: &str = "item/agentMessage/delta";
pub(crate) const TURN_COMPLETED: &str = "turn/completed";

pub(crate) fn initialize_params() -> Value {
    json!({
        "clientInfo": {
            "name": "alice-household-bridge",
            "version": env!("CARGO_PKG_VERSION")
        },
        "capabilities": {
            "experimentalApi": true
        }
    })
}

pub(crate) fn thread_start_params(cwd: &str, permissions: &str, instructions: &str) -> Value {
    json!({
        "cwd": cwd,
        "permissions": permissions,
        "developerInstructions": instructions,
        "approvalPolicy": "never",
        "ephemeral": false
    })
}

pub(crate) fn thread_resume_params(thread_id: &str) -> Value {
    json!({"threadId": thread_id})
}

pub(crate) fn turn_start_params(thread_id: &str, utterance: &str) -> Value {
    json!({
        "threadId": thread_id,
        "input": [{"type": "text", "text": utterance}]
    })
}
