//! TOML configuration schema and validation.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Failures while loading or validating the configuration file.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("environment variable {0} is not set")]
    MissingEnv(String),
    #[error("model {model} references unknown provider {provider}")]
    UnknownProvider { model: String, provider: String },
    #[error("invalid config: {0}")]
    Invalid(String),
}

/// Root of `config.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    #[serde(default)]
    pub runtime: RuntimeConfig,
    pub server: ServerConfig,
    pub defaults: DefaultsConfig,
    pub providers: HashMap<String, ProviderConfig>,
    pub models: ModelsConfig,
    pub profiles: Vec<ProfileConfig>,
    #[serde(default)]
    pub modes: Vec<ModeConfig>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    #[default]
    Legacy,
    HouseholdCodex,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    #[serde(default)]
    pub mode: RuntimeMode,
    #[serde(default)]
    pub codex_socket: Option<PathBuf>,
    #[serde(default)]
    pub codex_cwd_root: Option<PathBuf>,
    #[serde(default)]
    pub codex_model: Option<String>,
    #[serde(default)]
    pub codex_effort: Option<String>,
    #[serde(default = "default_permission_profile_prefix")]
    pub permission_profile_prefix: String,
    #[serde(default = "default_chunk_limit")]
    pub chunk_limit: usize,
    /// Phase-wide gate. Move this to a per-house setting before one house
    /// needs Homey while another must remain chat-only.
    #[serde(default)]
    pub homey_enabled: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            mode: RuntimeMode::Legacy,
            codex_socket: None,
            codex_cwd_root: None,
            codex_model: None,
            codex_effort: None,
            permission_profile_prefix: default_permission_profile_prefix(),
            chunk_limit: default_chunk_limit(),
            homey_enabled: false,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub listen: SocketAddr,
    /// Yandex account `user_id`s allowed to use the skill; empty means
    /// unrestricted (the draft-skill visibility is the only gate).
    #[serde(default)]
    pub allowed_user_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultsConfig {
    pub profile: String,
    #[serde(default = "default_context_window")]
    pub context_window: usize,
    #[serde(default = "default_reply_budget_ms")]
    pub reply_budget_ms: u64,
    #[serde(default = "default_provider_timeout_secs")]
    pub provider_timeout_secs: u64,
    #[serde(default = "default_utc_offset_hours")]
    pub utc_offset_hours: i32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub base_url: String,
    /// Name of the environment variable holding the API key.
    pub api_key_env: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelsConfig {
    pub fast: ModelPresetConfig,
    pub smart: ModelPresetConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPresetConfig {
    pub provider: String,
    pub model: String,
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
    pub input_price_per_mtok: f64,
    pub output_price_per_mtok: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub birthday: Option<chrono::NaiveDate>,
    /// `"adult"` or `"child"`.
    pub role: String,
    #[serde(default)]
    pub persona: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeConfig {
    pub name: String,
    pub triggers: Vec<String>,
    pub prompt: String,
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        Self::from_toml(&std::fs::read_to_string(path)?)
    }

    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let config: AppConfig = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.runtime.mode == RuntimeMode::HouseholdCodex {
            let socket = self.runtime.codex_socket.as_deref().ok_or_else(|| {
                ConfigError::Invalid("household runtime requires codex_socket".to_owned())
            })?;
            let cwd_root = self.runtime.codex_cwd_root.as_deref().ok_or_else(|| {
                ConfigError::Invalid("household runtime requires codex_cwd_root".to_owned())
            })?;
            if !socket.is_absolute() || !cwd_root.is_absolute() {
                return Err(ConfigError::Invalid(
                    "household Codex paths must be absolute".to_owned(),
                ));
            }
            if self.runtime.chunk_limit <= " Продолжать?".chars().count()
                || self.runtime.chunk_limit > 900
            {
                return Err(ConfigError::Invalid(
                    "household chunk_limit must leave room for continuation and be at most 900"
                        .to_owned(),
                ));
            }
            if self.runtime.permission_profile_prefix.is_empty()
                || !self
                    .runtime
                    .permission_profile_prefix
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
            {
                return Err(ConfigError::Invalid(
                    "household permission_profile_prefix is invalid".to_owned(),
                ));
            }
            match (
                self.runtime.codex_model.as_deref(),
                self.runtime.codex_effort.as_deref(),
            ) {
                (None, None) => {}
                (Some(model), Some(effort))
                    if !model.trim().is_empty()
                        && matches!(effort, "minimal" | "low" | "medium" | "high" | "xhigh") => {}
                _ => {
                    return Err(ConfigError::Invalid(
                        "household codex_model and supported codex_effort must be set together"
                            .to_owned(),
                    ));
                }
            }
        }
        for preset in [&self.models.fast, &self.models.smart] {
            if !self.providers.contains_key(&preset.provider) {
                return Err(ConfigError::UnknownProvider {
                    model: preset.model.clone(),
                    provider: preset.provider.clone(),
                });
            }
        }
        if !self
            .profiles
            .iter()
            .any(|p| p.name == self.defaults.profile)
        {
            return Err(ConfigError::Invalid(format!(
                "default profile {} is not defined",
                self.defaults.profile
            )));
        }
        for profile in &self.profiles {
            if profile.role != "adult" && profile.role != "child" {
                return Err(ConfigError::Invalid(format!(
                    "profile {}: role must be adult or child",
                    profile.name
                )));
            }
        }
        Ok(())
    }
}

fn default_context_window() -> usize {
    12
}
fn default_reply_budget_ms() -> u64 {
    2800
}
fn default_provider_timeout_secs() -> u64 {
    45
}
fn default_utc_offset_hours() -> i32 {
    3
}
fn default_temperature() -> f32 {
    0.7
}
fn default_permission_profile_prefix() -> String {
    "alice-house-".to_owned()
}
fn default_chunk_limit() -> usize {
    850
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> String {
        r#"
[server]
listen = "127.0.0.1:8080"
allowed_user_ids = ["USER1"]

[defaults]
profile = "Дима"

[providers.deepseek]
base_url = "https://api.deepseek.com/v1"
api_key_env = "TEST_DEEPSEEK_KEY"

[models.fast]
provider = "deepseek"
model = "deepseek-chat"
max_tokens = 300
input_price_per_mtok = 0.27
output_price_per_mtok = 1.10

[models.smart]
provider = "deepseek"
model = "deepseek-reasoner"
max_tokens = 400
input_price_per_mtok = 0.55
output_price_per_mtok = 2.19

[[profiles]]
name = "Дима"
aliases = ["дима"]
birthday = "1985-03-10"
role = "adult"
persona = "Общайся на равных."

[[modes]]
name = "fairy_tale"
triggers = ["расскажи сказку"]
prompt = "Рассказывай сказки."
"#
        .to_string()
    }

    #[test]
    fn parses_full_config_with_defaults() {
        let config = AppConfig::from_toml(&sample()).unwrap();
        assert_eq!(config.runtime.mode, RuntimeMode::Legacy);
        assert_eq!(config.defaults.context_window, 12);
        assert_eq!(config.defaults.reply_budget_ms, 2800);
        assert_eq!(config.defaults.utc_offset_hours, 3);
        assert_eq!(config.models.fast.temperature, 0.7);
        assert_eq!(config.profiles[0].name, "Дима");
        assert_eq!(config.profiles[0].role, "adult");
        assert_eq!(config.modes.len(), 1);
    }

    #[test]
    fn rejects_model_with_unknown_provider() {
        let broken = sample().replace(r#"provider = "deepseek""#, r#"provider = "nope""#);
        let err = AppConfig::from_toml(&broken).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownProvider { .. }));
    }

    #[test]
    fn rejects_unknown_default_profile() {
        let broken = sample().replace(r#"profile = "Дима""#, r#"profile = "Вася""#);
        let err = AppConfig::from_toml(&broken).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    #[test]
    fn parses_household_runtime_with_absolute_paths() {
        let household = format!(
            r#"
[runtime]
mode = "household_codex"
codex_socket = "/run/alice-codex/app-server.sock"
codex_cwd_root = "/srv/alice/houses"
permission_profile_prefix = "alice-house-"
chunk_limit = 850
codex_model = "gpt-5.6-luna"
codex_effort = "low"

{}"#,
            sample()
        );
        let config = AppConfig::from_toml(&household).unwrap();
        assert_eq!(config.runtime.mode, RuntimeMode::HouseholdCodex);
        assert_eq!(config.runtime.chunk_limit, 850);
        assert_eq!(config.runtime.codex_model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(config.runtime.codex_effort.as_deref(), Some("low"));
        assert!(!config.runtime.homey_enabled);
    }

    #[test]
    fn household_runtime_rejects_unknown_codex_effort() {
        let household = format!(
            r#"
[runtime]
mode = "household_codex"
codex_socket = "/run/alice-codex/app-server.sock"
codex_cwd_root = "/srv/alice/houses"
codex_model = "gpt-5.6-luna"
codex_effort = "instant"

{}"#,
            sample()
        );

        assert!(matches!(
            AppConfig::from_toml(&household),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn household_runtime_can_explicitly_enable_homey() {
        let household = format!(
            r#"
[runtime]
mode = "household_codex"
codex_socket = "/run/alice-codex/app-server.sock"
codex_cwd_root = "/srv/alice/houses"
homey_enabled = true

{}"#,
            sample()
        );

        assert!(
            AppConfig::from_toml(&household)
                .unwrap()
                .runtime
                .homey_enabled
        );
    }

    #[test]
    fn household_runtime_rejects_relative_paths_and_oversized_chunks() {
        for broken_runtime in [
            r#"
[runtime]
mode = "household_codex"
codex_socket = "relative.sock"
codex_cwd_root = "/srv/alice/houses"
permission_profile_prefix = "alice-house-"
chunk_limit = 850
"#,
            r#"
[runtime]
mode = "household_codex"
codex_socket = "/run/alice-codex/app-server.sock"
codex_cwd_root = "relative/houses"
permission_profile_prefix = "alice-house-"
chunk_limit = 850
"#,
            r#"
[runtime]
mode = "household_codex"
codex_socket = "/run/alice-codex/app-server.sock"
codex_cwd_root = "/srv/alice/houses"
permission_profile_prefix = "alice-house-"
chunk_limit = 901
"#,
        ] {
            assert!(matches!(
                AppConfig::from_toml(&format!("{broken_runtime}\n{}", sample())),
                Err(ConfigError::Invalid(_))
            ));
        }
    }
}
