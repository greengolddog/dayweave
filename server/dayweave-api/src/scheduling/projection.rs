use chrono::{DateTime, Utc};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

/// Content-free evidence that every selected blocking Calendar projection was
/// complete for the exact horizon used by a schedule preview.
///
/// Provider account, calendar, series, event and occurrence identifiers never
/// cross this boundary. The local collection UUID is sufficient to bind a
/// preview to one durable projection generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct CalendarProjectionStamp {
    pub collection_id: Uuid,
    pub collection_revision: u64,
    pub generation: u64,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub refreshed_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum CalendarProjectionFenceError {
    #[error("selected Google Calendar projection does not cover the requested horizon")]
    Incomplete,
    #[error("Google Calendar projection evidence is temporarily unavailable")]
    Unavailable,
}
