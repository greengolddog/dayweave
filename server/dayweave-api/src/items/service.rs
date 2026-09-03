use std::{sync::Arc, time::Duration as StdDuration};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{proposals::Clock, scheduling::truncate_to_postgres_timestamp_precision};

use super::{
    IdempotencyContext, Item, ItemDomainError, ItemMutation, ItemQuery, ItemRepository,
    ItemRepositoryError, NewItem, ReplaceItem,
    invalidation::{
        ItemInvalidationConfig, ItemInvalidationHub, ItemInvalidationOpenError,
        ItemInvalidationStream,
    },
};

const IDEMPOTENCY_TTL: StdDuration = StdDuration::from_hours(24);
const CURSOR_PREFIX: &[u8; 4] = b"DWI1";
const CURSOR_BYTES: usize = 32;
const MAX_CURSOR_TEXT_BYTES: usize = 256;

#[derive(Clone, Debug)]
pub struct IdempotencyKey {
    pub key: String,
    pub fingerprint: [u8; 32],
}

pub struct ItemService {
    repository: Arc<dyn ItemRepository>,
    clock: Arc<dyn Clock>,
    invalidations: ItemInvalidationHub,
}

impl std::fmt::Debug for ItemService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ItemService")
            .finish_non_exhaustive()
    }
}

impl ItemService {
    #[must_use]
    pub fn new(repository: Arc<dyn ItemRepository>, clock: Arc<dyn Clock>) -> Self {
        Self {
            repository,
            clock,
            invalidations: ItemInvalidationHub::new(ItemInvalidationConfig::default()),
        }
    }

    /// Replaces the bounded invalidation stream configuration while assembling
    /// a service. Primarily useful for deterministic embedded/HTTP tests.
    #[must_use]
    pub fn with_invalidation_config(mut self, config: ItemInvalidationConfig) -> Self {
        self.invalidations = ItemInvalidationHub::new(config);
        self
    }

    pub(super) async fn invalidation_stream(
        &self,
        cursor: Option<&str>,
    ) -> Result<ItemInvalidationStream, ItemInvalidationOpenError> {
        let sequence = cursor
            .map_or(Ok(0), |cursor| {
                decode_cursor(cursor, self.repository.cursor_scope())
            })
            .map_err(|_| ItemInvalidationOpenError::InvalidCursor)?;
        self.invalidations
            .open(self.repository.clone(), sequence)
            .await
    }

    /// Creates an item and its optional hierarchy edge atomically.
    ///
    /// # Errors
    ///
    /// Returns a domain, hierarchy, idempotency, or storage error.
    pub async fn create(
        &self,
        input: NewItem,
        idempotency: IdempotencyKey,
    ) -> Result<ItemMutation, ItemServiceError> {
        validate_idempotency_key(&idempotency.key)?;
        let now = truncate_to_postgres_timestamp_precision(self.clock.now());
        let item = Item::new(input, now)?;
        let mutation = self
            .repository
            .create(item, Self::context("items.create", idempotency, now)?)
            .await?;
        self.invalidations.poke();
        Ok(mutation)
    }

    /// Gets one active item.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` or a storage error.
    pub async fn get(&self, id: Uuid) -> Result<Item, ItemServiceError> {
        Ok(self.repository.get(id, false).await?)
    }

    pub(crate) async fn get_including_deleted(&self, id: Uuid) -> Result<Item, ItemServiceError> {
        Ok(self.repository.get(id, true).await?)
    }

    /// Lists items in deterministic sibling order.
    ///
    /// # Errors
    ///
    /// Returns a storage error.
    pub async fn list(&self, query: ItemQuery) -> Result<Vec<Item>, ItemServiceError> {
        Ok(self.repository.list(query).await?)
    }

