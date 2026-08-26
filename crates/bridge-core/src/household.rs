#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SurfaceIdentity {
    pub user_id: String,
    pub application_id: String,
}

impl SurfaceIdentity {
    pub fn new(user_id: impl Into<String>, application_id: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            application_id: application_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HouseContext {
    pub id: i64,
    pub name: String,
    pub codex_thread_id: Option<String>,
    pub homey_connector_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceResolution {
    Bound(HouseContext),
    PairingRequired { spoken_code: String },
    Unauthorized,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingReply {
    None,
    Thinking,
    Ready(String),
}
