use std::collections::{HashMap, HashSet};

use bridge_server::{house_store_pg::PgHouseholdStore, state_crypto::StateCipher};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};

#[derive(Debug)]
enum AdminCommand {
    CreateHouse {
        name: String,
        homey_connector: String,
    },
    AddMember {
        house_id: i64,
        user_id: String,
        role: String,
    },
    ApprovePairing {
        house_id: i64,
        code: String,
    },
    DisableMember {
        house_id: i64,
        user_id: String,
    },
    DisableSurface {
        application_id: String,
    },
}

#[derive(Debug, thiserror::Error)]
enum AdminError {
    #[error("invalid arguments")]
    InvalidArguments,
    #[error("required environment is missing or invalid")]
    InvalidEnvironment,
    #[error("database operation failed")]
    Database,
    #[error("pairing request was not found or has expired")]
    PairingNotFound,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        std::process::exit(2);
    }
}

async fn run() -> Result<(), AdminError> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let command = parse_command(&arguments)?;
    let database_url = std::env::var("DATABASE_URL").map_err(|_| AdminError::InvalidEnvironment)?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .map_err(|_| AdminError::Database)?;

    execute(command, pool).await
}

fn parse_command(arguments: &[String]) -> Result<AdminCommand, AdminError> {
    let Some((namespace, rest)) = arguments.split_first() else {
        return Err(AdminError::InvalidArguments);
    };
    let Some((action, flags)) = rest.split_first() else {
        return Err(AdminError::InvalidArguments);
    };

    match (namespace.as_str(), action.as_str()) {
        ("house", "create") => {
            let flags = parse_flags(flags, &["--name", "--homey-connector"])?;
            Ok(AdminCommand::CreateHouse {
                name: required(&flags, "--name")?.to_owned(),
                homey_connector: required(&flags, "--homey-connector")?.to_owned(),
            })
        }
        ("member", "add") => {
            let flags = parse_flags(flags, &["--house", "--user-id", "--role"])?;
            let role = required(&flags, "--role")?;
            if !matches!(role, "owner" | "member") {
                return Err(AdminError::InvalidArguments);
            }
            Ok(AdminCommand::AddMember {
                house_id: positive_id(required(&flags, "--house")?)?,
                user_id: required(&flags, "--user-id")?.to_owned(),
                role: role.to_owned(),
            })
        }
        ("pairing", "approve") => {
            let flags = parse_flags(flags, &["--house", "--code"])?;
            let code = required(&flags, "--code")?;
            if code.len() != 6 || !code.chars().all(|character| character.is_ascii_digit()) {
                return Err(AdminError::InvalidArguments);
            }
            Ok(AdminCommand::ApprovePairing {
                house_id: positive_id(required(&flags, "--house")?)?,
                code: code.to_owned(),
            })
        }
        ("member", "disable") => {
            let flags = parse_flags(flags, &["--house", "--user-id"])?;
            Ok(AdminCommand::DisableMember {
                house_id: positive_id(required(&flags, "--house")?)?,
                user_id: required(&flags, "--user-id")?.to_owned(),
            })
        }
        ("surface", "disable") => {
            let flags = parse_flags(flags, &["--application-id"])?;
            Ok(AdminCommand::DisableSurface {
                application_id: required(&flags, "--application-id")?.to_owned(),
            })
        }
        _ => Err(AdminError::InvalidArguments),
    }
}

fn parse_flags<'a>(
    arguments: &'a [String],
    expected: &[&str],
) -> Result<HashMap<String, &'a str>, AdminError> {
    if arguments.len() != expected.len() * 2 {
        return Err(AdminError::InvalidArguments);
    }
    let expected: HashSet<&str> = expected.iter().copied().collect();
    let mut parsed = HashMap::new();
    for pair in arguments.chunks_exact(2) {
        let name = pair[0].as_str();
        let value = pair[1].as_str();
        if !expected.contains(name)
            || value.is_empty()
            || parsed.insert(name.to_owned(), value).is_some()
        {
            return Err(AdminError::InvalidArguments);
        }
    }
    Ok(parsed)
}

fn required<'a>(flags: &'a HashMap<String, &'a str>, name: &str) -> Result<&'a str, AdminError> {
    flags.get(name).copied().ok_or(AdminError::InvalidArguments)
}

fn positive_id(value: &str) -> Result<i64, AdminError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(AdminError::InvalidArguments)
}

async fn execute(command: AdminCommand, pool: PgPool) -> Result<(), AdminError> {
    match command {
        AdminCommand::CreateHouse {
            name,
            homey_connector,
        } => {
            let row = sqlx::query(
                "INSERT INTO houses (name, homey_connector_id) VALUES ($1, $2) RETURNING id",
            )
            .bind(name)
            .bind(homey_connector)
            .fetch_one(&pool)
            .await
            .map_err(|_| AdminError::Database)?;
            println!("house created: {}", row.get::<i64, _>("id"));
        }
        AdminCommand::AddMember {
            house_id,
            user_id,
            role,
        } => {
            sqlx::query(
                "INSERT INTO house_members (house_id, yandex_user_id, role) \
                 VALUES ($1, $2, $3) \
                 ON CONFLICT (house_id, yandex_user_id) DO UPDATE \
                 SET role = EXCLUDED.role, enabled = TRUE, updated_at = now()",
            )
            .bind(house_id)
            .bind(user_id)
            .bind(role)
            .execute(&pool)
            .await
            .map_err(|_| AdminError::Database)?;
            println!("member added");
        }
        AdminCommand::ApprovePairing { house_id, code } => {
            let key = std::env::var("STATE_ENCRYPTION_KEY")
                .map_err(|_| AdminError::InvalidEnvironment)?;
            let cipher = StateCipher::from_hex(&key).map_err(|_| AdminError::InvalidEnvironment)?;
            let store = PgHouseholdStore::new(pool, cipher);
            if !store
                .approve_pairing(house_id, &code)
                .await
                .map_err(|_| AdminError::Database)?
            {
                return Err(AdminError::PairingNotFound);
            }
            println!("pairing approved");
        }
        AdminCommand::DisableMember { house_id, user_id } => {
            sqlx::query(
                "UPDATE house_members SET enabled = FALSE, updated_at = now() \
                 WHERE house_id = $1 AND yandex_user_id = $2",
            )
            .bind(house_id)
            .bind(user_id)
            .execute(&pool)
            .await
            .map_err(|_| AdminError::Database)?;
            println!("member disabled");
        }
        AdminCommand::DisableSurface { application_id } => {
            sqlx::query(
                "UPDATE surfaces SET enabled = FALSE, updated_at = now() \
                 WHERE application_id = $1",
            )
            .bind(application_id)
            .execute(&pool)
            .await
            .map_err(|_| AdminError::Database)?;
            println!("surface disabled");
        }
    }
    Ok(())
}
