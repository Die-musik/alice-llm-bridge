use std::{sync::Arc, time::Duration};

use dashmap::DashMap;

use crate::{
    HouseRuntime, HouseholdStore,
    house_prompt::build_house_instructions,
    household::{PendingReply, SurfaceIdentity, SurfaceResolution},
    reply::{ContinuationDecision, ReplyShaper},
};

const STILL_THINKING: &str = "Я ещё думаю. Спросите меня через несколько секунд.";

#[derive(Debug, Clone, Copy)]
pub struct HouseholdEngineConfig {
    pub reply_budget: Duration,
    pub chunk_limit: usize,
    pub homey_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HouseholdInput {
    pub identity: SurfaceIdentity,
    pub utterance: String,
    pub new_session: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HouseholdReply {
    Say(String),
    Refuse,
    Pairing(String),
    Busy,
    InternalError,
}

pub struct HouseholdEngine {
    store: Arc<dyn HouseholdStore>,
    runtime: Arc<dyn HouseRuntime>,
    shaper: ReplyShaper,
    // Single-replica ceiling: move this invariant to a Postgres advisory lock
    // before replica_count > 1.
    locks: DashMap<i64, Arc<tokio::sync::Mutex<()>>>,
    config: HouseholdEngineConfig,
}

impl HouseholdEngine {
    pub fn new(
        store: Arc<dyn HouseholdStore>,
        runtime: Arc<dyn HouseRuntime>,
        config: HouseholdEngineConfig,
    ) -> Self {
        Self {
            store,
            runtime,
            shaper: ReplyShaper::new(config.chunk_limit),
            locks: DashMap::new(),
            config,
        }
    }

    pub async fn respond(&self, input: HouseholdInput) -> HouseholdReply {
        let house = match self.store.resolve_surface(&input.identity).await {
            Ok(SurfaceResolution::Bound(house)) => house,
            Ok(SurfaceResolution::PairingRequired { spoken_code }) => {
                return HouseholdReply::Pairing(spoken_code);
            }
            Ok(SurfaceResolution::Unauthorized) => return HouseholdReply::Refuse,
            Err(_) => return HouseholdReply::InternalError,
        };
        let application_id = input.identity.application_id;

        match self
            .store
            .take_continuation(house.id, &application_id)
            .await
        {
            Ok(Some(chunks)) => {
                return self
                    .answer_continuation(house.id, &application_id, &input.utterance, chunks)
                    .await;
            }
            Ok(None) => {}
            Err(_) => return HouseholdReply::InternalError,
        }

        match self.store.poll_pending(house.id, &application_id).await {
            Ok(PendingReply::Ready(text)) => {
                let reply = self.shape_and_save(house.id, &application_id, &text).await;
                if !matches!(reply, HouseholdReply::InternalError)
                    && self
                        .store
                        .clear_pending(house.id, &application_id)
                        .await
                        .is_err()
                {
                    return HouseholdReply::InternalError;
                }
                return reply;
            }
            Ok(PendingReply::Thinking) => {
                return HouseholdReply::Say(STILL_THINKING.to_owned());
            }
            Ok(PendingReply::None) => {}
            Err(_) => return HouseholdReply::InternalError,
        }

        if input.new_session && input.utterance.trim().is_empty() {
            return HouseholdReply::Say("Здравствуйте. Чем помочь?".to_owned());
        }

        let lock = self
            .locks
            .entry(house.id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let Ok(guard) = lock.try_lock_owned() else {
            return HouseholdReply::Busy;
        };

        let store = self.store.clone();
        let runtime = self.runtime.clone();
        let work_house = house.clone();
        let utterance = input.utterance;
        let instructions = build_house_instructions(&house, self.config.homey_enabled);
        let mut work = tokio::spawn(async move {
            let _guard = guard;
            let thread_id = if let Some(thread_id) = work_house.codex_thread_id.clone() {
                thread_id
            } else {
                let thread_id = runtime
                    .start_thread(&work_house, &instructions)
                    .await
                    .map_err(|_| ())?;
                store
                    .save_thread_id(work_house.id, &thread_id)
                    .await
                    .map_err(|_| ())?;
                thread_id
            };
            runtime
                .turn(&work_house, &thread_id, &utterance)
                .await
                .map_err(|_| ())
        });

        match tokio::time::timeout(self.config.reply_budget, &mut work).await {
            Ok(Ok(Ok(text))) => self.shape_and_save(house.id, &application_id, &text).await,
            Ok(Ok(Err(()))) | Ok(Err(_)) => HouseholdReply::InternalError,
            Err(_) => {
                if self
                    .store
                    .mark_thinking(house.id, &application_id)
                    .await
                    .is_err()
                {
                    work.abort();
                    return HouseholdReply::InternalError;
                }
                let store = self.store.clone();
                let house_id = house.id;
                tokio::spawn(async move {
                    match work.await {
                        Ok(Ok(text)) => {
                            let _ = store.save_ready(house_id, &application_id, &text).await;
                        }
                        Ok(Err(())) | Err(_) => {
                            let _ = store.clear_pending(house_id, &application_id).await;
                        }
                    }
                });
                HouseholdReply::Say(STILL_THINKING.to_owned())
            }
        }
    }

    async fn answer_continuation(
        &self,
        house_id: i64,
        application_id: &str,
        utterance: &str,
        chunks: Vec<String>,
    ) -> HouseholdReply {
        match ContinuationDecision::from_utterance(utterance) {
            ContinuationDecision::Stop => HouseholdReply::Say("Хорошо.".to_owned()),
            ContinuationDecision::Empty => {
                if self
                    .store
                    .save_continuation(house_id, application_id, &chunks)
                    .await
                    .is_err()
                {
                    HouseholdReply::InternalError
                } else {
                    HouseholdReply::Say("Скажите, продолжать?".to_owned())
                }
            }
            ContinuationDecision::Continue => {
                self.shape_and_save(house_id, application_id, &chunks.concat())
                    .await
            }
        }
    }

    async fn shape_and_save(
        &self,
        house_id: i64,
        application_id: &str,
        text: &str,
    ) -> HouseholdReply {
        let shaped = self.shaper.split(text);
        if self
            .store
            .save_continuation(house_id, application_id, &shaped.remaining)
            .await
            .is_err()
        {
            HouseholdReply::InternalError
        } else {
            HouseholdReply::Say(shaped.spoken)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use super::{HouseholdEngine, HouseholdEngineConfig, HouseholdInput, HouseholdReply};
    use crate::{
        HouseholdStore, PendingReply, SurfaceIdentity,
        testing::{MemoryHouseholdStore, ScriptedHouseRuntime},
    };

    fn input(user_id: &str, application_id: &str, utterance: &str) -> HouseholdInput {
        HouseholdInput {
            identity: SurfaceIdentity::new(user_id, application_id),
            utterance: utterance.to_owned(),
            new_session: false,
        }
    }

    fn two_surface_store(thread_id: Option<&str>) -> Arc<MemoryHouseholdStore> {
        Arc::new(
            MemoryHouseholdStore::fixture()
                .house(1, "Дом мамы", thread_id, "homey-mother")
                .member(1, "OWNER")
                .member(1, "MOTHER")
                .surface(1, "OWNER", "owner-station")
                .surface(1, "MOTHER", "mother-station"),
        )
    }

    fn config(reply_budget: Duration) -> HouseholdEngineConfig {
        HouseholdEngineConfig {
            reply_budget,
            chunk_limit: 850,
            homey_enabled: false,
        }
    }

    #[tokio::test]
    async fn two_accounts_share_one_started_thread() {
        let store = two_surface_store(None);
        let runtime = ScriptedHouseRuntime::replying("Короткий ответ");
        let engine = HouseholdEngine::new(store, runtime.clone(), config(Duration::from_secs(1)));

        assert_eq!(
            engine
                .respond(input("OWNER", "owner-station", "Первый вопрос"))
                .await,
            HouseholdReply::Say("Короткий ответ".to_owned())
        );
        assert_eq!(
            engine
                .respond(input("MOTHER", "mother-station", "Второй вопрос"))
                .await,
            HouseholdReply::Say("Короткий ответ".to_owned())
        );

        assert_eq!(runtime.start_calls.lock().unwrap().len(), 1);
        let turns = runtime.turn_calls.lock().unwrap();
        assert_eq!(turns.len(), 2);
        assert!(turns.iter().all(|(thread_id, _)| thread_id == "thread-1"));
    }

    #[tokio::test]
    async fn continuation_consumes_any_non_refusal_without_new_turn() {
        let store = two_surface_store(Some("thread-1"));
        let runtime = ScriptedHouseRuntime::replying(&"А".repeat(1_000));
        let engine = HouseholdEngine::new(store, runtime.clone(), config(Duration::from_secs(1)));

        let HouseholdReply::Say(first) = engine
            .respond(input("OWNER", "owner-station", "Расскажи подробно"))
            .await
        else {
            panic!("expected first chunk")
        };
        assert!(first.ends_with(" Продолжать?"));

        let HouseholdReply::Say(second) = engine
            .respond(input("OWNER", "owner-station", "включи свет"))
            .await
        else {
            panic!("expected continuation")
        };
        assert!(!second.ends_with(" Продолжать?"));
        assert_eq!(runtime.turn_calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn explicit_stop_clears_continuation() {
        let store = two_surface_store(Some("thread-1"));
        let runtime = ScriptedHouseRuntime::replying(&"А".repeat(1_000));
        let engine = HouseholdEngine::new(
            store.clone(),
            runtime.clone(),
            config(Duration::from_secs(1)),
        );
        engine
            .respond(input("OWNER", "owner-station", "Расскажи подробно"))
            .await;

        assert_eq!(
            engine
                .respond(input("OWNER", "owner-station", "Не надо!"))
                .await,
            HouseholdReply::Say("Хорошо.".to_owned())
        );
        assert!(
            store
                .take_continuation(1, "owner-station")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(runtime.turn_calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn timed_out_answer_is_saved_only_for_initiating_surface() {
        let store = two_surface_store(Some("thread-1"));
        let runtime = ScriptedHouseRuntime::slow("Готовый ответ", Duration::from_millis(40));
        let engine = HouseholdEngine::new(store.clone(), runtime, config(Duration::from_millis(5)));

        assert_eq!(
            engine
                .respond(input("OWNER", "owner-station", "Сложный вопрос"))
                .await,
            HouseholdReply::Say("Я ещё думаю. Спросите меня через несколько секунд.".to_owned())
        );
        tokio::time::sleep(Duration::from_millis(60)).await;

        assert_eq!(
            store.poll_pending(1, "owner-station").await.unwrap(),
            PendingReply::Ready("Готовый ответ".to_owned())
        );
        assert_eq!(
            store.poll_pending(1, "mother-station").await.unwrap(),
            PendingReply::None
        );
    }

    #[tokio::test]
    async fn concurrent_surface_gets_busy_instead_of_starting_second_turn() {
        let store = two_surface_store(Some("thread-1"));
        let runtime = ScriptedHouseRuntime::slow("Ответ", Duration::from_millis(50));
        let engine = Arc::new(HouseholdEngine::new(
            store,
            runtime.clone(),
            config(Duration::from_secs(1)),
        ));

        let first_engine = engine.clone();
        let first = tokio::spawn(async move {
            first_engine
                .respond(input("OWNER", "owner-station", "Первый"))
                .await
        });
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(
            engine
                .respond(input("MOTHER", "mother-station", "Второй"))
                .await,
            HouseholdReply::Busy
        );
        assert!(matches!(first.await.unwrap(), HouseholdReply::Say(_)));
        assert_eq!(runtime.turn_calls.lock().unwrap().len(), 1);
    }
}
