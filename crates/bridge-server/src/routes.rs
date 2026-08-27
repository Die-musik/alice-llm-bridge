//! The webhook route: access control, greeting and dispatch into the engine.

use std::collections::HashSet;

use alice_protocol::{WebhookRequest, WebhookResponse};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bridge_core::{
    Engine, HouseholdEngine, HouseholdInput, HouseholdReply, SurfaceIdentity, phrases,
};

pub const GREETING: &str = "Привет! Я мост к нейросетям. Задай любой вопрос или скажи: помощь.";
pub const REFUSAL: &str = "Извини, это семейный навык. Я отвечаю только своим.";
pub const HOUSEHOLD_REFUSAL: &str = "Извините, эта Алиса не привязана к домашнему помощнику.";
pub const HOUSEHOLD_BUSY: &str = "Я уже отвечаю в этом доме. Повторите через несколько секунд.";

#[derive(Clone)]
pub enum SkillBackend {
    Legacy(Engine),
    Household(std::sync::Arc<HouseholdEngine>),
}

/// Shared server state; `Engine` is cheap to clone (an `Arc` inside).
#[derive(Clone)]
pub struct AppState {
    pub backend: SkillBackend,
    pub webhook_secret: String,
    pub allowed_user_ids: HashSet<String>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/alice/webhook/{secret}", post(webhook))
        .route("/health", get(|| async { "ok" }))
        .with_state(state)
}

async fn webhook(
    State(state): State<AppState>,
    Path(secret): Path<String>,
    Json(request): Json<WebhookRequest>,
) -> Response {
    if secret != state.webhook_secret {
        return StatusCode::NOT_FOUND.into_response();
    }

    let utterance = request.request.original_utterance.trim().to_string();
    let application_id = request.session.application.application_id.clone();

    match state.backend {
        SkillBackend::Legacy(engine) => {
            let user_id = request
                .session
                .user
                .as_ref()
                .map(|user| user.user_id.clone())
                .unwrap_or(application_id);
            if !state.allowed_user_ids.is_empty() && !state.allowed_user_ids.contains(&user_id) {
                tracing::warn!(outcome = "unauthorized", "rejected unknown user");
                return Json(WebhookResponse::say_and_close(REFUSAL)).into_response();
            }
            if request.session.new && utterance.is_empty() {
                return Json(WebhookResponse::say(GREETING)).into_response();
            }

            let reply = tokio::spawn(async move { engine.handle(&user_id, &utterance).await })
                .await
                .unwrap_or_else(|err| {
                    tracing::error!(error = %err, "engine task failed");
                    phrases::PHRASE_INTERNAL_ERROR.to_string()
                });
            Json(WebhookResponse::say(reply)).into_response()
        }
        SkillBackend::Household(engine) => {
            let Some(user_id) = request.session.user.map(|user| user.user_id) else {
                return Json(WebhookResponse::say_and_close(HOUSEHOLD_REFUSAL)).into_response();
            };
            let input = HouseholdInput {
                identity: SurfaceIdentity::new(user_id, application_id),
                utterance,
                new_session: request.session.new,
            };
            let reply = tokio::spawn(async move { engine.respond(input).await })
                .await
                .unwrap_or_else(|err| {
                    tracing::error!(error = %err, "household engine task failed");
                    HouseholdReply::InternalError
                });
            household_response(reply)
        }
    }
}

fn household_response(reply: HouseholdReply) -> Response {
    let response = match reply {
        HouseholdReply::Say(text) => WebhookResponse::say(text),
        HouseholdReply::Deferred => WebhookResponse::say_and_close("Секунду."),
        HouseholdReply::Refuse => WebhookResponse::say_and_close(HOUSEHOLD_REFUSAL),
        HouseholdReply::Pairing(code) => {
            WebhookResponse::say(format!("Код привязки: {code}. Сообщите его владельцу."))
        }
        HouseholdReply::Busy => WebhookResponse::say(HOUSEHOLD_BUSY),
        HouseholdReply::InternalError => WebhookResponse::say(phrases::PHRASE_INTERNAL_ERROR),
    };
    Json(response).into_response()
}
