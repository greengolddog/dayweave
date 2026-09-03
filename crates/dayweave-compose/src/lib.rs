//! Pure preparation of a canonical `DayWeave` snapshot for the deterministic
//! scheduling core.
//!
//! This crate performs no I/O, reads no clock, runs no scheduler, and computes
//! no publication digest. Callers supply the complete snapshot and every time
//! input explicitly, then decide how and where to execute the returned
//! [`dayweave_core::PlanRequest`].

mod metadata;
mod model;
mod prepare;

pub use metadata::{
    CalendarContextSpec, DayWeaveFirmBlockSpec, EnergyMetadata, MAX_RECURRENCE_BYTES,
    MAX_SCHEDULING_METADATA_BYTES, MAX_SCHEDULING_OFFSET_MINUTES, SchedulingMetadata,
    SchedulingMetadataError, SchedulingMetadataInput, ValidatedSchedulingMetadata,
    is_canonical_rfc3339, parse_recurrence, validate_scheduling_metadata,
};

pub use model::{
    AvailabilityInput, CanonicalItem, CanonicalItemKind, CanonicalItemStatus, CanonicalSplitPolicy,
    ComposeScheduleRequest, EnergyInput, FixedBlockInput, FixedBlockSourceInput,
    IgnoredPreviousAssignment, ManualPlacementAssignmentInput, ManualPlacementInput,
    ManualPlacementReleaseInput, PreparedSchedule, PreviousAssignmentInput, PreviousBlockInput,
    RejectedScheduleItem, SchedulerConfigInput,
};
pub use prepare::{
    MAX_CANONICAL_ITEMS, MAX_MANUAL_ASSIGNMENTS, MAX_MANUAL_BLOCKS, MAX_MANUAL_PLACEMENT_RELEASES,
    MAX_MANUAL_PLACEMENTS, PrepareScheduleError, prepare_canonical_schedule,
    validate_schedule_request,
};
