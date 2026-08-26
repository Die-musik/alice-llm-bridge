use bridge_core::{
    HouseContext, HouseholdStore, HouseholdStoreError, PendingReply, SurfaceIdentity,
    SurfaceResolution,
};
use sqlx::{PgPool, Row};

use crate::state_crypto::{EncryptedPayload, StateCipher};

const PAIRING_TTL_SECONDS: i64 = 600;
const PENDING_TTL_SECONDS: i64 = 120;
const CONTINUATION_TTL_SECONDS: i64 = 600;

#[derive(Clone)]
pub struct PgHouseholdStore {
    pool: PgPool,
    cipher: StateCipher,
}

impl PgHouseholdStore {
    pub fn new(pool: PgPool, cipher: StateCipher) -> Self {
        Self { pool, cipher }
    }

    pub async fn approve_pairing(
        &self,
        house_id: i64,
        code: &str,
    ) -> Result<bool, HouseholdStoreError> {
        if code.len() != 6 || !code.chars().all(|character| character.is_ascii_digit()) {
            return Ok(false);
        }

        let mut transaction = self.pool.begin().await.map_err(db_error)?;
        let rows = sqlx::query(
            "SELECT id, yandex_user_id, application_id, code_hash \
             FROM pairing_requests \
             WHERE (house_id = $1 OR house_id IS NULL) \
               AND approved_at IS NULL AND expires_at > now() \
             FOR UPDATE",
        )
        .bind(house_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(db_error)?;

        let matching = rows.into_iter().find_map(|row| {
            let request_id: i64 = row.get("id");
            let user_id: String = row.get("yandex_user_id");
            let application_id: String = row.get("application_id");
            let stored_hash: Vec<u8> = row.get("code_hash");
            self.cipher
                .pairing_code_matches(&stored_hash, &user_id, &application_id, code)
                .then_some((request_id, user_id, application_id))
        });

        let Some((request_id, user_id, application_id)) = matching else {
            transaction.rollback().await.map_err(db_error)?;
            return Ok(false);
        };

        let membership_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(\
                SELECT 1 FROM house_members m JOIN houses h ON h.id = m.house_id \
                WHERE m.house_id = $1 AND m.yandex_user_id = $2 \
                  AND m.enabled AND h.enabled\
             )",
        )
        .bind(house_id)
        .bind(&user_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(db_error)?;
        if !membership_exists {
            transaction.rollback().await.map_err(db_error)?;
            return Ok(false);
        }

        let inserted = sqlx::query(
            "INSERT INTO surfaces (application_id, house_id, yandex_user_id) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (application_id) DO UPDATE \
             SET enabled = TRUE, updated_at = now() \
             WHERE surfaces.house_id = EXCLUDED.house_id \
               AND surfaces.yandex_user_id = EXCLUDED.yandex_user_id",
        )
        .bind(&application_id)
        .bind(house_id)
        .bind(&user_id)
        .execute(&mut *transaction)
        .await
        .map_err(db_error)?;
        if inserted.rows_affected() != 1 {
            transaction.rollback().await.map_err(db_error)?;
            return Err(HouseholdStoreError(
                "application is already bound to another household".to_owned(),
            ));
        }

        sqlx::query("UPDATE pairing_requests SET house_id = $2, approved_at = now() WHERE id = $1")
            .bind(request_id)
            .bind(house_id)
            .execute(&mut *transaction)
            .await
            .map_err(db_error)?;
        transaction.commit().await.map_err(db_error)?;
        Ok(true)
    }

    async fn create_pairing_request(
        &self,
        house_id: Option<i64>,
        user_id: &str,
        application_id: &str,
    ) -> Result<String, HouseholdStoreError> {
        let code = format!("{:06}", rand::random_range(0..1_000_000_u32));
        let code_hash = self
            .cipher
            .pairing_code_hash(user_id, application_id, &code);
        sqlx::query(
            "INSERT INTO pairing_requests \
             (house_id, yandex_user_id, application_id, code_hash, expires_at) \
             VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5)) \
             ON CONFLICT (application_id) DO UPDATE \
             SET house_id = EXCLUDED.house_id, \
                 yandex_user_id = EXCLUDED.yandex_user_id, \
                 code_hash = EXCLUDED.code_hash, \
                 expires_at = EXCLUDED.expires_at, \
                 approved_at = NULL, \
                 created_at = now()",
        )
        .bind(house_id)
        .bind(user_id)
        .bind(application_id)
        .bind(code_hash.as_slice())
        .bind(PAIRING_TTL_SECONDS as f64)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(code)
    }

    fn open_text(
        &self,
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
        key_version: i16,
    ) -> Result<String, HouseholdStoreError> {
        let plaintext = self
            .cipher
            .open(&EncryptedPayload {
                nonce,
                ciphertext,
                key_version,
            })
            .map_err(crypto_error)?;
        String::from_utf8(plaintext)
            .map_err(|_| HouseholdStoreError("encrypted state is not valid UTF-8".to_owned()))
    }
}

fn db_error(_: sqlx::Error) -> HouseholdStoreError {
    HouseholdStoreError("household database operation failed".to_owned())
}

fn crypto_error(_: crate::state_crypto::CryptoError) -> HouseholdStoreError {
    HouseholdStoreError("household state authentication failed".to_owned())
}

#[async_trait::async_trait]
impl HouseholdStore for PgHouseholdStore {
    async fn resolve_surface(
        &self,
        identity: &SurfaceIdentity,
    ) -> Result<SurfaceResolution, HouseholdStoreError> {
        let bound = sqlx::query(
            "SELECT h.id, h.name, h.codex_thread_id, h.homey_connector_id \
             FROM surfaces s \
             JOIN house_members m \
               ON m.house_id = s.house_id AND m.yandex_user_id = s.yandex_user_id \
             JOIN houses h ON h.id = s.house_id \
             WHERE s.application_id = $1 AND s.yandex_user_id = $2 \
               AND s.enabled AND m.enabled AND h.enabled",
        )
        .bind(&identity.application_id)
        .bind(&identity.user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?;
        if let Some(row) = bound {
            return Ok(SurfaceResolution::Bound(HouseContext {
                id: row.get("id"),
                name: row.get("name"),
                codex_thread_id: row.get("codex_thread_id"),
                homey_connector_id: row.get("homey_connector_id"),
            }));
        }

        let application_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM surfaces WHERE application_id = $1)")
                .bind(&identity.application_id)
                .fetch_one(&self.pool)
                .await
                .map_err(db_error)?;
        if application_exists {
            return Ok(SurfaceResolution::Unauthorized);
        }

        let memberships = sqlx::query(
            "SELECT m.house_id \
             FROM house_members m JOIN houses h ON h.id = m.house_id \
             WHERE m.yandex_user_id = $1 AND m.enabled AND h.enabled \
             ORDER BY m.house_id LIMIT 2",
        )
        .bind(&identity.user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;
        if memberships.is_empty() {
            return Ok(SurfaceResolution::Unauthorized);
        }
        let house_id = (memberships.len() == 1).then(|| memberships[0].get::<i64, _>("house_id"));
        let spoken_code = self
            .create_pairing_request(house_id, &identity.user_id, &identity.application_id)
            .await?;
        Ok(SurfaceResolution::PairingRequired { spoken_code })
    }

    async fn save_thread_id(
        &self,
        house_id: i64,
        thread_id: &str,
    ) -> Result<(), HouseholdStoreError> {
        let result = sqlx::query(
            "UPDATE houses SET codex_thread_id = $2, updated_at = now() \
             WHERE id = $1 AND (codex_thread_id IS NULL OR codex_thread_id = $2)",
        )
        .bind(house_id)
        .bind(thread_id)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(HouseholdStoreError(
                "house already has a different Codex thread".to_owned(),
            ))
        }
    }

