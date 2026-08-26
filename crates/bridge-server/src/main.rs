use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use bridge_server::assemble::{build_engine, build_household_engine};
use bridge_server::config::{AppConfig, RuntimeMode};
use bridge_server::house_store_pg::PgHouseholdStore;
use bridge_server::routes::{AppState, SkillBackend, router};
use bridge_server::state_crypto::StateCipher;
use bridge_server::store_pg::PgStore;
use codex_runtime::{CodexRuntime, CodexRuntimeConfig};
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config_path =
        PathBuf::from(std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config.toml".to_string()));
    let config = AppConfig::load(&config_path)
        .with_context(|| format!("loading {}", config_path.display()))?;
    let webhook_secret = std::env::var("WEBHOOK_SECRET").context("WEBHOOK_SECRET must be set")?;
    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL must be set")?;

    if config.runtime.mode == RuntimeMode::Legacy && config.server.allowed_user_ids.is_empty() {
        tracing::warn!("allowed_user_ids is empty: every request will be accepted");
    }

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .context("connecting to postgres")?;
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .context("running migrations")?;

    let backend = match config.runtime.mode {
        RuntimeMode::Legacy => {
            SkillBackend::Legacy(build_engine(&config, Arc::new(PgStore::new(pool.clone())))?)
        }
        RuntimeMode::HouseholdCodex => {
            let key = std::env::var("STATE_ENCRYPTION_KEY")
                .context("STATE_ENCRYPTION_KEY must be set in household mode")?;
            let cipher = StateCipher::from_hex(&key).context("invalid STATE_ENCRYPTION_KEY")?;
            let codex = CodexRuntime::new(CodexRuntimeConfig {
                socket_path: config
                    .runtime
                    .codex_socket
                    .clone()
                    .expect("validated household codex_socket"),
                cwd_root: config
                    .runtime
                    .codex_cwd_root
                    .clone()
                    .expect("validated household codex_cwd_root"),
                permission_profile_prefix: config.runtime.permission_profile_prefix.clone(),
                homey_enabled: config.runtime.homey_enabled,
            })
            .context("invalid Codex runtime config")?;
            SkillBackend::Household(Arc::new(build_household_engine(
                &config,
                Arc::new(PgHouseholdStore::new(pool.clone(), cipher)),
                Arc::new(codex),
            )?))
        }
    };
    let state = AppState {
        backend,
        webhook_secret,
        allowed_user_ids: HashSet::from_iter(config.server.allowed_user_ids.iter().cloned()),
    };

    let listener = tokio::net::TcpListener::bind(config.server.listen).await?;
    tracing::info!(addr = %config.server.listen, "listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("ctrl-c handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("sigterm handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("shutting down");
}
