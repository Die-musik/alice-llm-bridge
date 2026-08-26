pub mod client;
pub mod error;
pub mod protocol;

use std::path::PathBuf;

use bridge_core::{HouseContext, HouseRuntime, RuntimeError as HouseRuntimeError};
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::UnixStream,
};

use client::{JsonRpcClient, JsonTransport};
use error::Result;
use protocol::{
    INITIALIZE, INITIALIZED, THREAD_RESUME, THREAD_START, TURN_START, initialize_params,
    thread_resume_params, thread_start_params, turn_start_params,
};

pub use error::CodexRuntimeError;

#[derive(Debug, Clone)]
pub struct CodexRuntimeConfig {
    pub socket_path: PathBuf,
    pub cwd_root: PathBuf,
    pub permission_profile_prefix: String,
}

#[derive(Debug, Clone)]
pub struct CodexRuntime {
    config: CodexRuntimeConfig,
}

impl CodexRuntime {
    pub fn new(config: CodexRuntimeConfig) -> Result<Self> {
        if !config.socket_path.is_absolute() {
            return Err(CodexRuntimeError::InvalidConfig(
                "socket path must be absolute",
            ));
        }
        if !config.cwd_root.is_absolute() {
            return Err(CodexRuntimeError::InvalidConfig(
                "house cwd root must be absolute",
            ));
        }
        if config.permission_profile_prefix.is_empty()
            || !config
                .permission_profile_prefix
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
        {
            return Err(CodexRuntimeError::InvalidConfig(
                "permission profile prefix is invalid",
            ));
        }
        Ok(Self { config })
    }

    pub async fn start_thread_on<S>(
        &self,
        stream: S,
        house: &HouseContext,
        instructions: &str,
    ) -> Result<String>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin,
    {
        let mut client = JsonRpcClient::jsonl(stream);
        self.start_thread_with(&mut client, house, instructions)
            .await
    }

    async fn start_thread_with<T>(
        &self,
        client: &mut JsonRpcClient<T>,
        house: &HouseContext,
        instructions: &str,
    ) -> Result<String>
    where
        T: JsonTransport,
    {
        if house.id <= 0 {
            return Err(CodexRuntimeError::InvalidConfig(
                "house id must be positive",
            ));
        }
        validate_homey_connector(&house.homey_connector_id)?;
        initialize(client).await?;
        let cwd = self.config.cwd_root.join(house.id.to_string());
        let cwd = cwd
            .to_str()
            .ok_or(CodexRuntimeError::InvalidConfig("house cwd is not UTF-8"))?;
        let permissions = format!("{}{}", self.config.permission_profile_prefix, house.id);
        let result = client
            .request(
                THREAD_START,
                thread_start_params(cwd, &permissions, instructions, &house.homey_connector_id),
            )
            .await?;
        validate_permission_result(&result, Some(&permissions))?;
        required_string(
            &result,
            "/thread/id",
            "thread/start response has no thread id",
        )
    }

    pub async fn turn_on<S>(
        &self,
        stream: S,
        house: &HouseContext,
        thread_id: &str,
        utterance: &str,
    ) -> Result<String>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin,
    {
        let mut client = JsonRpcClient::jsonl(stream);
        self.turn_with(&mut client, house, thread_id, utterance)
            .await
    }

    async fn turn_with<T>(
        &self,
        client: &mut JsonRpcClient<T>,
        house: &HouseContext,
        thread_id: &str,
        utterance: &str,
    ) -> Result<String>
    where
        T: JsonTransport,
    {
        if thread_id.is_empty() {
            return Err(CodexRuntimeError::Protocol("thread id is empty"));
        }
        if house.id <= 0 {
            return Err(CodexRuntimeError::InvalidConfig(
                "house id must be positive",
            ));
        }
        validate_homey_connector(&house.homey_connector_id)?;
        initialize(client).await?;
        let permissions = format!("{}{}", self.config.permission_profile_prefix, house.id);
        let resumed = client
            .request(
                THREAD_RESUME,
                thread_resume_params(thread_id, &permissions, &house.homey_connector_id),
            )
            .await?;
        validate_permission_result(&resumed, Some(&permissions))?;
        if required_string(
            &resumed,
            "/thread/id",
            "thread/resume response has no thread id",
        )? != thread_id
        {
            return Err(CodexRuntimeError::Protocol(
                "thread/resume returned a different thread",
            ));
        }
        let started = client
            .request(TURN_START, turn_start_params(thread_id, utterance))
            .await?;
        let turn_id = required_string(&started, "/turn/id", "turn/start response has no turn id")?;
        client
            .collect_turn(thread_id, &turn_id, &house.homey_connector_id)
            .await
    }
}

async fn initialize<T>(client: &mut JsonRpcClient<T>) -> Result<()>
where
    T: JsonTransport,
{
    client.request(INITIALIZE, initialize_params()).await?;
    client.notify(INITIALIZED, None).await
}

fn required_string(result: &Value, pointer: &str, message: &'static str) -> Result<String> {
    result
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(CodexRuntimeError::Protocol(message))
}

fn validate_permission_result(result: &Value, expected_profile: Option<&str>) -> Result<()> {
    let profile = result
        .pointer("/activePermissionProfile/id")
        .and_then(Value::as_str)
        .ok_or(CodexRuntimeError::Protocol(
            "app-server did not activate a permission profile",
        ))?;
    if expected_profile.is_some_and(|expected| expected != profile) {
        return Err(CodexRuntimeError::Protocol(
            "app-server activated a different permission profile",
        ));
    }
    if result.pointer("/sandbox/type").and_then(Value::as_str) != Some("readOnly")
        || result
            .pointer("/sandbox/networkAccess")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return Err(CodexRuntimeError::Protocol(
            "permission profile is not read-only and network-isolated",
        ));
    }
    Ok(())
}

fn validate_homey_connector(connector: &str) -> Result<()> {
    if !connector.starts_with("homey-")
        || !connector
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(CodexRuntimeError::InvalidConfig(
            "Homey connector id is invalid",
        ));
    }
    Ok(())
}

#[async_trait::async_trait]
impl HouseRuntime for CodexRuntime {
    async fn start_thread(
        &self,
        house: &HouseContext,
        instructions: &str,
    ) -> std::result::Result<String, HouseRuntimeError> {
        let stream = UnixStream::connect(&self.config.socket_path)
            .await
            .map_err(|_| HouseRuntimeError("Codex app-server is unavailable".to_owned()))?;
        let mut client = JsonRpcClient::unix_websocket(stream)
            .await
            .map_err(map_house_error)?;
        self.start_thread_with(&mut client, house, instructions)
            .await
            .map_err(map_house_error)
    }

    async fn turn(
        &self,
        house: &HouseContext,
        thread_id: &str,
        utterance: &str,
    ) -> std::result::Result<String, HouseRuntimeError> {
        let stream = UnixStream::connect(&self.config.socket_path)
            .await
            .map_err(|_| HouseRuntimeError("Codex app-server is unavailable".to_owned()))?;
        let mut client = JsonRpcClient::unix_websocket(stream)
            .await
            .map_err(map_house_error)?;
        self.turn_with(&mut client, house, thread_id, utterance)
            .await
            .map_err(map_house_error)
    }
}

fn map_house_error(error: CodexRuntimeError) -> HouseRuntimeError {
    HouseRuntimeError(error.to_string())
}