    async fn poll_pending(
        &self,
        house_id: i64,
        application_id: &str,
    ) -> Result<PendingReply, HouseholdStoreError> {
        let row = sqlx::query(
            "SELECT status, nonce, ciphertext, key_version \
             FROM pending_replies \
             WHERE house_id = $1 AND application_id = $2 AND expires_at > now()",
        )
        .bind(house_id)
        .bind(application_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?;
        let Some(row) = row else {
            return Ok(PendingReply::None);
        };
        if row.get::<String, _>("status") == "thinking" {
            return Ok(PendingReply::Thinking);
        }
        let text = self.open_text(
            row.try_get("nonce").map_err(db_error)?,
            row.try_get("ciphertext").map_err(db_error)?,
            row.try_get("key_version").map_err(db_error)?,
        )?;
        Ok(PendingReply::Ready(text))
    }

    async fn mark_thinking(
        &self,
        house_id: i64,
        application_id: &str,
    ) -> Result<(), HouseholdStoreError> {
        sqlx::query(
            "INSERT INTO pending_replies \
             (house_id, application_id, status, expires_at) \
             VALUES ($1, $2, 'thinking', now() + make_interval(secs => $3)) \
             ON CONFLICT (house_id, application_id) DO UPDATE \
             SET status = 'thinking', nonce = NULL, ciphertext = NULL, key_version = NULL, \
                 expires_at = EXCLUDED.expires_at, updated_at = now()",
        )
        .bind(house_id)
        .bind(application_id)
        .bind(PENDING_TTL_SECONDS as f64)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn save_ready(
        &self,
        house_id: i64,
        application_id: &str,
        text: &str,
    ) -> Result<(), HouseholdStoreError> {
        let payload = self.cipher.seal(text.as_bytes()).map_err(crypto_error)?;
        sqlx::query(
            "INSERT INTO pending_replies \
             (house_id, application_id, status, nonce, ciphertext, key_version, expires_at) \
             VALUES ($1, $2, 'ready', $3, $4, $5, now() + make_interval(secs => $6)) \
             ON CONFLICT (house_id, application_id) DO UPDATE \
             SET status = 'ready', nonce = EXCLUDED.nonce, ciphertext = EXCLUDED.ciphertext, \
                 key_version = EXCLUDED.key_version, expires_at = EXCLUDED.expires_at, \
                 updated_at = now()",
        )
        .bind(house_id)
        .bind(application_id)
        .bind(payload.nonce)
        .bind(payload.ciphertext)
        .bind(payload.key_version)
        .bind(PENDING_TTL_SECONDS as f64)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn clear_pending(
        &self,
        house_id: i64,
        application_id: &str,
    ) -> Result<(), HouseholdStoreError> {
        sqlx::query("DELETE FROM pending_replies WHERE house_id = $1 AND application_id = $2")
            .bind(house_id)
            .bind(application_id)
            .execute(&self.pool)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    async fn take_continuation(
        &self,
        house_id: i64,
        application_id: &str,
    ) -> Result<Option<Vec<String>>, HouseholdStoreError> {
        let row = sqlx::query(
            "DELETE FROM continuations \
             WHERE house_id = $1 AND application_id = $2 AND expires_at > now() \
             RETURNING nonce, ciphertext, key_version",
        )
        .bind(house_id)
        .bind(application_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let text = self.open_text(
            row.try_get("nonce").map_err(db_error)?,
            row.try_get("ciphertext").map_err(db_error)?,
            row.try_get("key_version").map_err(db_error)?,
        )?;
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|_| HouseholdStoreError("continuation payload is invalid".to_owned()))
    }

    async fn save_continuation(
        &self,
        house_id: i64,
        application_id: &str,
        chunks: &[String],
    ) -> Result<(), HouseholdStoreError> {
        if chunks.is_empty() {
            return self.clear_continuation(house_id, application_id).await;
        }
        let plaintext = serde_json::to_vec(chunks)
            .map_err(|_| HouseholdStoreError("continuation serialization failed".to_owned()))?;
        let payload = self.cipher.seal(&plaintext).map_err(crypto_error)?;
        sqlx::query(
            "INSERT INTO continuations \
             (house_id, application_id, nonce, ciphertext, key_version, expires_at) \
             VALUES ($1, $2, $3, $4, $5, now() + make_interval(secs => $6)) \
             ON CONFLICT (house_id, application_id) DO UPDATE \
             SET nonce = EXCLUDED.nonce, ciphertext = EXCLUDED.ciphertext, \
                 key_version = EXCLUDED.key_version, expires_at = EXCLUDED.expires_at, \
                 updated_at = now()",
        )
        .bind(house_id)
        .bind(application_id)
        .bind(payload.nonce)
        .bind(payload.ciphertext)
        .bind(payload.key_version)
        .bind(CONTINUATION_TTL_SECONDS as f64)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        Ok(())
    }

    async fn clear_continuation(
        &self,
        house_id: i64,
        application_id: &str,
    ) -> Result<(), HouseholdStoreError> {
        sqlx::query("DELETE FROM continuations WHERE house_id = $1 AND application_id = $2")
            .bind(house_id)
            .bind(application_id)
            .execute(&self.pool)
            .await
            .map_err(db_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::PgHouseholdStore;
    use crate::state_crypto::StateCipher;
    use bridge_core::{HouseholdStore, PendingReply, SurfaceIdentity, SurfaceResolution};
    use sqlx::Row;

    const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    fn store(pool: sqlx::PgPool) -> PgHouseholdStore {
        PgHouseholdStore::new(pool, StateCipher::from_hex(KEY).unwrap())
    }

    async fn house(pool: &sqlx::PgPool, name: &str) -> i64 {
        sqlx::query("INSERT INTO houses (name, homey_connector_id) VALUES ($1, $2) RETURNING id")
            .bind(name)
            .bind(format!("homey-{name}"))
            .fetch_one(pool)
            .await
            .unwrap()
            .get("id")
    }

    async fn member(pool: &sqlx::PgPool, house_id: i64, user_id: &str) {
        sqlx::query(
            "INSERT INTO house_members (house_id, yandex_user_id, role) VALUES ($1, $2, 'member')",
        )
        .bind(house_id)
        .bind(user_id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn surface(pool: &sqlx::PgPool, house_id: i64, user_id: &str, application_id: &str) {
        sqlx::query(
            "INSERT INTO surfaces (house_id, yandex_user_id, application_id) VALUES ($1, $2, $3)",
        )
        .bind(house_id)
        .bind(user_id)
        .bind(application_id)
        .execute(pool)
        .await
        .unwrap();
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn resolution_is_exact_and_isolated_between_houses(pool: sqlx::PgPool) {
        let first = house(&pool, "Первый").await;
        let second = house(&pool, "Второй").await;
        member(&pool, first, "OWNER").await;
        member(&pool, second, "MOTHER").await;
        surface(&pool, first, "OWNER", "owner-station").await;
        surface(&pool, second, "MOTHER", "mother-station").await;
        let store = store(pool);

        let SurfaceResolution::Bound(owner_house) = store
            .resolve_surface(&SurfaceIdentity::new("OWNER", "owner-station"))
            .await
            .unwrap()
        else {
            panic!("owner surface must resolve")
        };
        assert_eq!(owner_house.id, first);

        assert_eq!(
            store
                .resolve_surface(&SurfaceIdentity::new("OWNER", "mother-station"))
                .await
                .unwrap(),
            SurfaceResolution::Unauthorized
        );
        assert_eq!(
            store
                .resolve_surface(&SurfaceIdentity::new("STRANGER", "new-station"))
                .await
                .unwrap(),
            SurfaceResolution::Unauthorized
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn pairing_expires_and_never_stores_plain_code(pool: sqlx::PgPool) {
        let house_id = house(&pool, "Мама").await;
        member(&pool, house_id, "MOTHER").await;
        let store = store(pool.clone());
        let identity = SurfaceIdentity::new("MOTHER", "new-station");

        let SurfaceResolution::PairingRequired { spoken_code } =
            store.resolve_surface(&identity).await.unwrap()
        else {
            panic!("known member must receive a pairing code")
        };
        assert_eq!(spoken_code.len(), 6);
        assert!(
            spoken_code
                .chars()
                .all(|character| character.is_ascii_digit())
        );
        let row = sqlx::query(
            "SELECT code_hash, encode(code_hash, 'escape') AS printable FROM pairing_requests WHERE application_id = $1",
        )
        .bind(&identity.application_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        let hash: Vec<u8> = row.get("code_hash");
        let printable: String = row.get("printable");
        assert_eq!(hash.len(), 32);
        assert!(!printable.contains(&spoken_code));

        sqlx::query("UPDATE pairing_requests SET expires_at = now() - interval '1 second'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(!store.approve_pairing(house_id, &spoken_code).await.unwrap());

        let SurfaceResolution::PairingRequired { spoken_code } =
            store.resolve_surface(&identity).await.unwrap()
        else {
            panic!("expired request must be replaced")
        };
        assert!(store.approve_pairing(house_id, &spoken_code).await.unwrap());
        assert!(matches!(
            store.resolve_surface(&identity).await.unwrap(),
            SurfaceResolution::Bound(_)
        ));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn owner_selects_house_when_member_belongs_to_more_than_one(pool: sqlx::PgPool) {
        let first = house(&pool, "Первый").await;
        let second = house(&pool, "Второй").await;
        member(&pool, first, "OWNER").await;
        member(&pool, second, "OWNER").await;
        let store = store(pool);
        let identity = SurfaceIdentity::new("OWNER", "new-station");

        let SurfaceResolution::PairingRequired { spoken_code } =
            store.resolve_surface(&identity).await.unwrap()
        else {
            panic!("multi-house member still needs a neutral pairing code")
        };
        assert!(store.approve_pairing(second, &spoken_code).await.unwrap());
        let SurfaceResolution::Bound(bound) = store.resolve_surface(&identity).await.unwrap()
        else {
            panic!("approved surface must resolve")
        };
        assert_eq!(bound.id, second);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn temporary_state_is_encrypted_scoped_and_expires(pool: sqlx::PgPool) {
        let house_id = house(&pool, "Дом").await;
        member(&pool, house_id, "OWNER").await;
        surface(&pool, house_id, "OWNER", "station").await;
        let store = store(pool.clone());

        store.mark_thinking(house_id, "station").await.unwrap();
        assert_eq!(
            store.poll_pending(house_id, "station").await.unwrap(),
            PendingReply::Thinking
        );
        store
            .save_ready(house_id, "station", "секретный ответ")
            .await
            .unwrap();
        let row = sqlx::query(
            "SELECT ciphertext, nonce FROM pending_replies WHERE house_id = $1 AND application_id = $2",
        )
        .bind(house_id)
        .bind("station")
        .fetch_one(&pool)
        .await
        .unwrap();
        let ciphertext: Vec<u8> = row.get("ciphertext");
        let nonce: Vec<u8> = row.get("nonce");
        assert_eq!(nonce.len(), 12);
        assert!(
            !ciphertext
                .windows("секретный ответ".len())
                .any(|window| window == "секретный ответ".as_bytes())
        );
        assert_eq!(
            store.poll_pending(house_id, "station").await.unwrap(),
            PendingReply::Ready("секретный ответ".to_owned())
        );

        sqlx::query("UPDATE pending_replies SET expires_at = now() - interval '1 second'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            store.poll_pending(house_id, "station").await.unwrap(),
            PendingReply::None
        );

        let chunks = vec!["первая часть".to_owned(), "вторая часть".to_owned()];
        store
            .save_continuation(house_id, "station", &chunks)
            .await
            .unwrap();
        assert_eq!(
            store.take_continuation(house_id, "station").await.unwrap(),
            Some(chunks.clone())
        );
        assert!(
            store
                .take_continuation(house_id, "station")
                .await
                .unwrap()
                .is_none()
        );

        store
            .save_continuation(house_id, "station", &chunks)
            .await
            .unwrap();
        sqlx::query("UPDATE continuations SET expires_at = now() - interval '1 second'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            store
                .take_continuation(house_id, "station")
                .await
                .unwrap()
                .is_none()
        );

        assert_eq!(
            store.poll_pending(house_id + 1, "station").await.unwrap(),
            PendingReply::None
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn thread_id_cannot_be_replaced(pool: sqlx::PgPool) {
        let house_id = house(&pool, "Дом").await;
        let store = store(pool);

        store.save_thread_id(house_id, "thread-1").await.unwrap();
        store.save_thread_id(house_id, "thread-1").await.unwrap();
        assert!(store.save_thread_id(house_id, "thread-2").await.is_err());
    }
}
