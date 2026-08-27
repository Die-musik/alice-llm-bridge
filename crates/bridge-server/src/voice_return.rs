use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use bridge_core::VoiceReturn;
use reqwest::header::ORIGIN;
use serde::Deserialize;

#[derive(Clone)]
pub struct YandexVoiceReturnConfig {
    pub x_token: String,
    pub activation_name: String,
    pub targets: Vec<YandexVoiceReturnTarget>,
}

#[derive(Clone)]
pub struct YandexVoiceReturnTarget {
    pub application_id: String,
    pub device_id: String,
    pub scenario_name: String,
}

#[derive(Clone)]
pub struct YandexEndpoints {
    x_token_auth: String,
    quasar_page: String,
    scenario_list: String,
    scenario_create: String,
    scenario_update: String,
    scenario_actions: String,
}

impl Default for YandexEndpoints {
    fn default() -> Self {
        Self {
            x_token_auth: "https://mobileproxy.passport.yandex.net/1/bundle/auth/x_token/"
                .to_owned(),
            quasar_page: "https://yandex.ru/quasar/iot".to_owned(),
            scenario_list: "https://iot.quasar.yandex.ru/m/user/scenarios".to_owned(),
            scenario_create: "https://iot.quasar.yandex.ru/m/v4/user/scenarios".to_owned(),
            scenario_update: "https://iot.quasar.yandex.ru/m/v4/user/scenarios".to_owned(),
            scenario_actions: "https://iot.quasar.yandex.ru/m/user/scenarios".to_owned(),
        }
    }
}

