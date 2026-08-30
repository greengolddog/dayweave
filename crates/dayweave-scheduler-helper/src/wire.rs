use dayweave_core::{
    DecisionKind, ExplanationCode, ItemId, OccurrenceId, OccurrenceState, PlacementExplanation,
    PlanDecision, PlanScore, PlanViolation, RecurrenceOccurrenceIdentity, ScheduleBlock,
    ScheduleBlockKind, SchedulePlan, UnscheduledReason, UnscheduledWork, ViolationKind,
    ViolationSeverity,
};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WireEncodingError;

#[derive(Debug, Serialize)]
pub(crate) struct PlanOutput {
    as_of: String,
    horizon_start: String,
    horizon_end: String,
    blocks: Vec<ScheduleBlockOutput>,
    unscheduled: Vec<UnscheduledOutput>,
    decisions: Vec<DecisionOutput>,
    violations: Vec<ViolationOutput>,
    score: ScoreOutput,
    occurrences: Vec<OccurrenceOutput>,
}

impl TryFrom<SchedulePlan> for PlanOutput {
    type Error = WireEncodingError;

    fn try_from(plan: SchedulePlan) -> Result<Self, Self::Error> {
        Ok(Self {
            as_of: rfc3339(plan.as_of)?,
            horizon_start: rfc3339(plan.horizon_start)?,
            horizon_end: rfc3339(plan.horizon_end)?,
            blocks: plan
                .blocks
                .into_iter()
                .map(ScheduleBlockOutput::try_from)
                .collect::<Result<_, _>>()?,
            unscheduled: plan
                .unscheduled
                .into_iter()
                .map(UnscheduledOutput::from)
                .collect(),
            decisions: plan
                .decisions
                .into_iter()
                .map(DecisionOutput::from)
                .collect(),
            violations: plan
                .violations
                .into_iter()
                .map(ViolationOutput::try_from)
                .collect::<Result<_, _>>()?,
            score: ScoreOutput::from(plan.score),
            occurrences: plan
                .occurrences
                .into_iter()
                .map(OccurrenceOutput::try_from)
                .collect::<Result<_, _>>()?,
        })
    }
}

#[derive(Debug, Serialize)]
struct ScheduleBlockOutput {
    id: String,
    is_sensitive: bool,
    item_id: Option<ItemId>,
    occurrence_id: Option<OccurrenceId>,
    external_block_id: Option<String>,
    title: String,
    start: String,
    end: String,
    session_index: u16,
    kind: ScheduleBlockKind,
    explanations: Vec<ExplanationOutput>,
}

impl TryFrom<ScheduleBlock> for ScheduleBlockOutput {
    type Error = WireEncodingError;

    fn try_from(block: ScheduleBlock) -> Result<Self, Self::Error> {
        Ok(Self {
            id: block.id.to_string(),
            is_sensitive: block.is_sensitive,
            item_id: block.item_id,
            occurrence_id: block.occurrence_id,
            external_block_id: block.external_block_id.map(|value| value.to_string()),
            title: block.title,
            start: rfc3339(block.start)?,
            end: rfc3339(block.end)?,
            session_index: block.session_index,
            kind: block.kind,
            explanations: block
                .explanations
                .into_iter()
                .map(ExplanationOutput::from)
                .collect(),
        })
    }
}

#[derive(Debug, Serialize)]
struct ExplanationOutput {
    code: ExplanationCode,
    message: String,
}

impl From<PlacementExplanation> for ExplanationOutput {
    fn from(explanation: PlacementExplanation) -> Self {
        Self {
            code: explanation.code,
            message: explanation.message,
        }
    }
}

#[derive(Debug, Serialize)]
struct UnscheduledOutput {
    item_id: ItemId,
    occurrence_id: Option<OccurrenceId>,
    remaining: dayweave_core::Minutes,
    reason: UnscheduledReason,
    message: String,
}

impl From<UnscheduledWork> for UnscheduledOutput {
    fn from(work: UnscheduledWork) -> Self {
        Self {
            item_id: work.item_id,
            occurrence_id: work.occurrence_id,
            remaining: work.remaining,
            reason: work.reason,
            message: work.message,
        }
    }
}

#[derive(Debug, Serialize)]
struct DecisionOutput {
    item_id: ItemId,
    occurrence_id: Option<OccurrenceId>,
    kind: DecisionKind,
    message: String,
}

