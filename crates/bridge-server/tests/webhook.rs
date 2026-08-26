use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bridge_core::testing::{
    MemoryHouseholdStore, MemoryStore, ScriptedHouseRuntime, ScriptedProvider,
};
use bridge_core::{
    Engine, EngineConfig, FamilyRoster, HouseholdEngine, HouseholdEngineConfig, ModelPreset,
    ModelRegistry, Profile, ProfileRole, phrases,
};
use bridge_server::routes::{AppState, GREETING, SkillBackend, router};
use tower::util::ServiceExt;

fn engine(provider: Arc<ScriptedProvider>) -> Engine {
    let preset = ModelPreset {
        provider,
        model: "test-model".to_string(),
        max_tokens: 300,
        temperature: 0.7,
        input_price_per_mtok: 1.0,
        output_price_per_mtok: 2.0,
    };
    let roster = FamilyRoster::new(
        vec![Profile {
            name: "Дима".to_string(),
            aliases: vec!["дима".to_string()],
            birthday: None,
            role: ProfileRole::Adult,
            persona: String::new(),
        }],
        "Дима",
    )
    .unwrap();
    Engine::new(
        roster,
        Vec::new(),
        ModelRegistry {
            fast: preset.clone(),
            smart: preset,
        },
        Arc::new(MemoryStore::new()),
        EngineConfig {
            context_window: 4,
            reply_budget: Duration::from_millis(200),
            utc_offset_hours: 3,
        },
    )
}

fn state(provider: Arc<ScriptedProvider>) -> AppState {
    AppState {
        backend: SkillBackend::Legacy(engine(provider)),
        webhook_secret: "s3cret".to_string(),
        allowed_user_ids: HashSet::from(["ALLOWED".to_string()]),
    }
}

fn household_state(
    store: Arc<MemoryHouseholdStore>,
    runtime: Arc<ScriptedHouseRuntime>,
) -> AppState {
    AppState {
        backend: SkillBackend::Household(Arc::new(HouseholdEngine::new(
            store,
            runtime,
            HouseholdEngineConfig {
                reply_budget: Duration::from_millis(200),
                chunk_limit: 850,
            },
        ))),
        webhook_secret: "s3cret".to_owned(),
        allowed_user_ids: HashSet::new(),
    }
}

fn alice_request(user_id: &str, utterance: &str, new_session: bool) -> serde_json::Value {
    alice_request_for(user_id, "app", utterance, new_session)
}

fn alice_request_for(
    user_id: &str,
    application_id: &str,
    utterance: &str,
    new_session: bool,
) -> serde_json::Value {
    serde_json::json!({
        "meta": { "locale": "ru-RU", "timezone": "Europe/Moscow" },
        "session": {
            "message_id": 1,
            "session_id": "sess",
            "skill_id": "skill",
            "user": { "user_id": user_id },
            "application": { "application_id": application_id },
            "new": new_session
        },
        "request": {
            "command": utterance.to_lowercase(),
            "original_utterance": utterance,
            "type": "SimpleUtterance"
        },
        "version": "1.0"
    })
}