impl YandexEndpoints {
    #[doc(hidden)]
    pub fn under(base: &str) -> Self {
        let base = base.trim_end_matches('/');
        Self {
            x_token_auth: format!("{base}/auth/x_token"),
            quasar_page: format!("{base}/quasar/iot"),
            scenario_list: format!("{base}/scenarios"),
            scenario_create: format!("{base}/scenarios/create"),
            scenario_update: format!("{base}/scenarios"),
            scenario_actions: format!("{base}/scenarios"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum YandexVoiceReturnError {
    #[error("invalid voice-return configuration")]
    InvalidConfig,
    #[error("Yandex authentication failed")]
    Authentication,
    #[error("Yandex Smart Home request failed")]
    Request,
    #[error("Yandex Smart Home returned an invalid response")]
    InvalidResponse,
    #[error("voice-return surface is not configured")]
    UnknownSurface,
}

pub struct YandexVoiceReturn {
    // The Quasar web session is intentionally established once at process start.
    // If production observes authentication expiry before the next normal deploy,
    // replace this with one serialized re-authentication and a single request retry;
    // until then a restart is the bounded recovery and the Ready reply stays stored.
    client: reqwest::Client,
    csrf: String,
    scenario_actions: String,
    scenarios: HashMap<String, String>,
}

#[derive(Deserialize)]
struct AuthResponse {
    status: String,
    passport_host: String,
    track_id: String,
}

#[derive(Deserialize)]
struct StatusResponse {
    status: String,
}

#[derive(Deserialize)]
struct ScenarioList {
    status: String,
    #[serde(default)]
    scenarios: Vec<ScenarioSummary>,
}

#[derive(Deserialize)]
struct ScenarioSummary {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct ScenarioCreated {
    status: String,
    scenario_id: String,
}

impl YandexVoiceReturn {
    pub async fn connect(config: YandexVoiceReturnConfig) -> Result<Self, YandexVoiceReturnError> {
        Self::connect_with_endpoints(config, YandexEndpoints::default()).await
    }

    #[doc(hidden)]
    pub async fn connect_with_endpoints(
        config: YandexVoiceReturnConfig,
        endpoints: YandexEndpoints,
    ) -> Result<Self, YandexVoiceReturnError> {
        validate(&config)?;
        let client = reqwest::Client::builder()
            .cookie_store(true)
            // This optional integration must never stall bridge startup or a
            // completed reply indefinitely when Yandex is unavailable.
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| YandexVoiceReturnError::Request)?;

        let auth: AuthResponse = response_json(
            client
                .post(&endpoints.x_token_auth)
                .header(
                    "Ya-Consumer-Authorization",
                    format!("OAuth {}", config.x_token),
                )
                .form(&[("type", "x-token"), ("retpath", "https://www.yandex.ru")])
                .send()
                .await,
        )
        .await?;
        if auth.status != "ok" {
            return Err(YandexVoiceReturnError::Authentication);
        }

        let session_url = format!("{}/auth/session/", auth.passport_host.trim_end_matches('/'));
        client
            .get(session_url)
            .query(&[("track_id", auth.track_id)])
            .send()
            .await
            .map_err(|_| YandexVoiceReturnError::Authentication)?
            .error_for_status()
            .map_err(|_| YandexVoiceReturnError::Authentication)?;

        let quasar_page = client
            .get(&endpoints.quasar_page)
            .send()
            .await
            .map_err(|_| YandexVoiceReturnError::Authentication)?
            .error_for_status()
            .map_err(|_| YandexVoiceReturnError::Authentication)?
            .text()
            .await
            .map_err(|_| YandexVoiceReturnError::InvalidResponse)?;
        let csrf = extract_csrf(&quasar_page).ok_or(YandexVoiceReturnError::Authentication)?;

        let list: ScenarioList = response_json(
            client
                .get(&endpoints.scenario_list)
                .header("x-csrf-token", &csrf)
                .header(ORIGIN, "https://yandex.ru")
                .send()
                .await,
        )
        .await?;
        if list.status != "ok" {
            return Err(YandexVoiceReturnError::InvalidResponse);
        }

        let configured_names = config
            .targets
            .iter()
            .map(|target| target.scenario_name.clone())
            .collect();
        let existing = unique_scenarios(list.scenarios, &configured_names)?;
        let mut scenarios = HashMap::new();
        for target in config.targets {
            let scenario_id = if let Some(id) = existing.get(&target.scenario_name) {
                let updated: StatusResponse = response_json(
                    client
                        .put(format!(
                            "{}/{}",
                            endpoints.scenario_update.trim_end_matches('/'),
                            id
                        ))
                        .header("x-csrf-token", &csrf)
                        .header(ORIGIN, "https://yandex.ru")
                        .json(&scenario_payload(&target, &config.activation_name))
                        .send()
                        .await,
                )
                .await?;
                if updated.status != "ok" {
                    return Err(YandexVoiceReturnError::InvalidResponse);
                }
                id.clone()
            } else {
                let created: ScenarioCreated = response_json(
                    client
                        .post(&endpoints.scenario_create)
                        .header("x-csrf-token", &csrf)
                        .header(ORIGIN, "https://yandex.ru")
                        .json(&scenario_payload(&target, &config.activation_name))
                        .send()
                        .await,
                )
                .await?;
                if created.status != "ok" || created.scenario_id.is_empty() {
                    return Err(YandexVoiceReturnError::InvalidResponse);
                }
                created.scenario_id
            };
            scenarios.insert(target.application_id, scenario_id);
        }

        Ok(Self {
            client,
            csrf,
            scenario_actions: endpoints.scenario_actions,
            scenarios,
        })
    }
}

#[async_trait::async_trait]
impl VoiceReturn for YandexVoiceReturn {
    fn supports(&self, application_id: &str) -> bool {
        self.scenarios.contains_key(application_id)
    }

    async fn answer_ready(&self, application_id: &str) -> Result<(), String> {
        let scenario_id = self
            .scenarios
            .get(application_id)
            .ok_or(YandexVoiceReturnError::UnknownSurface)
            .map_err(|error| error.to_string())?;
        let url = format!(
            "{}/{}/actions",
            self.scenario_actions.trim_end_matches('/'),
            scenario_id
        );
        let response: StatusResponse = response_json(
            self.client
                .post(url)
                .header("x-csrf-token", &self.csrf)
                .header(ORIGIN, "https://yandex.ru")
                .json(&serde_json::json!({}))
                .send()
                .await,
        )
        .await
        .map_err(|error| error.to_string())?;
        if response.status != "ok" {
            return Err(YandexVoiceReturnError::InvalidResponse.to_string());
        }
        Ok(())
    }
}

fn validate(config: &YandexVoiceReturnConfig) -> Result<(), YandexVoiceReturnError> {
    if config.x_token.is_empty()
        || config.activation_name.trim().is_empty()
        || config.targets.is_empty()
    {
        return Err(YandexVoiceReturnError::InvalidConfig);
    }
    let mut applications = HashSet::new();
    let mut names = HashSet::new();
    for target in &config.targets {
        if target.application_id.is_empty()
            || target.device_id.is_empty()
            || target.scenario_name.trim().is_empty()
            || !applications.insert(&target.application_id)
            || !names.insert(&target.scenario_name)
        {
            return Err(YandexVoiceReturnError::InvalidConfig);
        }
    }
    Ok(())
}

fn unique_scenarios(
    scenarios: Vec<ScenarioSummary>,
    configured_names: &HashSet<String>,
) -> Result<HashMap<String, String>, YandexVoiceReturnError> {
    let mut unique = HashMap::new();
    for scenario in scenarios {
        if configured_names.contains(&scenario.name)
            && unique.insert(scenario.name, scenario.id).is_some()
        {
            return Err(YandexVoiceReturnError::InvalidResponse);
        }
    }
    Ok(unique)
}

fn extract_csrf(page: &str) -> Option<String> {
    page.split_once("\"csrfToken2\":\"")?
        .1
        .split_once('"')
        .map(|(value, _)| value.to_owned())
        .filter(|value| !value.is_empty())
}

fn scenario_payload(target: &YandexVoiceReturnTarget, activation_name: &str) -> serde_json::Value {
    serde_json::json!({
        "name": target.scenario_name,
        "icon": "home",
        "triggers": [{
            "trigger": {
                "type": "scenario.trigger.voice",
                "value": target.scenario_name.to_lowercase()
            }
        }],
        "steps": [{
            "type": "scenarios.steps.actions.v2",
            "parameters": {
                "items": [{
                    "id": target.device_id,
                    "type": "step.action.item.device",
                    "value": {
                        "id": target.device_id,
                        "item_type": "device",
                        "capabilities": [{
                            "type": "devices.capabilities.quasar.server_action",
                            "state": {
                                "instance": "text_action",
                                "value": format!("СКАЖИ НАВЫКУ {activation_name}")
                            }
                        }]
                    }
                }]
            }
        }]
    })
}

async fn response_json<T: for<'de> Deserialize<'de>>(
    response: Result<reqwest::Response, reqwest::Error>,
) -> Result<T, YandexVoiceReturnError> {
    response
        .map_err(|_| YandexVoiceReturnError::Request)?
        .error_for_status()
        .map_err(|_| YandexVoiceReturnError::Request)?
        .json()
        .await
        .map_err(|_| YandexVoiceReturnError::InvalidResponse)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{ScenarioSummary, YandexVoiceReturnError, unique_scenarios};

    #[test]
    fn duplicate_remote_scenario_names_fail_closed() {
        let scenarios = vec![
            ScenarioSummary {
                id: "first".to_owned(),
                name: "Соня GPT — гостиная".to_owned(),
            },
            ScenarioSummary {
                id: "second".to_owned(),
                name: "Соня GPT — гостиная".to_owned(),
            },
        ];

        assert!(matches!(
            unique_scenarios(
                scenarios,
                &HashSet::from(["Соня GPT — гостиная".to_owned()])
            ),
            Err(YandexVoiceReturnError::InvalidResponse)
        ));
    }
}
