#[derive(Debug, thiserror::Error)]
pub enum CodexRuntimeError {
    #[error("invalid Codex runtime configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("Codex app-server transport failed")]
    Transport,
    #[error("Codex app-server closed the connection")]
    EndOfStream,
    #[error("Codex app-server sent invalid JSONL")]
    InvalidMessage,
    #[error("Codex app-server line exceeds the safety limit")]
    MessageTooLarge,
    #[error("Codex app-server returned JSON-RPC error {0}")]
    Server(i64),
    #[error("Codex app-server protocol violation: {0}")]
    Protocol(&'static str),
    #[error("Codex app-server requested a forbidden operation")]
    ForbiddenRequest,
}

pub type Result<T> = std::result::Result<T, CodexRuntimeError>;
