use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use sqlx::PgPool;

#[derive(Clone, Default)]
pub struct Readiness {
    enabled: Arc<AtomicBool>,
    database: Option<PgPool>,
}

impl std::fmt::Debug for Readiness {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Readiness")
            .field("enabled", &self.is_ready())
            .field("database_configured", &self.database.is_some())
            .finish()
    }
}

impl Readiness {
    #[must_use]
    pub fn with_database(database: PgPool) -> Self {
        Self {
            enabled: Arc::new(AtomicBool::new(false)),
            database: Some(database),
        }
    }

    pub fn set_ready(&self, ready: bool) {
        self.enabled.store(ready, Ordering::Release);
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub async fn check(&self) -> bool {
        if !self.is_ready() {
            return false;
        }
        let Some(database) = &self.database else {
            return true;
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(database),
        )
        .await
        .is_ok_and(|result| result.is_ok_and(|value| value == 1))
    }
}
