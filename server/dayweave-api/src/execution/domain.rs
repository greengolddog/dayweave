use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

pub const MAX_PAUSE_SECONDS: u32 = 24 * 60 * 60;
pub const MAX_ACTUAL_SECONDS: u64 = i64::MAX as u64;
const MAX_REASON_CHARS: usize = 500;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Active,
    Paused,
    Completed,
    Skipped,
}

impl ExecutionStatus {
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Active | Self::Paused)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, ToSchema)]
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
    pub paused_at: Option<DateTime<Utc>>,
    pub pause_until: Option<DateTime<Utc>>,
    pub pause_reason: Option<String>,
    pub ended_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ExecutionSession {
    pub(crate) fn start(input: &StartExecution, now: DateTime<Utc>) -> Self {
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
            started_at: now,
            running_since: Some(now),
            paused_at: None,
            pause_until: None,
            pause_reason: None,
            ended_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    pub(crate) fn apply(
        &self,
        command: &ExecutionCommand,
        now: DateTime<Utc>,
    ) -> Result<Self, ExecutionDomainError> {
        // Wall clocks can move backwards. Session time is a persisted protocol clock: keeping it
        // monotonic preserves elapsed accounting, newest-first pagination, and client continuity.
        let transition_at = now.max(self.updated_at);
        match command {
            ExecutionCommand::Start(_) => Err(ExecutionDomainError::InvalidTransition),
            ExecutionCommand::Pause(input) => self.pause(input, transition_at),
            ExecutionCommand::Resume(input) => self.resume(input, transition_at),
            ExecutionCommand::Complete(input) => self.finish(
                input.session_id,
                input.actual_seconds,
                ExecutionStatus::Completed,
                transition_at,
            ),
            ExecutionCommand::Skip(input) => self.finish(
                input.session_id,
                input.actual_seconds,
                ExecutionStatus::Skipped,
                transition_at,
            ),
        }
    }

    fn pause(
        &self,
        input: &PauseExecution,
        now: DateTime<Utc>,
    ) -> Result<Self, ExecutionDomainError> {
        if self.id != input.session_id || !self.status.is_open() {
            return Err(ExecutionDomainError::InvalidTransition);
        }
        validate_reason(input.reason.as_deref())?;
        let pause_until = match (input.duration_seconds, input.pause_until) {
            (Some(seconds), None) if (1..=MAX_PAUSE_SECONDS).contains(&seconds) => {
                Some(now + chrono::Duration::seconds(i64::from(seconds)))
            }
            (None, Some(until)) => {
                let maximum = now + chrono::Duration::seconds(i64::from(MAX_PAUSE_SECONDS));
                if until <= now || until > maximum {
                    return Err(ExecutionDomainError::InvalidPause);
                }
                Some(until)
            }
            (None, None) => None,
            _ => {
                return Err(ExecutionDomainError::InvalidPause);
            }
        };
        let accumulated_seconds = self.elapsed_seconds(now)?;
        Ok(Self {
            status: ExecutionStatus::Paused,
            revision: next_revision(self.revision)?,
            accumulated_seconds,
            running_since: None,
            paused_at: self.paused_at.or(Some(now)),
            pause_until,
            pause_reason: input.reason.clone().or_else(|| self.pause_reason.clone()),
            updated_at: now,
            ..self.clone()
        })
    }

    fn resume(
        &self,
        input: &ResumeExecution,
        now: DateTime<Utc>,
    ) -> Result<Self, ExecutionDomainError> {
        if self.id != input.session_id || self.status != ExecutionStatus::Paused {
            return Err(ExecutionDomainError::InvalidTransition);
        }
        Ok(Self {
            status: ExecutionStatus::Active,
            revision: next_revision(self.revision)?,
            running_since: Some(now),
            paused_at: None,
            pause_until: None,
            pause_reason: None,
            updated_at: now,
            ..self.clone()
        })
    }

    fn finish(
        &self,
        session_id: Uuid,
        corrected_actual_seconds: Option<u64>,
        status: ExecutionStatus,
        now: DateTime<Utc>,
    ) -> Result<Self, ExecutionDomainError> {
        if self.id != session_id || !self.status.is_open() || status.is_open() {
            return Err(ExecutionDomainError::InvalidTransition);
        }
        let elapsed = self.elapsed_seconds(now)?;
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
            paused_at: self
                .paused_at
                .or((self.status == ExecutionStatus::Paused).then_some(now)),
            pause_until: None,
            pause_reason: None,
            ended_at: Some(now),
            updated_at: now,
            ..self.clone()
        })
    }

    fn elapsed_seconds(&self, now: DateTime<Utc>) -> Result<u64, ExecutionDomainError> {
        let running = self.running_since.map_or(Ok(0), |started| {
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
}

impl ExecutionCommand {
    #[must_use]
    pub const fn session_id(&self) -> Uuid {
        match self {
            Self::Start(input) => input.session_id,
            Self::Pause(input) => input.session_id,
            Self::Resume(input) => input.session_id,
            Self::Complete(input) | Self::Skip(input) => input.session_id,
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
                        if until > now
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