async fn post(
    app: axum::Router,
    secret: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/alice/webhook/{secret}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

#[tokio::test]
async fn wrong_secret_is_not_found() {
    let app = router(state(ScriptedProvider::replying("ок")));
    let (status, _) = post(app, "wrong", alice_request("ALLOWED", "привет", false)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unknown_user_is_refused() {
    let app = router(state(ScriptedProvider::replying("ок")));
    let (status, body) = post(app, "s3cret", alice_request("STRANGER", "привет", false)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["response"]["end_session"], serde_json::json!(true));
}

#[tokio::test]
async fn new_session_gets_greeting() {
    let app = router(state(ScriptedProvider::replying("ок")));
    let (_, body) = post(app, "s3cret", alice_request("ALLOWED", "", true)).await;
    assert_eq!(body["response"]["text"], serde_json::json!(GREETING));
    assert_eq!(body["response"]["end_session"], serde_json::json!(false));
}

#[tokio::test]
async fn question_is_answered() {
    let app = router(state(ScriptedProvider::replying("Марс — планета.")));
    let (_, body) = post(
        app,
        "s3cret",
        alice_request("ALLOWED", "Что такое Марс?", false),
    )
    .await;
    assert_eq!(
        body["response"]["text"],
        serde_json::json!("Марс — планета.")
    );
    assert_eq!(body["version"], serde_json::json!("1.0"));
}

#[tokio::test(start_paused = true)]
async fn slow_answer_is_deferred_across_requests() {
    let st = state(ScriptedProvider::slow(
        "готовый ответ",
        Duration::from_secs(5),
    ));

    let (_, first) = post(
        router(st.clone()),
        "s3cret",
        alice_request("ALLOWED", "сложный вопрос", false),
    )
    .await;
    assert_eq!(
        first["response"]["text"],
        serde_json::json!(phrases::PHRASE_THINKING_STARTED)
    );

    tokio::time::sleep(Duration::from_secs(6)).await;

    let (_, second) = post(
        router(st),
        "s3cret",
        alice_request("ALLOWED", "ну что", false),
    )
    .await;
    assert_eq!(
        second["response"]["text"],
        serde_json::json!("готовый ответ")
    );
}

#[tokio::test]
async fn health_endpoint_responds() {
    let app = router(state(ScriptedProvider::replying("ок")));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn household_stranger_is_refused_without_reaching_runtime() {
    let store = Arc::new(
        MemoryHouseholdStore::fixture()
            .house(1, "Секретный дом", Some("thread-1"), "homey-1")
            .member(1, "OWNER")
            .surface(1, "OWNER", "owner-station"),
    );
    let runtime = ScriptedHouseRuntime::replying("не должен вызываться");
    let app = router(household_state(store, runtime.clone()));

    let (_, body) = post(
        app,
        "s3cret",
        alice_request_for("STRANGER", "stranger-station", "Привет", false),
    )
    .await;

    assert_eq!(body["response"]["end_session"], serde_json::json!(true));
    assert!(runtime.turn_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn household_unbound_member_gets_pairing_without_house_name() {
    let store = Arc::new(
        MemoryHouseholdStore::fixture()
            .house(1, "Секретный дом", None, "homey-1")
            .member(1, "MOTHER"),
    );
    let app = router(household_state(
        store,
        ScriptedHouseRuntime::replying("не должен вызываться"),
    ));

    let (_, body) = post(
        app,
        "s3cret",
        alice_request_for("MOTHER", "new-station", "Привет", false),
    )
    .await;
    let text = body["response"]["text"].as_str().unwrap();
    assert!(text.contains("123456"));
    assert!(!text.contains("Секретный дом"));
}

#[tokio::test]
async fn household_two_accounts_and_surfaces_share_one_thread() {
    let store = Arc::new(
        MemoryHouseholdStore::fixture()
            .house(1, "Дом", None, "homey-1")
            .member(1, "OWNER")
            .member(1, "MOTHER")
            .surface(1, "OWNER", "owner-station")
            .surface(1, "MOTHER", "mother-station"),
    );
    let runtime = ScriptedHouseRuntime::replying("Ответ");
    let state = household_state(store, runtime.clone());

    for (user, application) in [("OWNER", "owner-station"), ("MOTHER", "mother-station")] {
        let (_, body) = post(
            router(state.clone()),
            "s3cret",
            alice_request_for(user, application, "Вопрос", false),
        )
        .await;
        assert_eq!(body["response"]["text"], serde_json::json!("Ответ"));
    }

    assert_eq!(runtime.start_calls.lock().unwrap().len(), 1);
    assert!(
        runtime
            .turn_calls
            .lock()
            .unwrap()
            .iter()
            .all(|(thread_id, _)| thread_id == "thread-1")
    );
}

#[tokio::test]
async fn household_two_houses_keep_threads_separate() {
    let store = Arc::new(
        MemoryHouseholdStore::fixture()
            .house(1, "Первый", Some("thread-1"), "homey-1")
            .house(2, "Второй", Some("thread-2"), "homey-2")
            .member(1, "FIRST")
            .member(2, "SECOND")
            .surface(1, "FIRST", "first-station")
            .surface(2, "SECOND", "second-station"),
    );
    let runtime = ScriptedHouseRuntime::replying("Ответ");
    let state = household_state(store, runtime.clone());

    for (user, application) in [("FIRST", "first-station"), ("SECOND", "second-station")] {
        post(
            router(state.clone()),
            "s3cret",
            alice_request_for(user, application, "Вопрос", false),
        )
        .await;
    }
    let calls = runtime.turn_calls.lock().unwrap();
    assert_eq!(calls[0].0, "thread-1");
    assert_eq!(calls[1].0, "thread-2");
}

#[tokio::test]
async fn household_response_text_and_tts_never_exceed_yandex_limit() {
    let store = Arc::new(
        MemoryHouseholdStore::fixture()
            .house(1, "Дом", Some("thread-1"), "homey-1")
            .member(1, "OWNER")
            .surface(1, "OWNER", "owner-station"),
    );
    let app = router(household_state(
        store,
        ScriptedHouseRuntime::replying(&"А".repeat(2_000)),
    ));

    let (_, body) = post(
        app,
        "s3cret",
        alice_request_for("OWNER", "owner-station", "Расскажи", false),
    )
    .await;

    for field in ["text", "tts"] {
        assert!(body["response"][field].as_str().unwrap().chars().count() <= 1024);
    }
}
