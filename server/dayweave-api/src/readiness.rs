use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use sqlx::PgPool;
use uuid::Uuid;

#[derive(Clone, Default)]
pub struct Readiness {
    enabled: Arc<AtomicBool>,
    durability_blockers: Arc<AtomicUsize>,
    database: Option<PgPool>,
    oauth_scope: Option<(Uuid, Uuid)>,
}

impl std::fmt::Debug for Readiness {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Readiness")
            .field("enabled", &self.is_ready())
            .field(
                "durability_blockers",
                &self.durability_blockers.load(Ordering::Acquire),
            )
            .field("database_configured", &self.database.is_some())
            .field("oauth_scope_monitored", &self.oauth_scope.is_some())
            .finish()
    }
}

impl Readiness {
    #[must_use]
    pub fn with_database(database: PgPool, workspace_id: Uuid, user_id: Uuid) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(false)),
            durability_blockers: Arc::new(AtomicUsize::new(0)),
            database: Some(database),
            oauth_scope: Some((workspace_id, user_id)),
        }
    }

    pub fn set_ready(&self, ready: bool) {
        self.enabled.store(ready, Ordering::Release);
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
            && self.durability_blockers.load(Ordering::Acquire) == 0
    }

    /// Marks a live credential whose ownership is currently only in process
    /// memory. Readiness remains false until it is durably held or definitively
    /// revoked. The counter is non-secret and shared by all readiness clones.
    pub(crate) fn add_durability_blocker(&self) {
        self.durability_blockers.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn remove_durability_blocker(&self) {
        let result =
            self.durability_blockers
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                    value.checked_sub(1)
                });
        debug_assert!(result.is_ok(), "durability blocker counter underflow");
    }

    pub async fn check(&self) -> bool {
        if !self.is_ready() {
            return false;
        }
        let Some(database) = &self.database else {
            return true;
        };
        let database_healthy = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(database),
        )
        .await
        .is_ok_and(|result| result.is_ok_and(|value| value == 1));
        if !database_healthy {
            return false;
        }
        let Some((workspace_id, user_id)) = self.oauth_scope else {
            return true;
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            sqlx::query_scalar::<_, bool>(
                "SELECT NOT ( \
                    EXISTS(SELECT 1 FROM google_oauth_scope_state WHERE workspace_id = $1 \
                        AND user_id = $2 AND revocation_kind IS NOT NULL) OR \
                    EXISTS(SELECT 1 FROM google_oauth_sessions WHERE workspace_id = $1 \
                        AND user_id = $2 AND status = 'exchanging') OR \
                    EXISTS(SELECT 1 FROM google_oauth_cleanup_tokens WHERE workspace_id = $1 \
                        AND user_id = $2 AND (status = 'operator_required' OR attempt_count >= 12)) OR \
                    EXISTS(SELECT 1 FROM google_oauth_legacy_credential_quarantine \
                        WHERE workspace_id = $1 AND user_id = $2 AND recovery_confirmed_at IS NULL) OR \
                    EXISTS(SELECT 1 FROM google_sync_runs WHERE workspace_id = $1 AND user_id = $2 \
                        AND (state IN ('reauthorization_required', 'failed') \
                            OR (state = 'running' AND lease_until <= clock_timestamp()))) OR \
                    EXISTS(SELECT 1 FROM google_sync_outbox WHERE workspace_id = $1 AND user_id = $2 \
                        AND (state = 'failed' OR (state = 'delivering' \
                            AND claimed_at <= clock_timestamp() - interval '10 minutes'))) \
                )",
            )
            .bind(workspace_id)
            .bind(user_id)
            .fetch_one(database),
        )
        .await
        .is_ok_and(|result| result.is_ok_and(std::convert::identity))
    }
}
