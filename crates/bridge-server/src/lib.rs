//! Axum webhook server: configuration, engine assembly and the Postgres
//! adapter for the dialogue engine defined in `bridge-core`.

pub mod assemble;
pub mod config;
pub mod house_store_pg;
pub mod routes;
pub mod state_crypto;
pub mod store_pg;
