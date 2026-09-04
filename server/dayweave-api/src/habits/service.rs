use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Datelike as _, Days, Duration, NaiveDate, NaiveTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    items::{ItemKind, ItemRepositoryError, ItemService, ItemServiceError},
    proposals::Clock,
    scheduling::truncate_to_postgres_timestamp_precision,
};

use super::{
    HabitAnalytics, HabitAnalyticsBucket, HabitDeltaChange, HabitDomainError, HabitIdempotency,
    HabitMutation, HabitOccurrence, HabitOutcomeCommand, HabitPause, HabitPauseResumeCommand,
    HabitPauseStartCommand, HabitRepository, HabitRepositoryError, MAX_HABIT_DATE_YEAR,
    MIN_HABIT_DATE_YEAR, OutcomeWrite, PauseCreate, PauseResume, calculate_analytics,
    invalidation::{HabitInvalidationHub, HabitInvalidationOpenError, HabitInvalidationStream},
    repository::OccurrencePageCursor,
};

const DELTA_CURSOR_PREFIX: &[u8; 4] = b"DWH1";
const OCCURRENCE_CURSOR_PREFIX: &[u8; 4] = b"DWO1";
const MAX_CURSOR_TEXT_BYTES: usize = 256;
const DELTA_CURSOR_BYTES: usize = 32;
const OCCURRENCE_CURSOR_BYTES: usize = 76;
const MAX_ANALYTICS_OCCURRENCES: usize = 50_000;
pub const DEFAULT_HABIT_PAGE_LIMIT: usize = 100;
pub const MAX_HABIT_PAGE_LIMIT: usize = 200;
pub const MAX_HABIT_RANGE_DAYS: i64 = 366;

#[derive(Clone, Debug)]
pub struct HabitIdempotencyKey {
    pub key: String,
    pub actor_session_id: Option<Uuid>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EncodedOccurrencePage {
    pub occurrences: Vec<HabitOccurrence>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EncodedHabitDeltaPage {
    pub changes: Vec<HabitDeltaChange>,
    pub next_cursor: String,
    pub has_more: bool,
}

pub struct HabitService {
    repository: Arc<dyn HabitRepository>,
    items: Arc<ItemService>,
    clock: Arc<dyn Clock>,
    invalidations: HabitInvalidationHub,
}

impl std::fmt::Debug for HabitService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HabitService")
            .finish_non_exhaustive()
    }
}

