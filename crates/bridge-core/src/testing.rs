//! Test support: in-memory fakes shared by unit and integration tests.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use llm_providers::{ChatCompletion, ChatProvider, ChatRequest, ProviderError, TokenUsage};

use crate::store::{
    ConversationStore, ExchangeRecord, MessageRole, StoreError, StoredMessage, Summary, UsageStats,
};
use crate::{
    house_store::{HouseholdStore, HouseholdStoreError},
    household::{HouseContext, PendingReply, SurfaceIdentity, SurfaceResolution},
};

#[derive(Debug, Default)]
pub struct MemoryHouseholdStore {
    household: Mutex<HouseholdInner>,
}

#[derive(Debug, Default)]
struct HouseholdInner {
    houses: HashMap<i64, HouseContext>,
    members: HashSet<(i64, String)>,
    surfaces: HashMap<String, (i64, String)>,
    pending: HashMap<(i64, String), PendingReply>,
    continuations: HashMap<(i64, String), Vec<String>>,
}

impl MemoryHouseholdStore {
    pub fn fixture() -> Self {
        Self::default()
    }

    pub fn house(
        mut self,
        id: i64,
        name: &str,
        codex_thread_id: Option<&str>,
        homey_connector_id: &str,
    ) -> Self {
        self.household
            .get_mut()
            .expect("memory store poisoned")
            .houses
            .insert(
                id,
                HouseContext {
                    id,
                    name: name.to_owned(),
                    codex_thread_id: codex_thread_id.map(str::to_owned),
                    homey_connector_id: homey_connector_id.to_owned(),
                },
            );
        self
    }

    pub fn member(mut self, house_id: i64, user_id: &str) -> Self {
        assert!(
            self.household
                .get_mut()
                .expect("memory store poisoned")
                .houses
                .contains_key(&house_id),
            "member house must exist"
        );
        self.household
            .get_mut()
            .expect("memory store poisoned")
            .members
            .insert((house_id, user_id.to_owned()));
        self
    }

    pub fn surface(mut self, house_id: i64, user_id: &str, application_id: &str) -> Self {
        let inner = self.household.get_mut().expect("memory store poisoned");
        assert!(
            inner.members.contains(&(house_id, user_id.to_owned())),
            "surface member must exist"
        );
        let previous = inner
            .surfaces
            .insert(application_id.to_owned(), (house_id, user_id.to_owned()));
        assert!(previous.is_none(), "application already belongs to a house");
        self
    }
}

#[async_trait::async_trait]
impl HouseholdStore for MemoryHouseholdStore {
    async fn resolve_surface(
        &self,
        identity: &SurfaceIdentity,
    ) -> Result<SurfaceResolution, HouseholdStoreError> {
        let inner = self.household.lock().expect("memory store poisoned");
        if let Some((house_id, user_id)) = inner.surfaces.get(&identity.application_id) {
            if user_id == &identity.user_id && inner.members.contains(&(*house_id, user_id.clone()))
            {
                return inner
                    .houses
                    .get(house_id)
                    .cloned()
                    .map(SurfaceResolution::Bound)
                    .ok_or_else(|| HouseholdStoreError("surface house is missing".to_owned()));
            }
            return Ok(SurfaceResolution::Unauthorized);
        }

        let mut matching_houses = inner
            .members
            .iter()
            .filter(|(_, user_id)| user_id == &identity.user_id)
            .map(|(house_id, _)| *house_id);
        let Some(_) = matching_houses.next() else {
            return Ok(SurfaceResolution::Unauthorized);
        };
        if matching_houses.next().is_some() {
            return Ok(SurfaceResolution::Unauthorized);
        }

        Ok(SurfaceResolution::PairingRequired {
            spoken_code: "123456".to_owned(),
        })
    }

    async fn save_thread_id(
        &self,
        house_id: i64,
        thread_id: &str,
    ) -> Result<(), HouseholdStoreError> {
        let mut inner = self.household.lock().expect("memory store poisoned");
        let house = inner
            .houses
            .get_mut(&house_id)
            .ok_or_else(|| HouseholdStoreError("house is missing".to_owned()))?;
        if house
            .codex_thread_id
            .as_deref()
            .is_some_and(|id| id != thread_id)
        {
            return Err(HouseholdStoreError(
                "house already has a different thread".to_owned(),
            ));
        }
        house.codex_thread_id = Some(thread_id.to_owned());
        Ok(())
    }

    async fn poll_pending(
        &self,
        house_id: i64,
        application_id: &str,
    ) -> Result<PendingReply, HouseholdStoreError> {
        Ok(self
            .household
            .lock()
            .expect("memory store poisoned")
            .pending
            .get(&(house_id, application_id.to_owned()))
            .cloned()
            .unwrap_or(PendingReply::None))
    }

    async fn mark_thinking(
        &self,
        house_id: i64,
        application_id: &str,
    ) -> Result<(), HouseholdStoreError> {
        self.household
            .lock()
            .expect("memory store poisoned")
            .pending
            .insert(
                (house_id, application_id.to_owned()),
                PendingReply::Thinking,
            );
        Ok(())
    }

