//! Dialogue engine for the Alice LLM bridge.
//!
//! The domain layer: family profiles, voice command parsing, prompt and
//! context assembly, deferred answers. Depends on abstractions only —
//! [`llm_providers::ChatProvider`] for models and [`ConversationStore`]
//! for persistence — so it stays free of HTTP and database concerns.

pub mod command;
mod engine;
mod error;
pub mod house_engine;
pub mod house_prompt;
pub mod house_runtime;
pub mod house_store;
pub mod household;
mod mode;
mod model;
mod pending;
pub mod phrases;
mod profile;
mod prompt;
pub mod reply;
mod store;
pub mod testing;
mod voice_return;

pub use engine::{Engine, EngineConfig};
pub use error::{CoreError, Result};
pub use house_engine::{HouseholdEngine, HouseholdEngineConfig, HouseholdInput, HouseholdReply};
pub use house_prompt::build_house_instructions;
pub use house_runtime::{HouseRuntime, RuntimeError, RuntimeResult};
pub use house_store::{HouseholdStore, HouseholdStoreError, StoreResult};
pub use household::{HouseContext, PendingReply, SurfaceIdentity, SurfaceResolution};
pub use mode::Mode;
pub use model::{ModelPreset, ModelRegistry, ModelTier, cost_micros};
pub use pending::{PendingAnswers, Poll};
pub use profile::{FamilyRoster, Profile, ProfileRole};
pub use prompt::{PromptContext, build_system_prompt};
pub use store::{
    ConversationStore, ExchangeRecord, MessageRole, StoreError, StoredMessage, Summary, UsageStats,
};
pub use voice_return::{DisabledVoiceReturn, VoiceReturn};