    /// Replaces all mutable fields using optimistic concurrency.
    ///
    /// # Errors
    ///
    /// Returns a validation, hierarchy, revision, idempotency, or storage error.
    pub async fn replace(
        &self,
        id: Uuid,
        expected_revision: u64,
        replacement: ReplaceItem,
        idempotency: IdempotencyKey,
    ) -> Result<ItemMutation, ItemServiceError> {
        validate_idempotency_key(&idempotency.key)?;
        validate_revision(expected_revision)?;
        let now = truncate_to_postgres_timestamp_precision(self.clock.now());
        let mutation = self
            .repository
            .replace(
                id,
                expected_revision,
                replacement,
                now,
                Self::context("items.replace", idempotency, now)?,
            )
            .await?;
        self.invalidations.poke();
        Ok(mutation)
    }

    /// Soft-deletes a leaf and emits a sync tombstone.
    ///
    /// # Errors
    ///
    /// Returns a revision, hierarchy, idempotency, or storage error.
    pub async fn trash(
        &self,
        id: Uuid,
        expected_revision: u64,
        idempotency: IdempotencyKey,
    ) -> Result<ItemMutation, ItemServiceError> {
        validate_idempotency_key(&idempotency.key)?;
        validate_revision(expected_revision)?;
        let now = truncate_to_postgres_timestamp_precision(self.clock.now());
        let mutation = self
            .repository
            .trash(
                id,
                expected_revision,
                now,
                Self::context("items.delete", idempotency, now)?,
            )
            .await?;
        self.invalidations.poke();
        Ok(mutation)
    }

    /// Restores a soft-deleted item with a new optimistic revision.
    ///
    /// # Errors
    ///
    /// Returns a revision, hierarchy, idempotency, or storage error.
    pub async fn restore(
        &self,
        id: Uuid,
        expected_revision: u64,
        idempotency: IdempotencyKey,
    ) -> Result<ItemMutation, ItemServiceError> {
        validate_idempotency_key(&idempotency.key)?;
        validate_revision(expected_revision)?;
        let now = truncate_to_postgres_timestamp_precision(self.clock.now());
        let mutation = self
            .repository
            .restore(
                id,
                expected_revision,
                now,
                Self::context("items.restore", idempotency, now)?,
            )
            .await?;
        self.invalidations.poke();
        Ok(mutation)
    }

