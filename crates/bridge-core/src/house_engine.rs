use std::{sync::Arc, time::Duration};

use dashmap::DashMap;

use crate::{
    DisabledVoiceReturn, HouseRuntime, HouseholdStore, VoiceReturn,
    house_prompt::build_house_instructions,
    household::{PendingReply, SurfaceIdentity, SurfaceResolution},
    reply::{ContinuationDecision, ReplyShaper},
};

const STILL_THINKING: &str = "Я ещё думаю. Скажите «готово», и я отвечу.";

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
    Deferred,
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
    voice_return: Arc<dyn VoiceReturn>,
}

impl HouseholdEngine {
    pub fn new(
        store: Arc<dyn HouseholdStore>,
        runtime: Arc<dyn HouseRuntime>,
        config: HouseholdEngineConfig,
    ) -> Self {
        Self::with_voice_return(store, runtime, config, Arc::new(DisabledVoiceReturn))
    }

    pub fn with_voice_return(
        store: Arc<dyn HouseholdStore>,
        runtime: Arc<dyn HouseRuntime>,
        config: HouseholdEngineConfig,
        voice_return: Arc<dyn VoiceReturn>,
    ) -> Self {
        Self {
            store,
            runtime,
            shaper: ReplyShaper::new(config.chunk_limit),
            locks: DashMap::new(),
            config,
            voice_return,
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
        let lock = self
            .locks
            .entry(house.id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let Ok(guard) = lock.try_lock_owned() else {
            return HouseholdReply::Busy;
        };

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

        match self.store.take_pending(house.id, &application_id).await {
            Ok(PendingReply::Ready(text)) => {
                let reply = self.shape_and_save(house.id, &application_id, &text).await;
                if matches!(reply, HouseholdReply::InternalError)
                    && self
                        .store
                        .save_ready(house.id, &application_id, &text)
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

        let store = self.store.clone();
        let runtime = self.runtime.clone();
        let work_house = house.clone();
        let utterance = input.utterance;
        let instructions = build_house_instructions(&house, self.config.homey_enabled);
        let mut work = tokio::spawn(async move {
            let _guard = guard;
            let house_id = work_house.id;
            if let Some(thread_id) = work_house.codex_thread_id.clone() {
                runtime
                    .turn(&work_house, &thread_id, &utterance)
                    .await
                    .map_err(|error| {
                        tracing::warn!(house_id, error = %error, "household runtime turn failed");
                    })
            } else {
                let (thread_id, answer) = runtime
                    .start_thread_and_turn(&work_house, &instructions, &utterance)
                    .await
                    .map_err(|error| {
                        tracing::warn!(house_id, error = %error, "household runtime first turn failed");
                    })?;
                store
                    .save_thread_id(house_id, &thread_id)
                    .await
                    .map_err(|error| {
                        tracing::warn!(house_id, error = %error, "household thread persistence failed");
                    })?;
                Ok(answer)
            }
        });

        match tokio::time::timeout(self.config.reply_budget, &mut work).await {
            Ok(Ok(Ok(text))) => self.shape_and_save(house.id, &application_id, &text).await,
            Ok(Ok(Err(()))) | Ok(Err(_)) => HouseholdReply::InternalError,
            Err(_) => {
                let automatic_return = self.voice_return.supports(&application_id);
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
                let voice_return = self.voice_return.clone();
                let house_id = house.id;
                tokio::spawn(async move {
                    match work.await {
                        Ok(Ok(text)) => {
                            if store
                                .save_ready(house_id, &application_id, &text)
                                .await
                                .is_ok()
                                && automatic_return
                                && voice_return.answer_ready(&application_id).await.is_err()
                            {
                                tracing::warn!(house_id, "automatic Alice voice return failed");
                            }
                        }
                        Ok(Err(())) | Err(_) => {
                            let _ = store.clear_pending(house_id, &application_id).await;
                        }
                    }
                });
                if automatic_return {
                    HouseholdReply::Deferred
                } else {
                    HouseholdReply::Say(STILL_THINKING.to_owned())
                }
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
        HouseholdStore, PendingReply, SurfaceIdentity, SurfaceResolution, VoiceReturn,
        testing::{MemoryHouseholdStore, ScriptedHouseRuntime},
    };

    #[derive(Default)]
    struct RecordingVoiceReturn {
        calls: std::sync::Mutex<Vec<String>>,
        store: Option<Arc<MemoryHouseholdStore>>,
        ready_at_call: std::sync::atomic::AtomicBool,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl VoiceReturn for RecordingVoiceReturn {
        fn supports(&self, application_id: &str) -> bool {
            application_id == "owner-station"
        }

        async fn answer_ready(&self, application_id: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(application_id.to_owned());
            if let Some(store) = &self.store
                && let PendingReply::Ready(text) = store
                    .take_pending(1, application_id)
                    .await
                    .map_err(|error| error.to_string())?
            {
                self.ready_at_call
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                store
                    .save_ready(1, application_id, &text)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            if self.fail {
                Err("scripted trigger failure".to_owned())
            } else {
                Ok(())
            }
        }
    }

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
        let engine = HouseholdEngine::new(
            store.clone(),
            runtime.clone(),
            config(Duration::from_secs(1)),
        );

        assert_eq!(
            engine
                .respond(input("OWNER", "owner-station", "Первый вопрос"))
                .await,
            HouseholdReply::Say("Короткий ответ".to_owned())
        );
        let SurfaceResolution::Bound(house) = store
            .resolve_surface(&SurfaceIdentity::new("OWNER", "owner-station"))
            .await
            .unwrap()
        else {
            panic!("expected bound house")
        };
        assert_eq!(house.codex_thread_id.as_deref(), Some("thread-1"));
        assert_eq!(
            engine
                .respond(input("MOTHER", "mother-station", "Второй вопрос"))
                .await,
            HouseholdReply::Say("Короткий ответ".to_owned())
        );

        assert_eq!(runtime.start_calls.lock().unwrap().len(), 1);
        let turns = runtime.turn_calls.lock().unwrap();
        assert_eq!(turns.len(), 1);
        assert!(turns.iter().all(|(thread_id, _)| thread_id == "thread-1"));
    }

    #[tokio::test]
    async fn failed_first_turn_does_not_persist_thread_id() {
        let store = two_surface_store(None);
        let runtime = ScriptedHouseRuntime::failing("first turn failed");
        let engine = HouseholdEngine::new(
            store.clone(),
            runtime.clone(),
            config(Duration::from_secs(1)),
        );

        assert_eq!(
            engine
                .respond(input("OWNER", "owner-station", "Первый вопрос"))
                .await,
            HouseholdReply::InternalError
        );

        let SurfaceResolution::Bound(house) = store
            .resolve_surface(&SurfaceIdentity::new("OWNER", "owner-station"))
            .await
            .unwrap()
        else {
            panic!("expected bound house")
        };
        assert_eq!(house.codex_thread_id, None);
        assert_eq!(runtime.start_calls.lock().unwrap().len(), 1);
        assert!(runtime.turn_calls.lock().unwrap().is_empty());
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
            HouseholdReply::Say("Я ещё думаю. Скажите «готово», и я отвечу.".to_owned())
        );
        tokio::time::sleep(Duration::from_millis(60)).await;

        assert_eq!(
            store.take_pending(1, "owner-station").await.unwrap(),
            PendingReply::Ready("Готовый ответ".to_owned())
        );
        assert_eq!(
            store.take_pending(1, "mother-station").await.unwrap(),
            PendingReply::None
        );
    }

    #[tokio::test]
    async fn supported_surface_closes_then_is_reinvoked_after_answer_is_ready() {
        let store = two_surface_store(Some("thread-1"));
        let runtime = ScriptedHouseRuntime::slow("Готовый ответ", Duration::from_millis(40));
        let voice_return = Arc::new(RecordingVoiceReturn {
            store: Some(store.clone()),
            ..RecordingVoiceReturn::default()
        });
        let engine = HouseholdEngine::with_voice_return(
            store.clone(),
            runtime.clone(),
            config(Duration::from_millis(5)),
            voice_return.clone(),
        );

        assert_eq!(
            engine
                .respond(input("OWNER", "owner-station", "Сложный вопрос"))
                .await,
            HouseholdReply::Deferred
        );
        tokio::time::sleep(Duration::from_millis(60)).await;

        assert_eq!(
            engine.respond(input("OWNER", "owner-station", "")).await,
            HouseholdReply::Say("Готовый ответ".to_owned())
        );
        assert!(
            voice_return
                .ready_at_call
                .load(std::sync::atomic::Ordering::SeqCst)
        );
        assert_eq!(
            voice_return.calls.lock().unwrap().as_slice(),
            ["owner-station"]
        );
        assert_eq!(runtime.turn_calls.lock().unwrap().len(), 1);
        assert_eq!(
            store.take_pending(1, "owner-station").await.unwrap(),
            PendingReply::None
        );
    }

    #[tokio::test]
    async fn failed_automatic_return_keeps_ready_reply_for_manual_recovery() {
        let store = two_surface_store(Some("thread-1"));
        let runtime = ScriptedHouseRuntime::slow("Готовый ответ", Duration::from_millis(40));
        let voice_return = Arc::new(RecordingVoiceReturn {
            store: Some(store.clone()),
            fail: true,
            ..RecordingVoiceReturn::default()
        });
        let engine = HouseholdEngine::with_voice_return(
            store.clone(),
            runtime.clone(),
            config(Duration::from_millis(5)),
            voice_return,
        );

        assert_eq!(
            engine
                .respond(input("OWNER", "owner-station", "Сложный вопрос"))
                .await,
            HouseholdReply::Deferred
        );
        tokio::time::sleep(Duration::from_millis(60)).await;

        assert_eq!(
            engine
                .respond(input("OWNER", "owner-station", "готово"))
                .await,
            HouseholdReply::Say("Готовый ответ".to_owned())
        );
        assert_eq!(runtime.turn_calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn concurrent_ready_consumers_do_not_start_an_extra_codex_turn() {
        let store = two_surface_store(Some("thread-1"));
        store
            .save_ready(1, "owner-station", "Готовый ответ")
            .await
            .unwrap();
        let runtime = ScriptedHouseRuntime::replying("Лишний ответ");
        let engine =
            HouseholdEngine::new(store, runtime.clone(), config(Duration::from_millis(50)));

        let (first, second) = tokio::join!(
            engine.respond(input("OWNER", "owner-station", "готово")),
            engine.respond(input("OWNER", "owner-station", "готово"))
        );
        let replies = [first, second];

        assert_eq!(
            replies
                .iter()
                .filter(|reply| **reply == HouseholdReply::Say("Готовый ответ".to_owned()))
                .count(),
            1
        );
        assert_eq!(
            replies
                .iter()
                .filter(|reply| **reply == HouseholdReply::Busy)
                .count(),
            1
        );
        assert!(runtime.turn_calls.lock().unwrap().is_empty());
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
