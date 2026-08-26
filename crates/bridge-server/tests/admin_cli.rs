use std::process::{Command, Output};

use bridge_core::{HouseholdStore, SurfaceIdentity, SurfaceResolution};
use bridge_server::{house_store_pg::PgHouseholdStore, state_crypto::StateCipher};
use sqlx::ConnectOptions;

const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

fn run(database_url: &str, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bridge-admin"))
        .args(arguments)
        .env("DATABASE_URL", database_url)
        .env("STATE_ENCRYPTION_KEY", KEY)
        .output()
        .unwrap()
}

fn success(database_url: &str, arguments: &[&str]) -> String {
    let output = run(database_url, arguments);
    assert!(
        output.status.success(),
        "bridge-admin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn owner_approves_exact_surface_without_echoing_private_ids(pool: sqlx::PgPool) {
    let database_url = pool.connect_options().to_url_lossy().to_string();
    let created = success(
        &database_url,
        &[
            "house",
            "create",
            "--name",
            "Дом мамы",
            "--homey-connector",
            "homey-mother",
        ],
    );
    let house_id: i64 = created
        .trim()
        .strip_prefix("house created: ")
        .unwrap()
        .parse()
        .unwrap();
    let member_output = success(
        &database_url,
        &[
            "member",
            "add",
            "--house",
            &house_id.to_string(),
            "--user-id",
            "MOTHER-PRIVATE-ID",
            "--role",
            "member",
        ],
    );
    assert!(!member_output.contains("MOTHER-PRIVATE-ID"));

    let store = PgHouseholdStore::new(pool, StateCipher::from_hex(KEY).unwrap());
    let identity = SurfaceIdentity::new("MOTHER-PRIVATE-ID", "MOTHER-STATION-PRIVATE-ID");
    let SurfaceResolution::PairingRequired { spoken_code } =
        store.resolve_surface(&identity).await.unwrap()
    else {
        panic!("new member surface must require pairing")
    };
    let approved = success(
        &database_url,
        &[
            "pairing",
            "approve",
            "--house",
            &house_id.to_string(),
            "--code",
            &spoken_code,
        ],
    );
    assert!(!approved.contains("MOTHER-PRIVATE-ID"));
    assert!(!approved.contains("MOTHER-STATION-PRIVATE-ID"));

    assert!(matches!(
        store.resolve_surface(&identity).await.unwrap(),
        SurfaceResolution::Bound(_)
    ));
    assert!(!matches!(
        store
            .resolve_surface(&SurfaceIdentity::new(
                "MOTHER-PRIVATE-ID",
                "ANOTHER-STATION"
            ))
            .await
            .unwrap(),
        SurfaceResolution::Bound(_)
    ));
}

#[test]
fn invalid_or_extra_flags_fail_without_echoing_values() {
    let output = run(
        "postgresql://unused.invalid/unused",
        &[
            "pairing",
            "approve",
            "--house",
            "1",
            "--code",
            "not-six-digits",
            "--extra",
            "PRIVATE",
        ],
    );

    assert!(!output.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!combined.contains("PRIVATE"));
    assert!(!combined.contains("not-six-digits"));
}
