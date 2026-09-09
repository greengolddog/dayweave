use std::sync::Arc;
use uuid::Uuid;

use super::{ItemProgressCommand, ItemProgressError, ItemProgressMutation, ItemProgressSnapshot};
use crate::{
    items::ItemService, proposals::Clock, scheduling::truncate_to_postgres_timestamp_precision,
};

pub struct ItemProgressService {
    items: Arc<ItemService>,
    clock: Arc<dyn Clock>,
}

impl ItemProgressService {
    #[must_use]
    pub fn new(items: Arc<ItemService>, clock: Arc<dyn Clock>) -> Self {
        Self { items, clock }
    }

    /// Reads a complete independent-progress snapshot joined to current item authority.
    ///
    /// # Errors
    /// Returns missing-item or unavailable-authority errors without inventing empty state.
    pub async fn get(&self, item_id: Uuid) -> Result<ItemProgressSnapshot, ItemProgressError> {
        if item_id.is_nil() {
            return Err(ItemProgressError::Invalid);
        }
        self.items.progress_repository().get_progress(item_id).await
    }

    /// Replaces independent progress without touching canonical or execution state.
    ///
    /// # Errors
    /// Returns typed validation, exact revision, operation-reuse, or storage failures.
    pub async fn put(
        &self,
        item_id: Uuid,
        command: ItemProgressCommand,
        actor_session_id: Option<Uuid>,
    ) -> Result<ItemProgressMutation, ItemProgressError> {
        self.items
            .progress_repository()
            .put_progress(
                item_id,
                command,
                truncate_to_postgres_timestamp_precision(self.clock.now()),
                actor_session_id,
            )
            .await
    }
}
