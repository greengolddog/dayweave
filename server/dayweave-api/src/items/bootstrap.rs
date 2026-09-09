//! A current-state replacement snapshot, not a shortened historical change stream.
use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use super::{DeltaChange, Item, ItemRepositoryError, ItemTombstone};

pub(crate) const MAX_BOOTSTRAP_MEMBERS: usize = 20_000;
pub(crate) const MAX_BOOTSTRAP_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_BOOTSTRAP_PAGE_MEMBERS: usize = 300;
pub(crate) const MAX_BOOTSTRAP_TICKETS: usize = 16;
pub(crate) const BOOTSTRAP_TTL: Duration = Duration::minutes(10);
pub(crate) const BOOTSTRAP_TRASH_RETENTION: Duration = Duration::days(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ItemBootstrapPosition {
    pub snapshot_id: Uuid,
    /// Number of members already delivered; never an item-change sequence.
    pub after: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ItemBootstrapPage {
    pub changes: Vec<DeltaChange>,
    pub head: u64,
    pub continuation: Option<ItemBootstrapPosition>,
}

/// Counts exact compact wire records, and rejects an incomplete/cyclic active forest.
pub(crate) fn validate_members(changes: &[DeltaChange]) -> Result<usize, ItemRepositoryError> {
    if changes.len() > MAX_BOOTSTRAP_MEMBERS {
        return Err(ItemRepositoryError::BootstrapTooLarge);
    }
    let mut ids = HashSet::new();
    let mut parents = HashMap::new();
    let mut bytes = 0_usize;
    for change in changes {
        let payload_bytes = serde_json::to_vec(change)
            .map_err(|_| ItemRepositoryError::Internal)?
            .len();
        if payload_bytes > super::repository::MAX_ITEM_DELTA_PAGE_PAYLOAD_BYTES {
            return Err(ItemRepositoryError::BootstrapTooLarge);
        }
        bytes = bytes
            .checked_add(payload_bytes)
            .ok_or(ItemRepositoryError::BootstrapTooLarge)?;
        if bytes > MAX_BOOTSTRAP_BYTES {
            return Err(ItemRepositoryError::BootstrapTooLarge);
        }
        let id = match change {
            DeltaChange::Upsert { item } => {
                if item.deleted_at.is_some() {
                    return Err(ItemRepositoryError::Internal);
                }
                parents.insert(item.id, item.parent_id);
                item.id
            }
            DeltaChange::Tombstone { tombstone } => tombstone.id,
        };
        if id.is_nil() || !ids.insert(id) {
            return Err(ItemRepositoryError::Internal);
        }
    }
    let mut done = HashSet::new();
    for id in parents.keys().copied() {
        let mut path = HashSet::new();
        let mut current = Some(id);
        while let Some(node) = current {
            if done.contains(&node) {
                break;
            }
            if !path.insert(node) {
                return Err(ItemRepositoryError::Internal);
            }
            current = *parents.get(&node).ok_or(ItemRepositoryError::Internal)?;
        }
        done.extend(path);
    }
    Ok(bytes)
}

pub(crate) fn page_prefix(changes: &[DeltaChange]) -> Result<usize, ItemRepositoryError> {
    let mut bytes = 0;
    let mut count = 0;
    for change in changes.iter().take(MAX_BOOTSTRAP_PAGE_MEMBERS) {
        let size = serde_json::to_vec(change)
            .map_err(|_| ItemRepositoryError::Internal)?
            .len();
        if size > super::repository::MAX_ITEM_DELTA_PAGE_PAYLOAD_BYTES {
            return Err(ItemRepositoryError::BootstrapTooLarge);
        }
        if bytes + size > super::repository::MAX_ITEM_DELTA_PAGE_PAYLOAD_BYTES {
            break;
        }
        bytes += size;
        count += 1;
    }
    Ok(count)
}

#[derive(Clone, Debug)]
pub(crate) struct MemoryBootstrap {
    pub id: Uuid,
    pub head: u64,
    pub expires_at: DateTime<Utc>,
    pub changes: Vec<DeltaChange>,
}

impl MemoryBootstrap {
    pub fn capture(
        items: impl Iterator<Item = Item>,
        head: u64,
        now: DateTime<Utc>,
    ) -> Result<Self, ItemRepositoryError> {
        let cutoff = now - BOOTSTRAP_TRASH_RETENTION;
        let mut members = Vec::new();
        let mut bytes = 0_usize;
        for item in items {
            let previous_count = members.len();
            if let Some(deleted_at) = item.deleted_at {
                if deleted_at >= cutoff {
                    members.push((
                        item.id,
                        DeltaChange::Tombstone {
                            tombstone: ItemTombstone {
                                id: item.id,
                                revision: item.revision,
                                deleted_at,
                                parent_id: item.parent_id,
                            },
                        },
                    ));
                }
            } else {
                members.push((
                    item.id,
                    DeltaChange::Upsert {
                        item: Box::new(item),
                    },
                ));
            }
            if members.len() > MAX_BOOTSTRAP_MEMBERS {
                return Err(ItemRepositoryError::BootstrapTooLarge);
            }
            if members.len() > previous_count {
                let size = serde_json::to_vec(&members.last().expect("new member").1)
                    .map_err(|_| ItemRepositoryError::Internal)?
                    .len();
                bytes = bytes
                    .checked_add(size)
                    .ok_or(ItemRepositoryError::BootstrapTooLarge)?;
                if bytes > MAX_BOOTSTRAP_BYTES
                    || size > super::repository::MAX_ITEM_DELTA_PAGE_PAYLOAD_BYTES
                {
                    return Err(ItemRepositoryError::BootstrapTooLarge);
                }
            }
        }
        members.sort_by_key(|(id, _)| *id);
        let changes = members
            .into_iter()
            .map(|(_, change)| change)
            .collect::<Vec<_>>();
        validate_members(&changes)?;
        Ok(Self {
            id: Uuid::new_v4(),
            head,
            expires_at: now + BOOTSTRAP_TTL,
            changes,
        })
    }

    pub fn page(&self, after: usize) -> Result<ItemBootstrapPage, ItemRepositoryError> {
        if after > self.changes.len() || (after == self.changes.len() && after != 0) {
            return Err(ItemRepositoryError::BootstrapCursorInvalid);
        }
        let count = page_prefix(&self.changes[after..])?;
        let next = after + count;
        Ok(ItemBootstrapPage {
            changes: self.changes[after..next].to_vec(),
            head: self.head,
            continuation: (next < self.changes.len()).then_some(ItemBootstrapPosition {
                snapshot_id: self.id,
                after: next,
            }),
        })
    }
}

#[cfg(test)]
#[path = "bootstrap_tests.rs"]
mod tests;
