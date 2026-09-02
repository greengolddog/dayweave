mod compose;
pub(crate) mod http;
mod invalidation;
mod memory;
mod ports;
mod postgres;
mod projection;
mod proposal_bridge;

pub(crate) const SCHEDULER_PUBLICATION_SCHEMA: &str = "dayweave-scheduler-publication/5";
pub(crate) const MANUAL_PLACEMENT_PUBLICATION_SCHEMA: &str = "dayweave-scheduler-publication/4";

/// `PostgreSQL` `timestamptz` stores microseconds. Query boundaries must already
/// use that precision so a read cannot silently change meaning when bound.
pub(crate) fn has_postgres_timestamp_precision(value: chrono::DateTime<chrono::Utc>) -> bool {
    value.timestamp_subsec_nanos().is_multiple_of(1_000)
}

pub(crate) fn truncate_to_postgres_timestamp_precision(
    value: chrono::DateTime<chrono::Utc>,
) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp_micros(value.timestamp_micros())
        .expect("a valid DateTime must remain representable at microsecond precision")
}

pub use compose::{
    ComposeScheduleError, ComposeScheduleResult, ManualPlacementApproval,
    ManualPlacementAssessmentOutput, ManualPlacementConflictOutput, ManualPlacementViolationOutput,
    RetainedManualPlacementAssignmentSummary, RetainedManualPlacementCatalog,
    RetainedManualPlacementSummary, Rfc3339SchedulePlan, compose_canonical_schedule,
};
pub(crate) use compose::{
    compose_canonical_schedule_unfenced, map_manual_placement_violations,
    retained_manual_placement_catalog,
};
pub use dayweave_compose::{
    AvailabilityInput, ComposeScheduleRequest, EnergyInput, FixedBlockInput, FixedBlockSourceInput,
    IgnoredPreviousAssignment, ManualPlacementAssignmentInput, ManualPlacementInput,
    ManualPlacementReleaseInput, PreviousAssignmentInput, PreviousBlockInput, RejectedScheduleItem,
    SchedulerConfigInput,
};
pub use invalidation::{ScheduleInvalidationConfig, ScheduleInvalidationConfigError};
pub(crate) use invalidation::{ScheduleInvalidationOpenError, ScheduleInvalidationSignal};
pub use memory::{
    InMemoryScheduleQueryPort, InMemorySimulationPort, simulation_request_digest,
    simulation_request_hash,
};
pub use ports::*;
pub(crate) use postgres::{
    AuthoritativePlanningEvidence, PublishedPlanningPolicy, assert_current_calendar_projection,
    assert_current_item_snapshot, assert_current_planning_policy_tx,
    authoritative_planning_evidence_tx, lock_owner, published_planning_policy_tx,
};
pub use postgres::{
    CurrentPublishedSchedule, PostgresSchedulingRepository, PublishScheduleSpec,
    PublishedScheduleRevision, SchedulePublication, SchedulePublicationError,
};
pub(crate) use projection::{CalendarProjectionFenceError, CalendarProjectionStamp};
pub(crate) use proposal_bridge::materialize_proposal;
