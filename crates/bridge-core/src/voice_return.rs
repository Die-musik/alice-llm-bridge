#[async_trait::async_trait]
pub trait VoiceReturn: Send + Sync {
    fn supports(&self, application_id: &str) -> bool;

    async fn answer_ready(&self, application_id: &str) -> Result<(), String>;
}

#[derive(Debug, Default)]
pub struct DisabledVoiceReturn;

#[async_trait::async_trait]
impl VoiceReturn for DisabledVoiceReturn {
    fn supports(&self, _application_id: &str) -> bool {
        false
    }

    async fn answer_ready(&self, _application_id: &str) -> Result<(), String> {
        Ok(())
    }
}