impl From<PlanDecision> for DecisionOutput {
    fn from(decision: PlanDecision) -> Self {
        Self {
            item_id: decision.item_id,
            occurrence_id: decision.occurrence_id,
            kind: decision.kind,
            message: decision.message,
        }
    }
}

#[derive(Debug, Serialize)]
struct ViolationOutput {
    kind: ViolationKind,
    severity: ViolationSeverity,
    item_ids: Vec<ItemId>,
    occurrence_ids: Vec<OccurrenceId>,
    start: Option<String>,
    end: Option<String>,
    penalty: u64,
    message: String,
}

impl TryFrom<PlanViolation> for ViolationOutput {
    type Error = WireEncodingError;

    fn try_from(violation: PlanViolation) -> Result<Self, Self::Error> {
        Ok(Self {
            kind: violation.kind,
            severity: violation.severity,
            item_ids: violation.item_ids,
            occurrence_ids: violation.occurrence_ids,
            start: violation.start.map(rfc3339).transpose()?,
            end: violation.end.map(rfc3339).transpose()?,
            penalty: violation.penalty,
            message: violation.message,
        })
    }
}

#[derive(Debug, Serialize)]
struct ScoreOutput {
    scheduled_minutes: u32,
    unscheduled_minutes: u32,
    soft_penalty: u64,
    moved_minutes: u32,
}

impl From<PlanScore> for ScoreOutput {
    fn from(score: PlanScore) -> Self {
        Self {
            scheduled_minutes: score.scheduled_minutes,
            unscheduled_minutes: score.unscheduled_minutes,
            soft_penalty: score.soft_penalty,
            moved_minutes: score.moved_minutes,
        }
    }
}

#[derive(Debug, Serialize)]
struct OccurrenceOutput {
    id: OccurrenceId,
    series_item_id: ItemId,
    identity: RecurrenceOccurrenceIdentity,
    nominal_start: String,
    nominal_end: String,
    window_start: String,
    window_end: String,
    local_date: Option<String>,
    ordinal: u32,
    state: OccurrenceState,
}

impl TryFrom<dayweave_core::Occurrence> for OccurrenceOutput {
    type Error = WireEncodingError;

    fn try_from(occurrence: dayweave_core::Occurrence) -> Result<Self, Self::Error> {
        Ok(Self {
            id: occurrence.id,
            series_item_id: occurrence.series_item_id,
            identity: occurrence.identity,
            nominal_start: rfc3339(occurrence.nominal_start)?,
            nominal_end: rfc3339(occurrence.nominal_end)?,
            window_start: rfc3339(occurrence.window_start)?,
            window_end: rfc3339(occurrence.window_end)?,
            local_date: occurrence.local_date.map(|date| date.to_string()),
            ordinal: occurrence.ordinal,
            state: occurrence.state,
        })
    }
}

fn rfc3339(value: OffsetDateTime) -> Result<String, WireEncodingError> {
    value.format(&Rfc3339).map_err(|_| WireEncodingError)
}

#[cfg(test)]
mod tests {
    use dayweave_core::{
        ItemId, Occurrence, OccurrenceId, OccurrenceState, RecurrenceOccurrenceIdentity,
    };
    use serde_json::json;
    use time::macros::{date, datetime};
    use uuid::Uuid;

    use super::OccurrenceOutput;

    #[test]
    fn occurrence_output_exposes_the_exact_move_identity() {
        let occurrence = Occurrence {
            id: OccurrenceId(Uuid::from_u128(2)),
            series_item_id: ItemId::from_uuid(Uuid::from_u128(1)),
            identity: RecurrenceOccurrenceIdentity::CalendarDay {
                date: date!(2026 - 09 - 01),
                bucket_ordinal: 2,
            },
            nominal_start: datetime!(2026-09-01 8:00 +02:00),
            nominal_end: datetime!(2026-09-01 9:00 +02:00),
            window_start: datetime!(2026-09-01 8:00 +02:00),
            window_end: datetime!(2026-09-01 9:00 +02:00),
            local_date: Some(date!(2026 - 09 - 01)),
            ordinal: 2,
            state: OccurrenceState::Generated,
        };
        let encoded =
            serde_json::to_value(OccurrenceOutput::try_from(occurrence).unwrap()).unwrap();
        assert_eq!(
            encoded["identity"],
            json!({
                "type": "calendar_day",
                "date": "2026-09-01",
                "bucket_ordinal": 2
            })
        );
        assert_eq!(encoded["nominal_start"], json!("2026-09-01T08:00:00+02:00"));
    }
}
