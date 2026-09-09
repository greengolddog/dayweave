use std::sync::Arc;
use uuid::Uuid;

use super::{
    ItemCompletionCommand, ItemCompletionError, ItemCompletionMutation, ItemCompletionSnapshot,
};
use crate::{
    items::ItemService, proposals::Clock, scheduling::truncate_to_postgres_timestamp_precision,
};

pub struct ItemCompletionService {
    items: Arc<ItemService>,
    clock: Arc<dyn Clock>,
}

impl ItemCompletionService {
    #[must_use]
    pub fn new(items: Arc<ItemService>, clock: Arc<dyn Clock>) -> Self {
        Self { items, clock }
    }

    /// Reads reviewed completion policy joined to canonical and execution authority.
    ///
    /// # Errors
    /// Returns missing-item or unavailable-authority errors without inventing empty state.
    pub async fn get(&self, item_id: Uuid) -> Result<ItemCompletionSnapshot, ItemCompletionError> {
        if item_id.is_nil() {
            return Err(ItemCompletionError::Invalid);
        }
        self.items
            .completion_repository()
            .get_completion(item_id)
            .await
    }

    /// Reviews completion policy and reconciles the complete canonical forest.
    ///
    /// # Errors
    /// Returns typed validation, exact revision, operation-reuse, or storage failures.
    pub async fn put(
        &self,
        item_id: Uuid,
        command: ItemCompletionCommand,
        actor_session_id: Option<Uuid>,
    ) -> Result<ItemCompletionMutation, ItemCompletionError> {
        self.items
            .completion_repository()
            .put_completion(
                item_id,
                command,
                truncate_to_postgres_timestamp_precision(self.clock.now()),
                actor_session_id,
            )
            .await
    }
}