    /// Returns a bounded delta page after an opaque cursor.
    ///
    /// # Errors
    ///
    /// Returns `InvalidCursor` or a storage error.
    pub async fn delta(
        &self,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<EncodedDeltaPage, ItemServiceError> {
        let cursor_scope = self.repository.cursor_scope();
        let after = cursor.map_or(Ok(0), |cursor| decode_cursor(cursor, cursor_scope))?;
        let page = self.repository.delta(after, limit).await?;
        Ok(EncodedDeltaPage {
            next_cursor: encode_cursor(page.watermark, cursor_scope),
            changes: page.changes,
            has_more: page.has_more,
        })
    }

    fn context(
        namespace: &'static str,
        idempotency: IdempotencyKey,
        now: DateTime<Utc>,
    ) -> Result<IdempotencyContext, ItemServiceError> {
        let ttl =
            chrono::Duration::from_std(IDEMPOTENCY_TTL).map_err(|_| ItemServiceError::Internal)?;
        Ok(IdempotencyContext {
            namespace,
            key: idempotency.key,
            fingerprint: idempotency.fingerprint,
            expires_at: now + ttl,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EncodedDeltaPage {
    pub changes: Vec<super::DeltaChange>,
    pub next_cursor: String,
    pub has_more: bool,
}

#[derive(Debug, Error)]
pub enum ItemServiceError {
    #[error(transparent)]
    Domain(#[from] ItemDomainError),
    #[error(transparent)]
    Repository(#[from] ItemRepositoryError),
    #[error("idempotency key must be 8-128 URL-safe ASCII characters")]
    InvalidIdempotencyKey,
    #[error("expected_revision must be positive")]
    InvalidRevision,
    #[error("delta cursor is invalid")]
    InvalidCursor,
    #[error("item service operation failed")]
    Internal,
}

fn validate_idempotency_key(value: &str) -> Result<(), ItemServiceError> {
    if (8..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        Ok(())
    } else {
        Err(ItemServiceError::InvalidIdempotencyKey)
    }
}

fn validate_revision(value: u64) -> Result<(), ItemServiceError> {
    if value == 0 {
        Err(ItemServiceError::InvalidRevision)
    } else {
        Ok(())
    }
}

pub(super) fn encode_cursor(sequence: u64, scope: Uuid) -> String {
    let mut bytes = [0_u8; CURSOR_BYTES];
    bytes[..4].copy_from_slice(CURSOR_PREFIX);
    bytes[4..20].copy_from_slice(scope.as_bytes());
    bytes[20..28].copy_from_slice(&sequence.to_be_bytes());
    let checksum = Sha256::digest(&bytes[..28]);
    bytes[28..].copy_from_slice(&checksum[..4]);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(super) fn decode_cursor(cursor: &str, expected_scope: Uuid) -> Result<u64, ItemServiceError> {
    if cursor.is_empty() || cursor.len() > MAX_CURSOR_TEXT_BYTES {
        return Err(ItemServiceError::InvalidCursor);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| ItemServiceError::InvalidCursor)?;
    if bytes.len() != CURSOR_BYTES || &bytes[..4] != CURSOR_PREFIX {
        return Err(ItemServiceError::InvalidCursor);
    }
    if bytes[4..20] != expected_scope.as_bytes()[..] {
        return Err(ItemServiceError::InvalidCursor);
    }
    let checksum = Sha256::digest(&bytes[..28]);
    if bytes[28..] != checksum[..4] {
        return Err(ItemServiceError::InvalidCursor);
    }
    let sequence: [u8; 8] = bytes[20..28]
        .try_into()
        .map_err(|_| ItemServiceError::InvalidCursor)?;
    let sequence = u64::from_be_bytes(sequence);
    if encode_cursor(sequence, expected_scope) != cursor {
        return Err(ItemServiceError::InvalidCursor);
    }
    Ok(sequence)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;
    use serde_json::json;

    use super::*;

    struct FixedClock(DateTime<Utc>);

    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
    }

    #[test]
    fn cursor_is_opaque_round_trippable_and_tamper_evident() {
        let scope = Uuid::new_v4();
        let cursor = encode_cursor(42, scope);
        assert_eq!(decode_cursor(&cursor, scope).unwrap(), 42);
        assert!(decode_cursor(&cursor, Uuid::new_v4()).is_err());
        let mut bytes = URL_SAFE_NO_PAD.decode(cursor).unwrap();
        bytes[7] ^= 1;
        assert!(decode_cursor(&URL_SAFE_NO_PAD.encode(bytes), scope).is_err());
    }

    #[test]
    fn idempotency_keys_are_bounded_and_header_safe() {
        assert!(validate_idempotency_key("offline-write-123").is_ok());
        assert!(validate_idempotency_key("short").is_err());
        assert!(validate_idempotency_key("contains spaces").is_err());
    }

    #[tokio::test]
    async fn item_mutation_timestamps_are_canonical_microseconds() {
        let raw_now = Utc.timestamp_opt(1_700_000_000, 123_456_789).unwrap();
        let expected_now = Utc.timestamp_opt(1_700_000_000, 123_456_000).unwrap();
        let service = ItemService::new(
            Arc::new(crate::items::InMemoryItemRepository::default()),
            Arc::new(FixedClock(raw_now)),
        );
        let input = serde_json::from_value(json!({
            "id": Uuid::new_v4(),
            "is_sensitive": false,
            "kind": "task",
            "status": "planned",
            "title": "Canonical clock",
            "notes": null,
            "timezone_name": "UTC",
            "duration_seconds": 60,
            "deadline_at": null,
            "earliest_start_at": null,
            "recurrence": null,
            "flexible_constraints": {},
            "split_policy": {"type": "indivisible"},
            "importance": 50,
            "urgency": 50,
            "parent_id": null,
            "sibling_order": 0
        }))
        .unwrap();

        let mutation = service
            .create(
                input,
                IdempotencyKey {
                    key: "clock-test".to_owned(),
                    fingerprint: [7; 32],
                },
            )
            .await
            .unwrap();

        assert_eq!(mutation.item.created_at, expected_now);
        assert_eq!(mutation.item.updated_at, expected_now);
    }
}
