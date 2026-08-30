//! Pure preparation of a canonical `DayWeave` snapshot for the deterministic
//! scheduling core.
//!
//! This crate performs no I/O, reads no clock, runs no scheduler, and computes
//! no publication digest. Callers supply the complete snapshot and every time
//! input explicitly, then decide how and where to execute the returned
//! [`dayweave_core::PlanRequest`].

mod model;
mod prepare;

pub use model::{
    AvailabilityInput, CanonicalItem, CanonicalItemKind, CanonicalItemStatus, CanonicalSplitPolicy,
    ComposeScheduleRequest, EnergyInput, FixedBlockInput, FixedBlockSourceInput,
    IgnoredPreviousAssignment, PreparedSchedule, PreviousAssignmentInput, PreviousBlockInput,
    RejectedScheduleItem, SchedulerConfigInput,
};
pub use prepare::{
    MAX_CANONICAL_ITEMS, PrepareScheduleError, prepare_canonical_schedule,
    validate_schedule_request,
};
