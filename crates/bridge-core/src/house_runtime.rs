use crate::household::HouseContext;

pub type RuntimeResult<T> = Result<T, RuntimeError>;

#[derive(Debug, thiserror::Error)]
#[error("house runtime error: {0}")]
pub struct RuntimeError(pub String);

#[async_trait::async_trait]
pub trait HouseRuntime: Send + Sync {
    async fn start_thread_and_turn(
        &self,
        house: &HouseContext,
        instructions: &str,
        utterance: &str,
    ) -> RuntimeResult<(String, String)>;

    async fn turn(
        &self,
        house: &HouseContext,
        thread_id: &str,
        utterance: &str,
    ) -> RuntimeResult<String>;
}
