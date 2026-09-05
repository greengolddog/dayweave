//! Durable habit occurrence outcomes, pauses, delta sync, and private analytics.

mod domain;
pub(crate) mod http;
mod invalidation;
mod repository;
mod service;

pub use domain::*;
pub(crate) use repository::OccurrencePageCursor;
pub use repository::{
    HabitIdempotency, HabitMissedConfiguration, HabitRepository, HabitRepositoryError,
    InMemoryHabitRepository, MissedReconcileWrite, MissedResolveWrite, OutcomeWrite, PauseCreate,
    PauseResume,
};
pub use service::{HabitIdempotencyKey, HabitService, HabitServiceError};
