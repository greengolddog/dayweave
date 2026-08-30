use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::scheduling::has_postgres_timestamp_precision;

pub const MAX_PAUSE_SECONDS: u32 = 24 * 60 * 60;
pub const MAX_DEFER_SECONDS: u32 = 24 * 60 * 60;
pub const MAX_ACTUAL_SECONDS: u64 = i64::MAX as u64;
const MAX_REASON_CHARS: usize = 500;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Active,
    Paused,
    Completed,
    Skipped,
    Deferred,
}

impl ExecutionStatus {
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Active | Self::Paused)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ExecutionSession {
    pub id: Uuid,
    pub item_id: Uuid,
    pub item_revision: u64,
    pub occurrence_id: Option<Uuid>,
    pub session_index: u16,
    pub planned_block_id: Option<Uuid>,
    pub source_device_id: Uuid,
    pub status: ExecutionStatus,
    pub revision: u64,
    pub accumulated_seconds: u64,
    pub actual_seconds: Option<u64>,
    pub started_at: DateTime<Utc>,
    pub running_since: Option<DateTime<Utc>>,
    #[serde(skip)]
    #[schema(ignore)]
    pub(crate) observed_running_since: Option<DateTime<Utc>>,
    pub paused_at: Option<DateTime<Utc>>,
    pub pause_until: Option<DateTime<Utc>>,
    pub pause_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_start: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_end: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionSessionWire {
    id: Uuid,
    item_id: Uuid,
    item_revision: u64,
    occurrence_id: Option<Uuid>,
    session_index: u16,
    planned_block_id: Option<Uuid>,
    source_device_id: Uuid,
    status: ExecutionStatus,
    revision: u64,
    accumulated_seconds: u64,
    actual_seconds: Option<u64>,
    started_at: DateTime<Utc>,
    running_since: Option<DateTime<Utc>>,
    paused_at: Option<DateTime<Utc>>,
    pause_until: Option<DateTime<Utc>>,
    pause_reason: Option<String>,
    move_start: Option<DateTime<Utc>>,
    move_end: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<ExecutionSessionWire> for ExecutionSession {
    type Error = ExecutionDomainError;

    fn try_from(wire: ExecutionSessionWire) -> Result<Self, Self::Error> {
        match (wire.status, wire.move_start, wire.move_end) {
            (ExecutionStatus::Deferred, Some(start), Some(end))
                if wire.ended_at == Some(wire.updated_at)
                    && start > wire.updated_at
                    && start < end
                    && has_postgres_timestamp_precision(start)
                    && has_postgres_timestamp_precision(end)
                    && end.signed_duration_since(start)
                        <= chrono::Duration::seconds(i64::from(MAX_DEFER_SECONDS)) => {}
            (ExecutionStatus::Deferred, _, _) => return Err(ExecutionDomainError::InvalidDefer),
            (_, None, None) => {}
            (_, _, _) => return Err(ExecutionDomainError::InvalidDefer),
        }
        Ok(Self {
            id: wire.id,
            item_id: wire.item_id,
            item_revision: wire.item_revision,
            occurrence_id: wire.occurrence_id,
            session_index: wire.session_index,
            planned_block_id: wire.planned_block_id,
            source_device_id: wire.source_device_id,
            status: wire.status,
            revision: wire.revision,
            accumulated_seconds: wire.accumulated_seconds,
            actual_seconds: wire.actual_seconds,
            started_at: wire.started_at,
            running_since: wire.running_since,
            observed_running_since: wire.running_since,
            paused_at: wire.paused_at,
            pause_until: wire.pause_until,
            pause_reason: wire.pause_reason,
            move_start: wire.move_start,
            move_end: wire.move_end,
            ended_at: wire.ended_at,
            created_at: wire.created_at,
            updated_at: wire.updated_at,
        })
    }
}

impl<'de> Deserialize<'de> for ExecutionSession {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ExecutionSessionWire::deserialize(deserializer)?;
        Self::try_from(wire).map_err(serde::de::Error::custom)
    }
}

impl ExecutionSession {
    #[cfg(test)]
    pub(crate) fn start(input: &StartExecution, now: DateTime<Utc>) -> Self {
        Self::start_with_protocol_time(input, now, now)
    }

    pub(crate) fn start_with_protocol_time(
        input: &StartExecution,
        transition_at: DateTime<Utc>,
        observed_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: input.session_id,
            item_id: input.item_id,
            item_revision: input.item_revision,
            occurrence_id: input.occurrence_id,
            session_index: input.session_index,
            planned_block_id: input.planned_block_id,
            source_device_id: input.device_id,
            status: ExecutionStatus::Active,
            revision: 1,
            accumulated_seconds: 0,
            actual_seconds: None,
            started_at: transition_at,
            running_since: Some(transition_at),
            observed_running_since: Some(observed_at),
            paused_at: None,
            pause_until: None,
            pause_reason: None,
            move_start: None,
            move_end: None,
            ended_at: None,
            created_at: transition_at,
            updated_at: transition_at,
        }
    }

    #[cfg(test)]
    pub(crate) fn apply(
        &self,
        command: &ExecutionCommand,
        now: DateTime<Utc>,
    ) -> Result<Self, ExecutionDomainError> {
        let transition_at = now.max(self.updated_at);
        self.apply_with_protocol_time(command, transition_at, transition_at)
    }

    pub(crate) fn apply_with_protocol_time(
        &self,
        command: &ExecutionCommand,
        transition_at: DateTime<Utc>,
        observed_at: DateTime<Utc>,
    ) -> Result<Self, ExecutionDomainError> {
        // The persisted workspace protocol clock can run ahead after wall-clock rollback. Keep it
        // causal for history ordering without charging that synthetic gap to the running timer.
        if transition_at < self.updated_at || transition_at < observed_at {
            return Err(ExecutionDomainError::InvalidTransition);
        }
        let elapsed_at = observed_at;
        match command {
            ExecutionCommand::Start(_) => Err(ExecutionDomainError::InvalidTransition),
            ExecutionCommand::Pause(input) => self.pause(input, transition_at, elapsed_at),
            ExecutionCommand::Resume(input) => self.resume(input, transition_at, observed_at),
            ExecutionCommand::Complete(input) => self.finish(
                input.session_id,
                input.actual_seconds,
                ExecutionStatus::Completed,
                transition_at,
                elapsed_at,
            ),
            ExecutionCommand::Skip(input) => self.finish(
                input.session_id,
                input.actual_seconds,
                ExecutionStatus::Skipped,
                transition_at,
                elapsed_at,
            ),
            ExecutionCommand::Defer(input) => self.defer(input, transition_at, elapsed_at),
        }
    }

    fn pause(
        &self,
        input: &PauseExecution,
        transition_at: DateTime<Utc>,
        elapsed_at: DateTime<Utc>,
    ) -> Result<Self, ExecutionDomainError> {
        if self.id != input.session_id || !self.status.is_open() {
            return Err(ExecutionDomainError::InvalidTransition);
        }
        validate_reason(input.reason.as_deref())?;
        let pause_until = match (input.duration_seconds, input.pause_until) {
            (Some(seconds), None) if (1..=MAX_PAUSE_SECONDS).contains(&seconds) => {
                Some(transition_at + chrono::Duration::seconds(i64::from(seconds)))
            }
            (None, Some(until)) => {
                let maximum =
                    transition_at + chrono::Duration::seconds(i64::from(MAX_PAUSE_SECONDS));
                if !has_postgres_timestamp_precision(until)
                    || until <= transition_at
                    || until > maximum
                {
                    return Err(ExecutionDomainError::InvalidPause);
                }
                Some(until)
            }
            (None, None) => None,
            _ => {
                return Err(ExecutionDomainError::InvalidPause);
            }
        };
        let accumulated_seconds = self.elapsed_seconds(elapsed_at)?;
        Ok(Self {
            status: ExecutionStatus::Paused,
            revision: next_revision(self.revision)?,
            accumulated_seconds,
            running_since: None,
            observed_running_since: None,
            paused_at: self.paused_at.or(Some(transition_at)),
            pause_until,
            pause_reason: input.reason.clone().or_else(|| self.pause_reason.clone()),
            updated_at: transition_at,
            ..self.clone()
        })
    }

    fn resume(
        &self,
        input: &ResumeExecution,
        transition_at: DateTime<Utc>,
        observed_at: DateTime<Utc>,
    ) -> Result<Self, ExecutionDomainError> {
        if self.id != input.session_id || self.status != ExecutionStatus::Paused {
            return Err(ExecutionDomainError::InvalidTransition);
        }
        Ok(Self {
            status: ExecutionStatus::Active,
            revision: next_revision(self.revision)?,
            running_since: Some(transition_at),
            observed_running_since: Some(observed_at),
            paused_at: None,
            pause_until: None,
            pause_reason: None,
            updated_at: transition_at,
            ..self.clone()
        })
    }

    fn finish(
        &self,
        session_id: Uuid,
        corrected_actual_seconds: Option<u64>,
        status: ExecutionStatus,
        transition_at: DateTime<Utc>,
        elapsed_at: DateTime<Utc>,
    ) -> Result<Self, ExecutionDomainError> {
        if self.id != session_id || !self.status.is_open() || status.is_open() {
            return Err(ExecutionDomainError::InvalidTransition);
        }
        let elapsed = self.elapsed_seconds(elapsed_at)?;
        let actual_seconds = corrected_actual_seconds.unwrap_or(elapsed);
        if actual_seconds > MAX_ACTUAL_SECONDS {
            return Err(ExecutionDomainError::InvalidActualDuration);
        }
        Ok(Self {
            status,
            revision: next_revision(self.revision)?,
            accumulated_seconds: elapsed,
            actual_seconds: Some(actual_seconds),
            running_since: None,
            observed_running_since: None,
            paused_at: self
                .paused_at
                .or((self.status == ExecutionStatus::Paused).then_some(transition_at)),
            pause_until: None,
            pause_reason: None,
            ended_at: Some(transition_at),
            updated_at: transition_at,
            ..self.clone()
        })
    }

    fn defer(
        &self,
        input: &DeferExecution,
        transition_at: DateTime<Utc>,
        elapsed_at: DateTime<Utc>,
    ) -> Result<Self, ExecutionDomainError> {
        if self.status != ExecutionStatus::Paused {
            return Err(ExecutionDomainError::InvalidTransition);
        }
        validate_defer(input, transition_at)?;
        let mut deferred = self.finish(
            input.session_id,
            input.actual_seconds,
            ExecutionStatus::Deferred,
            transition_at,
            elapsed_at,
        )?;
        deferred.move_start = Some(input.move_start);
        deferred.move_end = Some(input.move_end);
        Ok(deferred)
    }

    fn elapsed_seconds(&self, now: DateTime<Utc>) -> Result<u64, ExecutionDomainError> {
        let running = self.observed_running_since.map_or(Ok(0), |started| {
            let seconds = now.signed_duration_since(started).num_seconds().max(0);
            u64::try_from(seconds).map_err(|_| ExecutionDomainError::DurationOverflow)
        })?;
        let total = self
            .accumulated_seconds
            .checked_add(running)
            .unwrap_or(MAX_ACTUAL_SECONDS)
            .min(MAX_ACTUAL_SECONDS);
        Ok(total)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionCommand {
    Start(StartExecution),
    Pause(PauseExecution),
    Resume(ResumeExecution),
    Complete(FinishExecution),
    Skip(FinishExecution),
    Defer(DeferExecution),
}

impl ExecutionCommand {
    #[must_use]
    pub const fn session_id(&self) -> Uuid {
        match self {
            Self::Start(input) => input.session_id,
            Self::Pause(input) => input.session_id,
            Self::Resume(input) => input.session_id,
            Self::Complete(input) | Self::Skip(input) => input.session_id,
            Self::Defer(input) => input.session_id,
        }
    }

    pub(crate) fn validate(&self, now: DateTime<Utc>) -> Result<(), ExecutionDomainError> {
        match self {
            Self::Start(input) => input.validate(),
            Self::Pause(input) => {
                reject_nil(input.session_id)?;
                validate_reason(input.reason.as_deref())?;
                match (input.duration_seconds, input.pause_until) {
                    (Some(seconds), None) if (1..=MAX_PAUSE_SECONDS).contains(&seconds) => Ok(()),
                    (None, Some(until))
                        if has_postgres_timestamp_precision(until)
                            && until > now
                            && until
                                <= now
                                    + chrono::Duration::seconds(i64::from(MAX_PAUSE_SECONDS)) =>
                    {
                        Ok(())
                    }
                    (None, None) => Ok(()),
                    _ => Err(ExecutionDomainError::InvalidPause),
                }
            }
            Self::Resume(input) => reject_nil(input.session_id),
            Self::Complete(input) | Self::Skip(input) => {
                reject_nil(input.session_id)?;
                if input
                    .actual_seconds
                    .is_some_and(|seconds| seconds > MAX_ACTUAL_SECONDS)
                {
                    Err(ExecutionDomainError::InvalidActualDuration)
                } else {
                    Ok(())
                }
            }
            Self::Defer(input) => validate_defer(input, now),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StartExecution {
    pub session_id: Uuid,
    pub item_id: Uuid,
    pub item_revision: u64,
    pub occurrence_id: Option<Uuid>,
    pub session_index: u16,
    pub planned_block_id: Option<Uuid>,
    pub device_id: Uuid,
}

impl StartExecution {
    fn validate(&self) -> Result<(), ExecutionDomainError> {
        reject_nil(self.session_id)?;
        reject_nil(self.item_id)?;
        reject_nil(self.device_id)?;
        self.occurrence_id.map_or(Ok(()), reject_nil)?;
        self.planned_block_id.map_or(Ok(()), reject_nil)?;
        if self.item_revision == 0 {
            return Err(ExecutionDomainError::InvalidItemRevision);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PauseExecution {
    pub session_id: Uuid,
    pub duration_seconds: Option<u32>,
    pub pause_until: Option<DateTime<Utc>>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ResumeExecution {
    pub session_id: Uuid,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FinishExecution {
    pub session_id: Uuid,
    pub actual_seconds: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DeferExecution {
    pub session_id: Uuid,
    pub move_start: DateTime<Utc>,
    pub move_end: DateTime<Utc>,
    /// Exact actual duration returned by the durable defer assessment.
    #[schema(required, nullable = false)]
    pub actual_seconds: Option<u64>,
    /// Exact canonical digest returned by the durable defer assessment.
    #[schema(required, nullable = false)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assessment_digest: Option<String>,
    /// The same digest when the assessment reports conflicts; otherwise omit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_assessment_digest: Option<String>,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ExecutionDomainError {
    #[error("execution identifiers must not be nil")]
    NilIdentifier,
    #[error("item revision must be positive")]
    InvalidItemRevision,
    #[error("pause duration, end, or reason is invalid")]
    InvalidPause,
    #[error("pause reason exceeds 500 characters or is blank")]
    InvalidPauseReason,
    #[error("actual duration is outside the supported range")]
    InvalidActualDuration,
    #[error(
        "deferred move window must use microsecond precision, start in the future, and span no more than 24 hours"
    )]
    InvalidDefer,
    #[error("defer assessment digest or approval is malformed or mismatched")]
    InvalidDeferAssessment,
    #[error("execution command does not match the active session state")]
    InvalidTransition,
    #[error("execution revision or duration exceeded the supported range")]
    DurationOverflow,
}

fn validate_reason(reason: Option<&str>) -> Result<(), ExecutionDomainError> {
    if reason
        .is_some_and(|value| value.trim().is_empty() || value.chars().count() > MAX_REASON_CHARS)
    {
        Err(ExecutionDomainError::InvalidPauseReason)
    } else {
        Ok(())
    }
}

fn validate_defer(input: &DeferExecution, now: DateTime<Utc>) -> Result<(), ExecutionDomainError> {
    reject_nil(input.session_id)?;
    if input
        .actual_seconds
        .is_some_and(|seconds| seconds > MAX_ACTUAL_SECONDS)
    {
        return Err(ExecutionDomainError::InvalidActualDuration);
    }
    if input
        .assessment_digest
        .as_deref()
        .is_some_and(|digest| !is_canonical_sha256_digest(digest))
        || input
            .approved_assessment_digest
            .as_deref()
            .is_some_and(|digest| !is_canonical_sha256_digest(digest))
        || match (
            input.assessment_digest.as_deref(),
            input.approved_assessment_digest.as_deref(),
        ) {
            (None, Some(_)) => true,
            (Some(assessment), Some(approved)) => assessment != approved,
            _ => false,
        }
    {
        return Err(ExecutionDomainError::InvalidDeferAssessment);
    }
    let duration = input.move_end.signed_duration_since(input.move_start);
    if !has_postgres_timestamp_precision(input.move_start)
        || !has_postgres_timestamp_precision(input.move_end)
        || input.move_start <= now
        || input.move_end <= input.move_start
        || duration > chrono::Duration::seconds(i64::from(MAX_DEFER_SECONDS))
    {
        return Err(ExecutionDomainError::InvalidDefer);
    }
    Ok(())
}

fn is_canonical_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn reject_nil(id: Uuid) -> Result<(), ExecutionDomainError> {
    if id.is_nil() {
        Err(ExecutionDomainError::NilIdentifier)
    } else {
        Ok(())
    }
}

fn next_revision(current: u64) -> Result<u64, ExecutionDomainError> {
    current
        .checked_add(1)
        .ok_or(ExecutionDomainError::DurationOverflow)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn start() -> StartExecution {
        StartExecution {
            session_id: Uuid::from_u128(1),
            item_id: Uuid::from_u128(2),
            item_revision: 4,
            occurrence_id: None,
            session_index: 0,
            planned_block_id: Some(Uuid::from_u128(3)),
            device_id: Uuid::from_u128(4),
        }
    }

    #[test]
    fn timer_accumulates_running_segments_but_not_break_time() {
        let t0 = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let session = ExecutionSession::start(&start(), t0);
        let paused = session
            .apply(
                &ExecutionCommand::Pause(PauseExecution {
                    session_id: session.id,
                    duration_seconds: Some(300),
                    pause_until: None,
                    reason: Some("Tea".to_owned()),
                }),
                t0 + chrono::Duration::seconds(90),
            )
            .unwrap();
        let resumed = paused
            .apply(
                &ExecutionCommand::Resume(ResumeExecution {
                    session_id: session.id,
                }),
                t0 + chrono::Duration::seconds(390),
            )
            .unwrap();
        let completed = resumed
            .apply(
                &ExecutionCommand::Complete(FinishExecution {
                    session_id: session.id,
                    actual_seconds: None,
                }),
                t0 + chrono::Duration::seconds(450),
            )
            .unwrap();

        assert_eq!(completed.accumulated_seconds, 150);
        assert_eq!(completed.actual_seconds, Some(150));
        assert_eq!(completed.status, ExecutionStatus::Completed);
    }

    #[test]
    fn persisted_session_clock_never_moves_backwards_with_wall_clock() {
        let t0 = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let session = ExecutionSession::start(&start(), t0);
        let paused = session
            .apply(
                &ExecutionCommand::Pause(PauseExecution {
                    session_id: session.id,
                    duration_seconds: Some(60),
                    pause_until: None,
                    reason: None,
                }),
                t0 - chrono::Duration::minutes(5),
            )
            .unwrap();
        assert_eq!(paused.updated_at, t0);
        assert_eq!(paused.paused_at, Some(t0));
        assert_eq!(paused.pause_until, Some(t0 + chrono::Duration::seconds(60)));

        let resumed = paused
            .apply(
                &ExecutionCommand::Resume(ResumeExecution {
                    session_id: session.id,
                }),
                t0 - chrono::Duration::minutes(4),
            )
            .unwrap();
        assert_eq!(resumed.updated_at, t0);
        assert_eq!(resumed.running_since, Some(t0));

        let completed = resumed
            .apply(
                &ExecutionCommand::Complete(FinishExecution {
                    session_id: session.id,
                    actual_seconds: None,
                }),
                t0 - chrono::Duration::minutes(3),
            )
            .unwrap();
        assert_eq!(completed.updated_at, t0);
        assert_eq!(completed.ended_at, Some(t0));
        assert_eq!(completed.accumulated_seconds, 0);
    }

    #[test]
    fn workspace_protocol_clock_does_not_inflate_elapsed_work() {
        let observed_start = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let protocol_start = observed_start + chrono::Duration::hours(1);
        let session =
            ExecutionSession::start_with_protocol_time(&start(), protocol_start, observed_start);
        let observed_finish = observed_start + chrono::Duration::seconds(10);
        let causal_transition = protocol_start + chrono::Duration::microseconds(1);
        let completed = session
            .apply_with_protocol_time(
                &ExecutionCommand::Complete(FinishExecution {
                    session_id: session.id,
                    actual_seconds: None,
                }),
                causal_transition,
                observed_finish,
            )
            .unwrap();

        assert_eq!(completed.updated_at, causal_transition);
        assert_eq!(completed.ended_at, Some(causal_transition));
        assert_eq!(completed.accumulated_seconds, 10);
        assert_eq!(completed.actual_seconds, Some(10));

        let session = ExecutionSession::start(&start(), observed_start);
        let paused = session
            .apply_with_protocol_time(
                &ExecutionCommand::Pause(PauseExecution {
                    session_id: session.id,
                    duration_seconds: None,
                    pause_until: None,
                    reason: None,
                }),
                protocol_start,
                observed_start + chrono::Duration::seconds(10),
            )
            .unwrap();
        let resumed = paused
            .apply_with_protocol_time(
                &ExecutionCommand::Resume(ResumeExecution {
                    session_id: session.id,
                }),
                protocol_start + chrono::Duration::microseconds(1),
                observed_start + chrono::Duration::seconds(20),
            )
            .unwrap();
        let completed = resumed
            .apply_with_protocol_time(
                &ExecutionCommand::Complete(FinishExecution {
                    session_id: session.id,
                    actual_seconds: None,
                }),
                protocol_start + chrono::Duration::microseconds(2),
                observed_start + chrono::Duration::seconds(25),
            )
            .unwrap();
        assert_eq!(completed.accumulated_seconds, 15);
        assert_eq!(completed.actual_seconds, Some(15));

        let wire = serde_json::to_value(&resumed).unwrap();
        assert!(wire.get("observed_running_since").is_none());
        assert_eq!(wire["running_since"], serde_json::json!(resumed.updated_at));
    }

    #[test]
    fn pause_rejects_conflicting_duration_and_until() {
        let now = Utc::now();
        let command = ExecutionCommand::Pause(PauseExecution {
            session_id: Uuid::from_u128(1),
            duration_seconds: Some(60),
            pause_until: Some(now + chrono::Duration::minutes(1)),
            reason: None,
        });
        assert_eq!(
            command.validate(now),
            Err(ExecutionDomainError::InvalidPause)
        );
    }

    #[test]
    fn absolute_pause_requires_postgres_microsecond_precision() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let session = ExecutionSession::start(&start(), now);
        let exact = ExecutionCommand::Pause(PauseExecution {
            session_id: session.id,
            duration_seconds: None,
            pause_until: Some(now + chrono::Duration::microseconds(1)),
            reason: None,
        });
        assert_eq!(exact.validate(now), Ok(()));
        assert_eq!(
            session.apply(&exact, now).unwrap().pause_until,
            Some(now + chrono::Duration::microseconds(1))
        );

        let nanosecond = ExecutionCommand::Pause(PauseExecution {
            session_id: session.id,
            duration_seconds: None,
            pause_until: Some(
                now + chrono::Duration::minutes(1) + chrono::Duration::nanoseconds(1),
            ),
            reason: None,
        });
        assert_eq!(
            nanosecond.validate(now),
            Err(ExecutionDomainError::InvalidPause)
        );
        assert_eq!(
            session.apply(&nanosecond, now),
            Err(ExecutionDomainError::InvalidPause)
        );
    }

    #[test]
    fn timed_pause_can_be_extended_without_counting_break_time() {
        let t0 = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let session = ExecutionSession::start(&start(), t0);
        let paused = session
            .apply(
                &ExecutionCommand::Pause(PauseExecution {
                    session_id: session.id,
                    duration_seconds: Some(60),
                    pause_until: None,
                    reason: Some("Tea".to_owned()),
                }),
                t0 + chrono::Duration::seconds(30),
            )
            .unwrap();
        let extended = paused
            .apply(
                &ExecutionCommand::Pause(PauseExecution {
                    session_id: session.id,
                    duration_seconds: Some(600),
                    pause_until: None,
                    reason: None,
                }),
                t0 + chrono::Duration::seconds(90),
            )
            .unwrap();

        assert_eq!(extended.accumulated_seconds, 30);
        assert_eq!(extended.pause_reason.as_deref(), Some("Tea"));
        assert_eq!(
            extended.pause_until,
            Some(t0 + chrono::Duration::seconds(690))
        );
    }

    #[test]
    fn legacy_defer_wire_bytes_remain_unchanged_when_assessment_fields_are_absent() {
        #[derive(Serialize)]
        #[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
        enum LegacyExecutionCommand {
            Defer(LegacyDeferExecution),
        }

        #[derive(Serialize)]
        #[serde(deny_unknown_fields)]
        struct LegacyDeferExecution {
            session_id: Uuid,
            move_start: DateTime<Utc>,
            move_end: DateTime<Utc>,
            actual_seconds: Option<u64>,
        }

        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let current = ExecutionCommand::Defer(DeferExecution {
            session_id: Uuid::from_u128(1),
            move_start: now + chrono::Duration::minutes(1),
            move_end: now + chrono::Duration::minutes(2),
            actual_seconds: Some(7),
            assessment_digest: None,
            approved_assessment_digest: None,
        });
        let legacy = LegacyExecutionCommand::Defer(LegacyDeferExecution {
            session_id: Uuid::from_u128(1),
            move_start: now + chrono::Duration::minutes(1),
            move_end: now + chrono::Duration::minutes(2),
            actual_seconds: Some(7),
        });

        let current_bytes = serde_json::to_vec(&current).unwrap();
        assert_eq!(current_bytes, serde_json::to_vec(&legacy).unwrap());
        assert_eq!(
            serde_json::from_slice::<ExecutionCommand>(&current_bytes).unwrap(),
            current
        );
        let shape: serde_json::Value = serde_json::from_slice(&current_bytes).unwrap();
        assert!(shape.get("assessment_digest").is_none());
        assert!(shape.get("approved_assessment_digest").is_none());
    }

    #[test]
    fn defer_wire_rejects_unknown_fields() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let mut shape = serde_json::to_value(ExecutionCommand::Defer(DeferExecution {
            session_id: Uuid::from_u128(1),
            move_start: now + chrono::Duration::minutes(1),
            move_end: now + chrono::Duration::minutes(2),
            actual_seconds: None,
            assessment_digest: None,
            approved_assessment_digest: None,
        }))
        .unwrap();
        shape["assessment"] = serde_json::json!("unexpected");

        assert!(serde_json::from_value::<ExecutionCommand>(shape).is_err());
    }

    #[test]
    fn defer_assessment_digests_are_canonical_and_approval_must_match() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let digest = format!("sha256:{}", "a".repeat(64));
        let other_digest = format!("sha256:{}", "b".repeat(64));
        let command = |assessment_digest: Option<String>, approved_assessment_digest| {
            ExecutionCommand::Defer(DeferExecution {
                session_id: Uuid::from_u128(1),
                move_start: now + chrono::Duration::minutes(1),
                move_end: now + chrono::Duration::minutes(2),
                actual_seconds: None,
                assessment_digest,
                approved_assessment_digest,
            })
        };

        assert_eq!(command(Some(digest.clone()), None).validate(now), Ok(()));
        let approved = command(Some(digest.clone()), Some(digest.clone()));
        assert_eq!(approved.validate(now), Ok(()));
        let approved_shape = serde_json::to_value(&approved).unwrap();
        assert_eq!(approved_shape["assessment_digest"], digest);
        assert_eq!(approved_shape["approved_assessment_digest"], digest);
        assert_eq!(
            serde_json::from_value::<ExecutionCommand>(approved_shape).unwrap(),
            approved
        );

        for invalid in [
            command(Some("sha256:abc".to_owned()), None),
            command(Some(format!("sha256:{}", "A".repeat(64))), None),
            command(Some(format!("sha256:{}", "g".repeat(64))), None),
            command(Some(format!("SHA256:{}", "a".repeat(64))), None),
            command(None, Some(digest.clone())),
            command(Some(digest.clone()), Some(other_digest)),
            command(
                Some(digest.clone()),
                Some(format!("sha256:{}", "A".repeat(64))),
            ),
        ] {
            assert_eq!(
                invalid.validate(now),
                Err(ExecutionDomainError::InvalidDeferAssessment)
            );
        }
    }

    #[test]
    fn defer_requires_paused_session_and_preserves_paused_actual_semantics() {
        let t0 = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let move_start = t0 + chrono::Duration::hours(2);
        let move_end = move_start + chrono::Duration::minutes(45);
        let session = ExecutionSession::start(&start(), t0);
        let active_result = session.apply(
            &ExecutionCommand::Defer(DeferExecution {
                session_id: session.id,
                move_start,
                move_end,
                actual_seconds: None,
                assessment_digest: None,
                approved_assessment_digest: None,
            }),
            t0 + chrono::Duration::seconds(90),
        );
        assert_eq!(active_result, Err(ExecutionDomainError::InvalidTransition));

        let paused = session
            .apply(
                &ExecutionCommand::Pause(PauseExecution {
                    session_id: session.id,
                    duration_seconds: None,
                    pause_until: None,
                    reason: Some("Waiting".to_owned()),
                }),
                t0 + chrono::Duration::seconds(30),
            )
            .unwrap();
        let deferred = paused
            .apply(
                &ExecutionCommand::Defer(DeferExecution {
                    session_id: session.id,
                    move_start,
                    move_end,
                    actual_seconds: None,
                    assessment_digest: None,
                    approved_assessment_digest: None,
                }),
                t0 + chrono::Duration::minutes(10),
            )
            .unwrap();

        assert_eq!(deferred.status, ExecutionStatus::Deferred);
        assert!(!deferred.status.is_open());
        assert_eq!(deferred.accumulated_seconds, 30);
        assert_eq!(deferred.actual_seconds, Some(30));
        assert_eq!(deferred.move_start, Some(move_start));
        assert_eq!(deferred.move_end, Some(move_end));
        assert_eq!(deferred.ended_at, Some(t0 + chrono::Duration::minutes(10)));
        assert!(deferred.running_since.is_none());
        assert_eq!(deferred.paused_at, Some(t0 + chrono::Duration::seconds(30)));
        assert!(deferred.pause_until.is_none());
        assert!(deferred.pause_reason.is_none());

        let corrected = paused
            .apply(
                &ExecutionCommand::Defer(DeferExecution {
                    session_id: session.id,
                    move_start,
                    move_end,
                    actual_seconds: Some(12),
                    assessment_digest: None,
                    approved_assessment_digest: None,
                }),
                t0 + chrono::Duration::minutes(10),
            )
            .unwrap();
        assert_eq!(corrected.accumulated_seconds, 30);
        assert_eq!(corrected.actual_seconds, Some(12));
    }

    #[test]
    fn defer_requires_an_exact_future_window_inside_twenty_four_hours() {
        let now = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let session_id = Uuid::from_u128(1);
        let valid = ExecutionCommand::Defer(DeferExecution {
            session_id,
            move_start: now + chrono::Duration::days(30),
            move_end: now + chrono::Duration::days(31),
            actual_seconds: None,
            assessment_digest: None,
            approved_assessment_digest: None,
        });
        assert_eq!(valid.validate(now), Ok(()));

        let nanosecond = ExecutionCommand::Defer(DeferExecution {
            session_id,
            move_start: now + chrono::Duration::minutes(1) + chrono::Duration::nanoseconds(1),
            move_end: now + chrono::Duration::minutes(2),
            actual_seconds: None,
            assessment_digest: None,
            approved_assessment_digest: None,
        });
        assert_eq!(
            nanosecond.validate(now),
            Err(ExecutionDomainError::InvalidDefer)
        );

        for command in [
            ExecutionCommand::Defer(DeferExecution {
                session_id,
                move_start: now,
                move_end: now + chrono::Duration::minutes(1),
                actual_seconds: None,
                assessment_digest: None,
                approved_assessment_digest: None,
            }),
            ExecutionCommand::Defer(DeferExecution {
                session_id,
                move_start: now + chrono::Duration::minutes(1),
                move_end: now + chrono::Duration::minutes(1),
                actual_seconds: None,
                assessment_digest: None,
                approved_assessment_digest: None,
            }),
            ExecutionCommand::Defer(DeferExecution {
                session_id,
                move_start: now + chrono::Duration::days(30),
                move_end: now + chrono::Duration::days(31) + chrono::Duration::seconds(1),
                actual_seconds: None,
                assessment_digest: None,
                approved_assessment_digest: None,
            }),
        ] {
            assert_eq!(
                command.validate(now),
                Err(ExecutionDomainError::InvalidDefer)
            );
        }

        let invalid_actual = ExecutionCommand::Defer(DeferExecution {
            session_id,
            move_start: now + chrono::Duration::minutes(1),
            move_end: now + chrono::Duration::minutes(2),
            actual_seconds: Some(MAX_ACTUAL_SECONDS + 1),
            assessment_digest: None,
            approved_assessment_digest: None,
        });
        assert_eq!(
            invalid_actual.validate(now),
            Err(ExecutionDomainError::InvalidActualDuration)
        );
    }

    #[test]
    fn defer_revalidates_against_the_persisted_monotonic_clock() {
        let t0 = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let session = ExecutionSession::start(&start(), t0);
        let paused = session
            .apply(
                &ExecutionCommand::Pause(PauseExecution {
                    session_id: session.id,
                    duration_seconds: None,
                    pause_until: None,
                    reason: None,
                }),
                t0 + chrono::Duration::hours(2),
            )
            .unwrap();
        let command = ExecutionCommand::Defer(DeferExecution {
            session_id: session.id,
            move_start: t0 + chrono::Duration::minutes(90),
            move_end: t0 + chrono::Duration::hours(3),
            actual_seconds: None,
            assessment_digest: None,
            approved_assessment_digest: None,
        });

        assert_eq!(command.validate(t0 + chrono::Duration::hours(1)), Ok(()));
        assert_eq!(
            paused.apply(&command, t0 + chrono::Duration::hours(1)),
            Err(ExecutionDomainError::InvalidDefer)
        );
    }

    #[test]
    fn session_wire_shape_omits_legacy_move_fields_and_rejects_partial_pairs() {
        let t0 = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let session = ExecutionSession::start(&start(), t0);
        let legacy_shape = serde_json::to_value(&session).unwrap();
        assert!(legacy_shape.get("move_start").is_none());
        assert!(legacy_shape.get("move_end").is_none());
        assert_eq!(
            serde_json::from_value::<ExecutionSession>(legacy_shape.clone()).unwrap(),
            session
        );

        let mut partial = legacy_shape.clone();
        partial["move_start"] = serde_json::json!(t0 + chrono::Duration::hours(1));
        assert!(serde_json::from_value::<ExecutionSession>(partial).is_err());

        let paused = session
            .apply(
                &ExecutionCommand::Pause(PauseExecution {
                    session_id: session.id,
                    duration_seconds: None,
                    pause_until: None,
                    reason: None,
                }),
                t0 + chrono::Duration::seconds(30),
            )
            .unwrap();
        let deferred = paused
            .apply(
                &ExecutionCommand::Defer(DeferExecution {
                    session_id: session.id,
                    move_start: t0 + chrono::Duration::hours(1),
                    move_end: t0 + chrono::Duration::hours(2),
                    actual_seconds: None,
                    assessment_digest: None,
                    approved_assessment_digest: None,
                }),
                t0 + chrono::Duration::minutes(1),
            )
            .unwrap();
        let deferred_shape = serde_json::to_value(&deferred).unwrap();
        assert!(deferred_shape.get("move_start").is_some());
        assert!(deferred_shape.get("move_end").is_some());
        assert_eq!(
            serde_json::from_value::<ExecutionSession>(deferred_shape.clone()).unwrap(),
            deferred
        );

        let mut partial_deferred = deferred_shape.clone();
        partial_deferred.as_object_mut().unwrap().remove("move_end");
        assert!(serde_json::from_value::<ExecutionSession>(partial_deferred).is_err());

        let mut mismatched_terminal_time = deferred_shape.clone();
        mismatched_terminal_time["ended_at"] =
            serde_json::json!(deferred.updated_at - chrono::Duration::seconds(1));
        assert!(serde_json::from_value::<ExecutionSession>(mismatched_terminal_time).is_err());

        let mut nanosecond_deferred = deferred_shape.clone();
        nanosecond_deferred["move_start"] =
            serde_json::json!(deferred.move_start.unwrap() + chrono::Duration::nanoseconds(1));
        assert!(serde_json::from_value::<ExecutionSession>(nanosecond_deferred).is_err());

        let mut stale_deferred = deferred_shape;
        stale_deferred["move_start"] = stale_deferred["updated_at"].clone();
        assert!(serde_json::from_value::<ExecutionSession>(stale_deferred).is_err());

        let mut nonterminal_with_move = legacy_shape;
        nonterminal_with_move["move_start"] = serde_json::json!(t0 + chrono::Duration::hours(1));
        nonterminal_with_move["move_end"] = serde_json::json!(t0 + chrono::Duration::hours(2));
        assert!(serde_json::from_value::<ExecutionSession>(nonterminal_with_move).is_err());
    }

    #[test]
    fn forgotten_session_can_always_be_closed() {
        let t0 = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let session = ExecutionSession::start(&start(), t0);
        let completed = session
            .apply(
                &ExecutionCommand::Complete(FinishExecution {
                    session_id: session.id,
                    actual_seconds: None,
                }),
                t0 + chrono::Duration::days(400),
            )
            .unwrap();

        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert_eq!(completed.actual_seconds, Some(400 * 24 * 60 * 60));
    }
}