impl HabitService {
    #[must_use]
    pub fn new(
        repository: Arc<dyn HabitRepository>,
        items: Arc<ItemService>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            repository,
            items,
            clock,
            invalidations: HabitInvalidationHub::new(),
        }
    }

    pub(super) async fn invalidation_stream(
        &self,
        cursor: Option<&str>,
    ) -> Result<HabitInvalidationStream, HabitServiceError> {
        let sequence = cursor.map_or(Ok(0), |value| {
            decode_delta_cursor(value, self.repository.cursor_scope())
        })?;
        self.invalidations
            .open(self.repository.clone(), sequence)
            .await
            .map_err(|error| match error {
                HabitInvalidationOpenError::CursorAhead => HabitServiceError::CursorAhead,
                HabitInvalidationOpenError::Capacity => HabitServiceError::StreamCapacity,
                HabitInvalidationOpenError::Repository(error) => {
                    HabitServiceError::Repository(error)
                }
            })
    }

    /// Creates or corrects an outcome for one publisher-admitted occurrence.
    ///
    /// # Errors
    ///
    /// Returns a validation, authorization-bound repository, idempotency, or
    /// optimistic revision error without partially advancing the ledger.
    pub async fn put_outcome(
        &self,
        habit_id: Uuid,
        occurrence_id: Uuid,
        command: HabitOutcomeCommand,
        key: HabitIdempotencyKey,
    ) -> Result<HabitMutation<HabitOccurrence>, HabitServiceError> {
        let now = truncate_to_postgres_timestamp_precision(self.clock.now());
        validate_ids(&[habit_id, occurrence_id, command.operation_id])?;
        let idempotency = make_idempotency(
            "habits.outcome.put",
            &key,
            command.operation_id,
            &(habit_id, occurrence_id, &command),
        )?;
        if let Some(value) = self.repository.replay_outcome(&idempotency).await? {
            return Ok(HabitMutation {
                value,
                replayed: true,
            });
        }
        command.outcome.validate(now)?;
        self.verify_habit(habit_id).await?;
        let mutation = self
            .repository
            .put_outcome(OutcomeWrite {
                habit_id,
                occurrence_id,
                expected_revision: command.expected_revision,
                outcome: command.outcome,
                recorded_at: now,
                idempotency,
            })
            .await?;
        if !mutation.replayed {
            self.invalidations.poke();
        }
        Ok(mutation)
    }

    /// Opens one durable habit pause using the habit's persisted streak policy.
    ///
    /// # Errors
    ///
    /// Returns a validation, idempotency, repository, or open-pause conflict.
    pub async fn create_pause(
        &self,
        habit_id: Uuid,
        command: HabitPauseStartCommand,
        key: HabitIdempotencyKey,
    ) -> Result<HabitMutation<HabitPause>, HabitServiceError> {
        let now = truncate_to_postgres_timestamp_precision(self.clock.now());
        validate_ids(&[habit_id, command.pause_id, command.operation_id])?;
        let idempotency = make_idempotency(
            "habits.pause.create",
            &key,
            command.operation_id,
            &(habit_id, &command),
        )?;
        if let Some(value) = self.repository.replay_pause(&idempotency).await? {
            return Ok(HabitMutation {
                value,
                replayed: true,
            });
        }
        if command.expected_revision != 0 {
            return Err(HabitServiceError::InvalidCreateRevision);
        }
        validate_mutation_time(command.started_at, now)?;
        let item = self.verify_habit(habit_id).await?;
        let preserves_streak = item
            .flexible_constraints
            .get("preserves_streak_when_paused")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let mutation = self
            .repository
            .create_pause(PauseCreate {
                id: command.pause_id,
                habit_id,
                expected_revision: command.expected_revision,
                started_at: command.started_at,
                preserves_streak,
                recorded_at: now,
                idempotency,
            })
            .await?;
        if !mutation.replayed {
            self.invalidations.poke();
        }
        Ok(mutation)
    }

    /// Closes an exact open pause revision.
    ///
    /// # Errors
    ///
    /// Returns a validation, idempotency, repository, or optimistic revision error.
    pub async fn resume_pause(
        &self,
        habit_id: Uuid,
        pause_id: Uuid,
        command: HabitPauseResumeCommand,
        key: HabitIdempotencyKey,
    ) -> Result<HabitMutation<HabitPause>, HabitServiceError> {
        let now = truncate_to_postgres_timestamp_precision(self.clock.now());
        validate_ids(&[habit_id, pause_id, command.operation_id])?;
        let idempotency = make_idempotency(
            "habits.pause.resume",
            &key,
            command.operation_id,
            &(habit_id, pause_id, &command),
        )?;
        if let Some(value) = self.repository.replay_pause(&idempotency).await? {
            return Ok(HabitMutation {
                value,
                replayed: true,
            });
        }
        if command.expected_revision == 0 {
            return Err(HabitServiceError::InvalidCorrectionRevision);
        }
        validate_mutation_time(command.ended_at, now)?;
        self.verify_habit(habit_id).await?;
        let mutation = self
            .repository
            .resume_pause(PauseResume {
                id: pause_id,
                habit_id,
                expected_revision: command.expected_revision,
                ended_at: command.ended_at,
                recorded_at: now,
                idempotency,
            })
            .await?;
        if !mutation.replayed {
            self.invalidations.poke();
        }
        Ok(mutation)
    }

    /// Lists an exact bounded local-date page of authoritative occurrences.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid habit/range/limit/cursor or unavailable storage.
    pub async fn list_occurrences(
        &self,
        habit_id: Uuid,
        start_date: NaiveDate,
        end_date: NaiveDate,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<EncodedOccurrencePage, HabitServiceError> {
        self.verify_habit(habit_id).await?;
        validate_range(start_date, end_date)?;
        validate_limit(limit)?;
        let after = cursor
            .map(|value| {
                decode_occurrence_cursor(
                    value,
                    self.repository.cursor_scope(),
                    habit_id,
                    start_date,
                    end_date,
                )
            })
            .transpose()?;
        let (occurrences, has_more) = self
            .repository
            .list_occurrences(habit_id, start_date, end_date, after, limit)
            .await?;
        let next_cursor = if has_more {
            occurrences.last().map(|last| {
                encode_occurrence_cursor(
                    self.repository.cursor_scope(),
                    habit_id,
                    start_date,
                    end_date,
                    OccurrencePageCursor {
                        local_date: last.evidence.local_date,
                        nominal_start: last.evidence.nominal_start,
                        id: last.evidence.id,
                    },
                )
            })
        } else {
            None
        };
        Ok(EncodedOccurrencePage {
            occurrences,
            next_cursor,
            has_more,
        })
    }

    /// Returns a bounded workspace-scoped delta page from an opaque cursor.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid limit/cursor or unavailable storage.
    pub async fn delta(
        &self,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<EncodedHabitDeltaPage, HabitServiceError> {
        validate_limit(limit)?;
        let scope = self.repository.cursor_scope();
        let after = cursor.map_or(Ok(0), |value| decode_delta_cursor(value, scope))?;
        let page = self.repository.delta(after, limit).await?;
        Ok(EncodedHabitDeltaPage {
            changes: page.changes,
            next_cursor: encode_delta_cursor(page.watermark, scope),
            has_more: page.has_more,
        })
    }

    /// Computes deterministic aggregate and trend analytics without returning notes.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid habit/range, excessive occurrence count,
    /// inconsistent cursor progress, or unavailable storage.
    pub async fn analytics(
        &self,
        habit_id: Uuid,
        start_date: NaiveDate,
        end_date: NaiveDate,
        bucket: HabitAnalyticsBucket,
    ) -> Result<HabitAnalytics, HabitServiceError> {
        self.verify_habit(habit_id).await?;
        validate_range(start_date, end_date)?;
        let mut occurrences = Vec::new();
        let mut after = None;
        loop {
            let (page, has_more) = self
                .repository
                .list_occurrences(habit_id, start_date, end_date, after, MAX_HABIT_PAGE_LIMIT)
                .await?;
            if occurrences.len().saturating_add(page.len()) > MAX_ANALYTICS_OCCURRENCES {
                return Err(HabitServiceError::AnalyticsTooLarge);
            }
            after = page.last().map(|last| OccurrencePageCursor {
                local_date: last.evidence.local_date,
                nominal_start: last.evidence.nominal_start,
                id: last.evidence.id,
            });
            occurrences.extend(page);
            if !has_more {
                break;
            }
        }
        let range_start = start_date.and_time(NaiveTime::MIN).and_utc();
        let range_end = end_date
            .checked_add_days(Days::new(1))
            .ok_or(HabitServiceError::InvalidDateRange)?
            .and_time(NaiveTime::MIN)
            .and_utc();
        let (pause_start, pause_end) =
            occurrences
                .iter()
                .fold((range_start, range_end), |(start, end), occurrence| {
                    (
                        start.min(occurrence.evidence.window_start),
                        end.max(occurrence.evidence.window_end),
                    )
                });
        let pauses = self
            .repository
            .list_pauses(habit_id, pause_start, pause_end)
            .await?;
        Ok(calculate_analytics(
            habit_id,
            &occurrences,
            &pauses,
            start_date,
            end_date,
            bucket,
            truncate_to_postgres_timestamp_precision(self.clock.now()),
        ))
    }

    async fn verify_habit(&self, habit_id: Uuid) -> Result<crate::items::Item, HabitServiceError> {
        if habit_id.is_nil() {
            return Err(HabitServiceError::InvalidIdentifier);
        }
        let item = self
            .items
            .get(habit_id)
            .await
            .map_err(|error| match error {
                ItemServiceError::Repository(ItemRepositoryError::NotFound(_)) => {
                    HabitServiceError::Repository(HabitRepositoryError::HabitNotFound(habit_id))
                }
                other => HabitServiceError::Items(other),
            })?;
        if item.kind != ItemKind::Habit || item.recurrence.is_none() {
            return Err(HabitServiceError::Repository(
                HabitRepositoryError::NotHabit(habit_id),
            ));
        }
        Ok(item)
    }
}

fn validate_ids(ids: &[Uuid]) -> Result<(), HabitServiceError> {
    if ids.iter().any(Uuid::is_nil) {
        Err(HabitServiceError::InvalidIdentifier)
    } else {
        Ok(())
    }
}

fn validate_mutation_time(
    value: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), HabitServiceError> {
    if !value.timestamp_subsec_nanos().is_multiple_of(1_000)
        || value > now + Duration::minutes(5)
        || value < now - Duration::days(366 * 20)
    {
        Err(HabitServiceError::InvalidMutationTime)
    } else {
        Ok(())
    }
}

fn validate_range(start: NaiveDate, end: NaiveDate) -> Result<(), HabitServiceError> {
    let days = (end - start).num_days().checked_add(1);
    if start.year() < MIN_HABIT_DATE_YEAR
        || end.year() > MAX_HABIT_DATE_YEAR
        || !days.is_some_and(|days| (1..=MAX_HABIT_RANGE_DAYS).contains(&days))
    {
        return Err(HabitServiceError::InvalidDateRange);
    }
    Ok(())
}

fn validate_limit(limit: usize) -> Result<(), HabitServiceError> {
    if !(1..=MAX_HABIT_PAGE_LIMIT).contains(&limit) {
        return Err(HabitServiceError::InvalidLimit);
    }
    Ok(())
}

fn make_idempotency<T: Serialize>(
    namespace: &'static str,
    key: &HabitIdempotencyKey,
    operation_id: Uuid,
    body: &T,
) -> Result<HabitIdempotency, HabitServiceError> {
    if !(8..=128).contains(&key.key.len())
        || !key
            .key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(HabitServiceError::InvalidIdempotencyKey);
    }
    let body = serde_json::to_vec(body).map_err(|_| HabitServiceError::Internal)?;
    let mut fingerprint = Sha256::new();
    fingerprint.update(namespace.as_bytes());
    fingerprint.update([0]);
    fingerprint.update(body);
    Ok(HabitIdempotency {
        namespace,
        key_hash: Sha256::digest(key.key.as_bytes()).into(),
        request_fingerprint: fingerprint.finalize().into(),
        operation_id,
        actor_session_id: key.actor_session_id,
    })
}

