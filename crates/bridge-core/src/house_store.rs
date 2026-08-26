use crate::household::{PendingReply, SurfaceIdentity, SurfaceResolution};

pub type StoreResult<T> = Result<T, HouseholdStoreError>;

#[derive(Debug, thiserror::Error)]
#[error("household storage error: {0}")]
pub struct HouseholdStoreError(pub String);

#[async_trait::async_trait]
pub trait HouseholdStore: Send + Sync {
    async fn resolve_surface(&self, identity: &SurfaceIdentity) -> StoreResult<SurfaceResolution>;
    async fn save_thread_id(&self, house_id: i64, thread_id: &str) -> StoreResult<()>;
    async fn poll_pending(&self, house_id: i64, application_id: &str) -> StoreResult<PendingReply>;
    async fn mark_thinking(&self, house_id: i64, application_id: &str) -> StoreResult<()>;
    async fn save_ready(&self, house_id: i64, application_id: &str, text: &str) -> StoreResult<()>;
    async fn clear_pending(&self, house_id: i64, application_id: &str) -> StoreResult<()>;
    async fn take_continuation(
        &self,
        house_id: i64,
        application_id: &str,
    ) -> StoreResult<Option<Vec<String>>>;
    async fn save_continuation(
        &self,
        house_id: i64,
        application_id: &str,
        chunks: &[String],
    ) -> StoreResult<()>;
    async fn clear_continuation(&self, house_id: i64, application_id: &str) -> StoreResult<()>;
}

#[cfg(test)]
mod tests {
    use super::HouseholdStore;
    use crate::household::{SurfaceIdentity, SurfaceResolution};
    use crate::testing::MemoryHouseholdStore;

    #[tokio::test]
    async fn two_accounts_and_surfaces_resolve_to_one_thread() {
        let store = MemoryHouseholdStore::fixture()
            .house(1, "Дом мамы", Some("thread-1"), "homey-mother")
            .member(1, "OWNER")
            .member(1, "MOTHER")
            .surface(1, "OWNER", "owner-phone")
            .surface(1, "MOTHER", "mother-station");

        for identity in [
            SurfaceIdentity::new("OWNER", "owner-phone"),
            SurfaceIdentity::new("MOTHER", "mother-station"),
        ] {
            let SurfaceResolution::Bound(house) = store.resolve_surface(&identity).await.unwrap()
            else {
                panic!("expected a bound household surface")
            };
            assert_eq!(house.id, 1);
            assert_eq!(house.codex_thread_id.as_deref(), Some("thread-1"));
        }
    }

    #[tokio::test]
    async fn known_member_can_pair_but_stranger_learns_nothing_about_house() {
        let store = MemoryHouseholdStore::fixture()
            .house(1, "Дом мамы", None, "homey-mother")
            .member(1, "MOTHER");

        let known = store
            .resolve_surface(&SurfaceIdentity::new("MOTHER", "new-station"))
            .await
            .unwrap();
        assert!(matches!(
            known,
            SurfaceResolution::PairingRequired { spoken_code } if spoken_code.len() == 6
        ));

        let stranger = store
            .resolve_surface(&SurfaceIdentity::new("STRANGER", "new-station"))
            .await
            .unwrap();
        assert_eq!(stranger, SurfaceResolution::Unauthorized);
    }

    #[tokio::test]
    async fn application_cannot_be_attached_to_two_houses() {
        let result = std::panic::catch_unwind(|| {
            MemoryHouseholdStore::fixture()
                .house(1, "Первый", None, "homey-1")
                .house(2, "Второй", None, "homey-2")
                .member(1, "OWNER")
                .member(2, "MOTHER")
                .surface(1, "OWNER", "shared-station")
                .surface(2, "MOTHER", "shared-station")
        });

        assert!(result.is_err());
    }
}