    async fn save_ready(
        &self,
        house_id: i64,
        application_id: &str,
        text: &str,
    ) -> Result<(), HouseholdStoreError> {
        self.household
            .lock()
            .expect("memory store poisoned")
            .pending
            .insert(
                (house_id, application_id.to_owned()),
                PendingReply::Ready(text.to_owned()),
            );
        Ok(())
    }

    async fn clear_pending(
        &self,
        house_id: i64,
        application_id: &str,
    ) -> Result<(), HouseholdStoreError> {
        self.household
            .lock()
            .expect("memory store poisoned")
            .pending
            .remove(&(house_id, application_id.to_owned()));
        Ok(())
    }

    async fn take_continuation(
        &self,
        house_id: i64,
        application_id: &str,
    ) -> Result<Option<Vec<String>>, HouseholdStoreError> {
        Ok(self
            .household
            .lock()
            .expect("memory store poisoned")
            .continuations
            .remove(&(house_id, application_id.to_owned())))
    }

    async fn save_continuation(
        &self,
        house_id: i64,
        application_id: &str,
        chunks: &[String],
    ) -> Result<(), HouseholdStoreError> {
        let key = (house_id, application_id.to_owned());
        let mut inner = self.household.lock().expect("memory store poisoned");
        if chunks.is_empty() {
            inner.continuations.remove(&key);
        } else {
            inner.continuations.insert(key, chunks.to_vec());
        }
        Ok(())
    }

    async fn clear_continuation(
        &self,
        house_id: i64,
        application_id: &str,
    ) -> Result<(), HouseholdStoreError> {
        self.household
            .lock()
            .expect("memory store poisoned")
            .continuations
            .remove(&(house_id, application_id.to_owned()));
        Ok(())
    }
}

/// [`ChatProvider`] fake with a scripted reply, an optional delay and a log
/// of every request it received.
pub struct ScriptedProvider {
    pub delay: Duration,
    /// `Ok(reply)` or `Err(kind)` where kind is one of
    /// `"timeout" | "rate" | "auth"`; anything else maps to an API error.
    pub script: std::result::Result<String, String>,
    pub calls: Mutex<Vec<ChatRequest>>,
}

impl ScriptedProvider {
    pub fn replying(text: &str) -> Arc<Self> {
        Arc::new(Self {
            delay: Duration::ZERO,
            script: Ok(text.to_string()),
            calls: Mutex::new(Vec::new()),
        })
    }

