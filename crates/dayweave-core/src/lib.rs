//! Durable scheduling primitives shared by every `DayWeave` client and service.
//!
//! The engine deliberately has no I/O, wall-clock, randomness, or platform
//! dependencies. Given the same [`PlanRequest`], it always emits the same
//! [`SchedulePlan`], which makes offline planning and cross-device conflict
//! resolution reproducible.

mod domain;
mod recurrence;
mod scheduler;

pub use domain::*;
pub use recurrence::*;
pub use scheduler::*;
