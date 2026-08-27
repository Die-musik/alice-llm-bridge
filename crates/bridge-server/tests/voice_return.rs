use bridge_core::VoiceReturn;
use bridge_server::voice_return::{
    YandexEndpoints, YandexVoiceReturn, YandexVoiceReturnConfig, YandexVoiceReturnTarget,
};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn provisions_static_skill_reinvoke_and_runs_it_for_the_mapped_surface() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/x_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok",
            "passport_host": server.uri(),
            "track_id": "track-1"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth/session/"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("set-cookie", "Session_id=test; Path=/"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/quasar/iot"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(r#"<script>window.state={"csrfToken2":"csrf-1"}</script>"#),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/scenarios"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok",
            "scenarios": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/scenarios/create"))
        .and(body_partial_json(serde_json::json!({
            "name": "Соня GPT — гостиная",
            "steps": [{
                "parameters": {"items": [{"id": "living-room-device", "value": {
                    "id": "living-room-device", "capabilities": [{
                    "state": {
                        "instance": "text_action",
                        "value": "СКАЖИ НАВЫКУ Искусственный интеллект"
                    }
                }]}}]}
            }]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok",
            "scenario_id": "scenario-1"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/scenarios/scenario-1/actions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let voice_return = YandexVoiceReturn::connect_with_endpoints(
        YandexVoiceReturnConfig {
            x_token: "secret-token".to_owned(),
            activation_name: "Искусственный интеллект".to_owned(),
            targets: vec![YandexVoiceReturnTarget {
                application_id: "living-room-surface".to_owned(),
                device_id: "living-room-device".to_owned(),
                scenario_name: "Соня GPT — гостиная".to_owned(),
            }],
        },
        YandexEndpoints::under(&server.uri()),
    )
    .await
    .unwrap();

    assert!(voice_return.supports("living-room-surface"));
    assert!(!voice_return.supports("bedroom-surface"));
    voice_return
        .answer_ready("living-room-surface")
        .await
        .unwrap();
}

#[tokio::test]
async fn rewrites_an_existing_named_scenario_before_reusing_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/x_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok", "passport_host": server.uri(), "track_id": "track-1"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/auth/session/"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/quasar/iot"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"csrfToken2":"csrf-1"}"#))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/scenarios"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok",
            "scenarios": [{"id": "scenario-1", "name": "Соня GPT — гостиная"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/scenarios/scenario-1"))
        .and(body_partial_json(serde_json::json!({
            "name": "Соня GPT — гостиная",
            "steps": [{"parameters": {"items": [{"id": "living-room-device", "value": {
                "id": "living-room-device", "capabilities": [{
                "state": {"value": "СКАЖИ НАВЫКУ Искусственный интеллект"}
            }]}}]}}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "status": "ok"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let voice_return = YandexVoiceReturn::connect_with_endpoints(
        YandexVoiceReturnConfig {
            x_token: "secret-token".to_owned(),
            activation_name: "Искусственный интеллект".to_owned(),
            targets: vec![YandexVoiceReturnTarget {
                application_id: "living-room-surface".to_owned(),
                device_id: "living-room-device".to_owned(),
                scenario_name: "Соня GPT — гостиная".to_owned(),
            }],
        },
        YandexEndpoints::under(&server.uri()),
    )
    .await
    .unwrap();

    assert!(voice_return.supports("living-room-surface"));
}