pub(super) fn encode_delta_cursor(sequence: u64, scope: Uuid) -> String {
    let mut bytes = [0_u8; DELTA_CURSOR_BYTES];
    bytes[..4].copy_from_slice(DELTA_CURSOR_PREFIX);
    bytes[4..20].copy_from_slice(scope.as_bytes());
    bytes[20..28].copy_from_slice(&sequence.to_be_bytes());
    let checksum = Sha256::digest(&bytes[..28]);
    bytes[28..].copy_from_slice(&checksum[..4]);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn decode_delta_cursor(value: &str, scope: Uuid) -> Result<u64, HabitServiceError> {
    if value.is_empty() || value.len() > MAX_CURSOR_TEXT_BYTES {
        return Err(HabitServiceError::InvalidCursor);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| HabitServiceError::InvalidCursor)?;
    if bytes.len() != DELTA_CURSOR_BYTES
        || &bytes[..4] != DELTA_CURSOR_PREFIX
        || &bytes[4..20] != scope.as_bytes()
        || Sha256::digest(&bytes[..28])[..4] != bytes[28..]
    {
        return Err(HabitServiceError::InvalidCursor);
    }
    let sequence = u64::from_be_bytes(
        bytes[20..28]
            .try_into()
            .map_err(|_| HabitServiceError::InvalidCursor)?,
    );
    if encode_delta_cursor(sequence, scope) != value {
        return Err(HabitServiceError::InvalidCursor);
    }
    Ok(sequence)
}

fn encode_occurrence_cursor(
    scope: Uuid,
    habit_id: Uuid,
    start_date: NaiveDate,
    end_date: NaiveDate,
    cursor: OccurrencePageCursor,
) -> String {
    let mut bytes = [0_u8; OCCURRENCE_CURSOR_BYTES];
    bytes[..4].copy_from_slice(OCCURRENCE_CURSOR_PREFIX);
    bytes[4..20].copy_from_slice(scope.as_bytes());
    bytes[20..36].copy_from_slice(habit_id.as_bytes());
    bytes[36..40].copy_from_slice(&start_date.num_days_from_ce().to_be_bytes());
    bytes[40..44].copy_from_slice(&end_date.num_days_from_ce().to_be_bytes());
    bytes[44..48].copy_from_slice(&cursor.local_date.num_days_from_ce().to_be_bytes());
    bytes[48..56].copy_from_slice(&cursor.nominal_start.timestamp_micros().to_be_bytes());
    bytes[56..72].copy_from_slice(cursor.id.as_bytes());
    let checksum = Sha256::digest(&bytes[..72]);
    bytes[72..].copy_from_slice(&checksum[..4]);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn decode_occurrence_cursor(
    value: &str,
    scope: Uuid,
    habit_id: Uuid,
    start_date: NaiveDate,
    end_date: NaiveDate,
) -> Result<OccurrencePageCursor, HabitServiceError> {
    if value.is_empty() || value.len() > MAX_CURSOR_TEXT_BYTES {
        return Err(HabitServiceError::InvalidCursor);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| HabitServiceError::InvalidCursor)?;
    if bytes.len() != OCCURRENCE_CURSOR_BYTES
        || &bytes[..4] != OCCURRENCE_CURSOR_PREFIX
        || &bytes[4..20] != scope.as_bytes()
        || &bytes[20..36] != habit_id.as_bytes()
        || Sha256::digest(&bytes[..72])[..4] != bytes[72..]
    {
        return Err(HabitServiceError::InvalidCursor);
    }
    let read_i32 = |range: std::ops::Range<usize>| {
        bytes[range]
            .try_into()
            .map(i32::from_be_bytes)
            .map_err(|_| HabitServiceError::InvalidCursor)
    };
    if read_i32(36..40)? != start_date.num_days_from_ce()
        || read_i32(40..44)? != end_date.num_days_from_ce()
    {
        return Err(HabitServiceError::InvalidCursor);
    }
    let local_date = NaiveDate::from_num_days_from_ce_opt(read_i32(44..48)?)
        .ok_or(HabitServiceError::InvalidCursor)?;
    let micros = i64::from_be_bytes(
        bytes[48..56]
            .try_into()
            .map_err(|_| HabitServiceError::InvalidCursor)?,
    );
    let nominal_start =
        DateTime::from_timestamp_micros(micros).ok_or(HabitServiceError::InvalidCursor)?;
    let id = Uuid::from_slice(&bytes[56..72]).map_err(|_| HabitServiceError::InvalidCursor)?;
    let cursor = OccurrencePageCursor {
        local_date,
        nominal_start,
        id,
    };
    if encode_occurrence_cursor(scope, habit_id, start_date, end_date, cursor) != value {
        return Err(HabitServiceError::InvalidCursor);
    }
    Ok(cursor)
}

#[derive(Debug, Error)]
pub enum HabitServiceError {
    #[error(transparent)]
    Domain(#[from] HabitDomainError),
    #[error(transparent)]
    Repository(#[from] HabitRepositoryError),
    #[error(transparent)]
    Items(ItemServiceError),
    #[error("habit identifier must not be nil")]
    InvalidIdentifier,
    #[error("Idempotency-Key must be 8-128 URL-safe ASCII characters")]
    InvalidIdempotencyKey,
    #[error("creation expected_revision must be zero")]
    InvalidCreateRevision,
    #[error("correction expected_revision must be positive")]
    InvalidCorrectionRevision,
    #[error("timestamp is outside the supported precision or time range")]
    InvalidMutationTime,
    #[error("date range must be within 1900-2200, ordered, and no longer than 366 days")]
    InvalidDateRange,
    #[error("limit must be between 1 and 200")]
    InvalidLimit,
    #[error("cursor is invalid or belongs to another query/workspace")]
    InvalidCursor,
    #[error("stream cursor is ahead of authoritative state")]
    CursorAhead,
    #[error("habit stream capacity is exhausted")]
    StreamCapacity,
    #[error("analytics occurrence bound was exceeded")]
    AnalyticsTooLarge,
    #[error("habit service operation failed")]
    Internal,
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Utc};
    use uuid::Uuid;

    use super::{
        HabitServiceError, OccurrencePageCursor, decode_delta_cursor, decode_occurrence_cursor,
        encode_delta_cursor, encode_occurrence_cursor,
    };

    #[test]
    fn cursors_are_canonical_tamper_evident_and_bound_to_workspace_and_query() {
        let scope = Uuid::from_u128(1);
        let habit_id = Uuid::from_u128(2);
        let start = chrono::NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
        let end = chrono::NaiveDate::from_ymd_opt(2026, 9, 7).unwrap();
        let cursor = OccurrencePageCursor {
            local_date: chrono::NaiveDate::from_ymd_opt(2026, 9, 4).unwrap(),
            nominal_start: Utc.with_ymd_and_hms(2026, 9, 4, 8, 30, 0).unwrap(),
            id: Uuid::from_u128(3),
        };
        let encoded = encode_occurrence_cursor(scope, habit_id, start, end, cursor);
        assert_eq!(
            decode_occurrence_cursor(&encoded, scope, habit_id, start, end).unwrap(),
            cursor
        );
        assert!(matches!(
            decode_occurrence_cursor(&encoded, Uuid::from_u128(4), habit_id, start, end),
            Err(HabitServiceError::InvalidCursor)
        ));
        assert!(matches!(
            decode_occurrence_cursor(&encoded, scope, Uuid::from_u128(5), start, end),
            Err(HabitServiceError::InvalidCursor)
        ));
        assert!(matches!(
            decode_occurrence_cursor(&encoded, scope, habit_id, start, end.succ_opt().unwrap(),),
            Err(HabitServiceError::InvalidCursor)
        ));

        let mut tampered = encoded.into_bytes();
        let last = tampered.last_mut().expect("nonempty cursor");
        *last = if *last == b'A' { b'B' } else { b'A' };
        assert!(matches!(
            decode_occurrence_cursor(
                std::str::from_utf8(&tampered).unwrap(),
                scope,
                habit_id,
                start,
                end,
            ),
            Err(HabitServiceError::InvalidCursor)
        ));

        let delta = encode_delta_cursor(42, scope);
        assert_eq!(decode_delta_cursor(&delta, scope).unwrap(), 42);
        assert!(matches!(
            decode_delta_cursor(&delta, Uuid::from_u128(6)),
            Err(HabitServiceError::InvalidCursor)
        ));
    }
}
