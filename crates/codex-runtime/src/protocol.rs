use serde_json::{Value, json};

pub(crate) const INITIALIZE: &str = "initialize";
pub(crate) const INITIALIZED: &str = "initialized";
pub(crate) const THREAD_START: &str = "thread/start";
pub(crate) const THREAD_RESUME: &str = "thread/resume";
pub(crate) const TURN_START: &str = "turn/start";
pub(crate) const AGENT_MESSAGE_DELTA: &str = "item/agentMessage/delta";
pub(crate) const ITEM_COMPLETED: &str = "item/completed";
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

pub(crate) fn thread_start_params(
    cwd: &str,
    permissions: &str,
    instructions: &str,
    homey_connector: Option<&str>,
) -> Value {
    json!({
        "cwd": cwd,
        "permissions": permissions,
        "developerInstructions": instructions,
        "approvalPolicy": "never",
        "ephemeral": false,
        "config": isolated_house_config(homey_connector)
    })
}

pub(crate) fn thread_resume_params(
    thread_id: &str,
    permissions: &str,
    homey_connector: Option<&str>,
) -> Value {
    json!({
        "threadId": thread_id,
        "permissions": permissions,
        "approvalPolicy": "never",
        "config": isolated_house_config(homey_connector)
    })
}

pub(crate) fn turn_start_params(thread_id: &str, utterance: &str) -> Value {
    json!({
        "threadId": thread_id,
        "input": [{"type": "text", "text": utterance}]
    })
}

fn isolated_house_config(homey_connector: Option<&str>) -> Value {
    let mut config = json!({
        "features": {
            "shell_tool": false,
            "unified_exec": false,
            "skill_mcp_dependency_install": false
        },
        "tools": {
            "web_search": false,
            "view_image": false
        },
        "apps": {
            "_default": {"enabled": false}
        }
    });
    if let Some(homey_connector) = homey_connector {
        config["mcp_servers"] = json!({homey_connector: {"enabled": true}});
    }
    config
}
