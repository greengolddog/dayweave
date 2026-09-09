use std::collections::HashMap;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{ItemProgressCommand, ItemProgressError, ItemProgressMutation, ItemProgressSnapshot};

/// Kept inside the canonical in-memory repository mutex, not a second service lock.
#[derive(Clone, Debug, Default)]
pub(crate) struct MemoryProgress {
    snapshots: HashMap<Uuid, ItemProgressSnapshot>,
    receipts: HashMap<Uuid, Receipt>,
}

#[derive(Clone, Debug)]
struct Receipt {
    item_id: Uuid,
    command: ItemProgressCommand,
    before: ItemProgressSnapshot,
    after: ItemProgressMutation,
}

impl MemoryProgress {
    pub(crate) fn get(&self, item_id: Uuid, item_revision: u64) -> ItemProgressSnapshot {
        let mut snapshot = self
            .snapshots
            .get(&item_id)
            .cloned()
            .unwrap_or_else(|| ItemProgressSnapshot::empty(item_id, item_revision));
        snapshot.item_revision = item_revision;
        snapshot
    }

    pub(crate) fn replay(
        &self,
        item_id: Uuid,
        command: &ItemProgressCommand,
    ) -> Result<Option<ItemProgressMutation>, ItemProgressError> {
        let Some(receipt) = self.receipts.get(&command.operation_id) else {
            return Ok(None);
        };
        if receipt.item_id != item_id || receipt.command != *command {
            return Err(ItemProgressError::OperationReused);
        }
        debug_assert_eq!(receipt.before.revision + 1, receipt.after.progress.revision);
        Ok(Some(ItemProgressMutation {
            replayed: true,
            ..receipt.after.clone()
        }))
    }

    pub(crate) fn put(
        &mut self,
        item_id: Uuid,
        item_revision: u64,
        command: ItemProgressCommand,
        now: DateTime<Utc>,
    ) -> Result<ItemProgressMutation, ItemProgressError> {
        command.validate(item_id)?;
        let before = self.get(item_id, item_revision);
        let after = ItemProgressMutation {
            operation_id: command.operation_id,
            replayed: false,
            progress: before.replaced(&command, now)?,
        };
        self.snapshots.insert(item_id, after.progress.clone());
        self.receipts.insert(
            command.operation_id,
            Receipt {
                item_id,
                command,
                before,
                after: after.clone(),
            },
        );
        Ok(after)
    }
}
