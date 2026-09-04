//! Durable scheduling primitives shared by every `DayWeave` client and service.
//!
//! The engine deliberately has no I/O, wall-clock, randomness, or platform
//! dependencies. Given the same [`PlanRequest`], it always emits the same
//! [`SchedulePlan`], which makes offline planning and cross-device conflict
//! resolution reproducible.

mod custom_recurrence;
mod domain;
mod habits;
mod recurrence;
mod scheduler;

pub use custom_recurrence::*;
pub use domain::*;
pub use habits::*;
pub use recurrence::*;
pub use scheduler::*;