    pub fn slow(text: &str, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            delay,
            script: Ok(text.to_string()),
            calls: Mutex::new(Vec::new()),
        })
    }

    pub fn failing(kind: &str) -> Arc<Self> {
        Arc::new(Self {
            delay: Duration::ZERO,
            script: Err(kind.to_string()),
            calls: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait::async_trait]
impl ChatProvider for ScriptedProvider {
    async fn chat(&self, request: &ChatRequest) -> llm_providers::Result<ChatCompletion> {
        self.calls
            .lock()
            .expect("scripted provider mutex poisoned")
            .push(request.clone());
        tokio::time::sleep(self.delay).await;
        match &self.script {
            Ok(text) => Ok(ChatCompletion {
                text: text.clone(),
                usage: TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 20,
                },
            }),
            Err(kind) => Err(match kind.as_str() {
                "timeout" => ProviderError::Timeout,
                "rate" => ProviderError::RateLimited,
                "auth" => ProviderError::Auth,
                other => ProviderError::Api {
                    status: 500,
                    message: other.to_string(),
                },
            }),
        }
    }
}

#[derive(Debug, Clone)]
struct Row {
    id: i64,
    profile: String,
    role: MessageRole,
    content: String,
    cost_micros: i64,
    created_at: DateTime<Utc>,
}

/// In-memory [`ConversationStore`] with the same semantics as the
/// production Postgres implementation.
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    rows: Vec<Row>,
    summaries: HashMap<String, Summary>,
    next_id: i64,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl ConversationStore for MemoryStore {
    async fn record_exchange(&self, exchange: &ExchangeRecord) -> Result<(), StoreError> {
        let mut inner = self.inner.lock().expect("memory store mutex poisoned");
        let now = Utc::now();
        for (role, content, cost) in [
            (MessageRole::User, &exchange.user_text, 0),
            (
                MessageRole::Assistant,
                &exchange.assistant_text,
                exchange.cost_micros,
            ),
        ] {
            inner.next_id += 1;
            let id = inner.next_id;
            inner.rows.push(Row {
                id,
                profile: exchange.profile.clone(),
                role,
                content: content.clone(),
                cost_micros: cost,
                created_at: now,
            });
        }
        Ok(())
    }

    async fn recent_messages(
        &self,
        profile: &str,
        limit: usize,
    ) -> Result<Vec<StoredMessage>, StoreError> {
        let inner = self.inner.lock().expect("memory store mutex poisoned");
        let mut messages: Vec<StoredMessage> = inner
            .rows
            .iter()
            .filter(|r| r.profile == profile)
            .rev()
            .take(limit)
            .map(|r| StoredMessage {
                role: r.role,
                content: r.content.clone(),
            })
            .collect();
        messages.reverse();
        Ok(messages)
    }

    async fn summary(&self, profile: &str) -> Result<Option<Summary>, StoreError> {
        Ok(self
            .inner
            .lock()
            .expect("memory store mutex poisoned")
            .summaries
            .get(profile)
            .cloned())
    }

    async fn upsert_summary(&self, profile: &str, summary: &Summary) -> Result<(), StoreError> {
        self.inner
            .lock()
            .expect("memory store mutex poisoned")
            .summaries
            .insert(profile.to_string(), summary.clone());
        Ok(())
    }

    async fn unsummarized(
        &self,
        profile: &str,
        keep_last: usize,
    ) -> Result<Vec<(i64, StoredMessage)>, StoreError> {
        let inner = self.inner.lock().expect("memory store mutex poisoned");
        let covers_until = inner
            .summaries
            .get(profile)
            .map(|s| s.covers_until_message_id)
            .unwrap_or(0);
        let mut rows: Vec<(i64, StoredMessage)> = inner
            .rows
            .iter()
            .filter(|r| r.profile == profile && r.id > covers_until)
            .map(|r| {
                (
                    r.id,
                    StoredMessage {
                        role: r.role,
                        content: r.content.clone(),
                    },
                )
            })
            .collect();
        rows.truncate(rows.len().saturating_sub(keep_last));
        Ok(rows)
    }

    async fn clear_profile(&self, profile: &str) -> Result<(), StoreError> {
        let mut inner = self.inner.lock().expect("memory store mutex poisoned");
        inner.rows.retain(|r| r.profile != profile);
        inner.summaries.remove(profile);
        Ok(())
    }

    async fn usage_since(&self, since: DateTime<Utc>) -> Result<UsageStats, StoreError> {
        let inner = self.inner.lock().expect("memory store mutex poisoned");
        let mut stats = UsageStats::default();
        for row in inner.rows.iter().filter(|r| r.created_at >= since) {
            if row.role == MessageRole::Assistant {
                stats.requests += 1;
            }
            stats.cost_micros += row.cost_micros;
        }
        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ExchangeRecord, MessageRole, Summary};

    fn exchange(profile: &str, n: u32) -> ExchangeRecord {
        ExchangeRecord {
            profile: profile.to_string(),
            user_text: format!("вопрос {n}"),
            assistant_text: format!("ответ {n}"),
            model: "test-model".to_string(),
            prompt_tokens: 10,
            completion_tokens: 20,
            cost_micros: 100,
        }
    }

    #[tokio::test]
    async fn records_and_reads_recent_messages() {
        let store = MemoryStore::new();
        for n in 1..=3 {
            store.record_exchange(&exchange("Дима", n)).await.unwrap();
        }
        let recent = store.recent_messages("Дима", 4).await.unwrap();
        assert_eq!(recent.len(), 4);
        assert_eq!(recent[0].role, MessageRole::User);
        assert_eq!(recent[0].content, "вопрос 2");
        assert_eq!(recent[3].content, "ответ 3");
        assert!(store.recent_messages("Маша", 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn summary_roundtrip_and_unsummarized() {
        let store = MemoryStore::new();
        for n in 1..=4 {
            store.record_exchange(&exchange("Дима", n)).await.unwrap();
        }
        let pending = store.unsummarized("Дима", 4).await.unwrap();
        assert_eq!(pending.len(), 4);
        assert_eq!(pending[0].1.content, "вопрос 1");

        let last_id = pending.last().unwrap().0;
        store
            .upsert_summary(
                "Дима",
                &Summary {
                    content: "резюме".to_string(),
                    covers_until_message_id: last_id,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            store.summary("Дима").await.unwrap().unwrap().content,
            "резюме"
        );
        assert!(store.unsummarized("Дима", 4).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn usage_counts_assistant_replies() {
        let store = MemoryStore::new();
        store.record_exchange(&exchange("Дима", 1)).await.unwrap();
        store.record_exchange(&exchange("Маша", 2)).await.unwrap();
        let stats = store
            .usage_since(chrono::Utc::now() - chrono::Duration::hours(1))
            .await
            .unwrap();
        assert_eq!(stats.requests, 2);
        assert_eq!(stats.cost_micros, 200);
        let none = store
            .usage_since(chrono::Utc::now() + chrono::Duration::hours(1))
            .await
            .unwrap();
        assert_eq!(none.requests, 0);
    }

    #[tokio::test]
    async fn clear_profile_removes_history_and_summary() {
        let store = MemoryStore::new();
        store.record_exchange(&exchange("Дима", 1)).await.unwrap();
        store
            .upsert_summary(
                "Дима",
                &Summary {
                    content: "s".to_string(),
                    covers_until_message_id: 1,
                },
            )
            .await
            .unwrap();
        store.clear_profile("Дима").await.unwrap();
        assert!(store.recent_messages("Дима", 10).await.unwrap().is_empty());
        assert!(store.summary("Дима").await.unwrap().is_none());
    }
}
