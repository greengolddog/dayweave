mod compose;
pub(crate) mod http;
mod memory;
mod ports;
mod postgres;
mod projection;
mod proposal_bridge;

pub(crate) const SCHEDULER_PUBLICATION_SCHEMA: &str = "dayweave-scheduler-publication/2";

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

pub(crate) use compose::compose_canonical_schedule_unfenced;
pub use compose::{
    ComposeScheduleError, ComposeScheduleResult, Rfc3339SchedulePlan, compose_canonical_schedule,
};
pub use dayweave_compose::{
    AvailabilityInput, ComposeScheduleRequest, EnergyInput, FixedBlockInput, FixedBlockSourceInput,
    IgnoredPreviousAssignment, PreviousAssignmentInput, PreviousBlockInput, RejectedScheduleItem,
    SchedulerConfigInput,
};
pub use memory::{
    InMemoryScheduleQueryPort, InMemorySimulationPort, simulation_request_digest,
    simulation_request_hash,
};
pub use ports::*;
pub use postgres::{
    PostgresSchedulingRepository, PublishScheduleSpec, PublishedScheduleRevision,
    SchedulePublication, SchedulePublicationError,
};
pub(crate) use projection::{CalendarProjectionFenceError, CalendarProjectionStamp};
pub(crate) use proposal_bridge::materialize_proposal;
