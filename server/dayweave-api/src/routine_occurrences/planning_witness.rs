//! Versioned, private input qualification for occurrence-aware local planning.
//! A witness is not a publication digest, execution permit or persisted lease.
use std::{collections::BTreeMap, fmt, io};

use dayweave_compose::{MAX_CANONICAL_ITEMS, validate_schedule_request};
use dayweave_core::OccurrenceLifecycleContext;
use serde::{Deserialize, Deserializer, Serialize, de};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::scheduling::ComposeScheduleRequest;

pub const ROUTINE_PLANNING_WITNESS_BYTES: usize = 16 * 1024 * 1024;
const MAX_CURSOR_BYTES: usize = 4096;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutinePlanningWitnessRequest {
    pub schema_version: u16,
    pub schedule: ComposeScheduleRequest,
    /// Exact complete active canonical snapshot, not just scheduled members.
    #[serde(deserialize_with = "unique_source_revisions")]
    pub expected_source_item_revisions: BTreeMap<Uuid, u64>,
    /// Terminal occurrence-list/delta checkpoint already installed by the client.
    pub terminal_cursor: String,
}

impl RoutinePlanningWitnessRequest {
    /// Rejects malformed or oversized requests before any authority is read.
    ///
    /// # Errors
    /// Returns `Invalid` for unsupported shapes or nonportable values and
    /// `TooLarge` for the bounded source map, cursor or serialized body.
    pub fn validate(&self) -> Result<(), RoutinePlanningWitnessError> {
        if self.expected_source_item_revisions.len() > MAX_CANONICAL_ITEMS
            || self.terminal_cursor.len() > MAX_CURSOR_BYTES
        {
            return Err(RoutinePlanningWitnessError::TooLarge);
        }
        if self.schema_version != 1
            || self.terminal_cursor.is_empty()
            || !self.terminal_cursor.is_ascii()
            || self
                .expected_source_item_revisions
                .iter()
                .any(|(id, revision)| id.is_nil() || *revision == 0 || *revision > i64::MAX as u64)
        {
            return Err(RoutinePlanningWitnessError::Invalid);
        }
        validate_schedule_request(&self.schedule)
            .map_err(|_| RoutinePlanningWitnessError::Invalid)?;
        validate_wire_size(self)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutinePlanningWitnessResponse {
    pub schema_version: u16,
    pub result: RoutinePlanningWitnessResult,
}

impl RoutinePlanningWitnessResponse {
    pub(crate) fn validate_size(&self) -> Result<(), RoutinePlanningWitnessError> {
        validate_wire_size(self)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoutinePlanningWitnessResult {
    Qualified {
        witness: Box<RoutinePlanningWitness>,
    },
    RemoteRequired {
        reason: RoutinePlanningRemoteReason,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoutinePlanningWitness {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    /// Non-publishable binding of the complete original request and owner scope.
    pub request_fingerprint: String,
    /// Non-publishable binding of all qualified inputs and captured authority.
    pub witness_fingerprint: String,
    /// Expected helper-v2 fingerprint for an independent native computation.
    pub local_input_fingerprint: String,
    /// Opaque content-free binding of the selected Calendar generations.
    pub calendar_projection_fingerprint: String,
    pub source_item_revisions: BTreeMap<Uuid, u64>,
    pub terminal_cursor: String,
    /// Authoritatively normalized input for helper v2; never an implicit edit.
    pub schedule: ComposeScheduleRequest,
    #[schema(value_type = Object)]
    pub occurrence_lifecycle: OccurrenceLifecycleContext,
    pub execution_snapshot_revision: u64,
    pub habit_change_head: u64,
    pub published_schedule_revision_id: Option<Uuid>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoutinePlanningRemoteReason {
    FirstPublicationRequired,
    ExecutionEvidenceRequired,
    RetainedManualPlacementRequired,
    SourceIneligible,
    CalendarProjectionIncomplete,
    CompositionUnsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RoutinePlanningWitnessError {
    #[error("Invalid routine planning request")]
    Invalid,
    #[error("Routine planning evidence exceeds resource bounds")]
    TooLarge,
    #[error("Canonical planning sources changed")]
    SourceChanged,
    #[error("Terminal occurrence checkpoint changed")]
    CursorChanged,
    #[error("Routine planning authority is unavailable")]
    Unavailable,
}

/// UUID spellings that decode to the same identity must not overwrite a source.
fn unique_source_revisions<'de, D>(deserializer: D) -> Result<BTreeMap<Uuid, u64>, D::Error>
where
    D: Deserializer<'de>,
{
    struct Sources;
    impl<'de> de::Visitor<'de> for Sources {
        type Value = BTreeMap<Uuid, u64>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a bounded unique source revision map")
        }

        fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
        where
            M: de::MapAccess<'de>,
        {
            let mut sources = BTreeMap::new();
            while let Some((id, revision)) = map.next_entry::<Uuid, u64>()? {
                if sources.len() >= MAX_CANONICAL_ITEMS || sources.insert(id, revision).is_some() {
                    return Err(de::Error::custom("Invalid source revision map"));
                }
            }
            Ok(sources)
        }
    }
    deserializer.deserialize_map(Sources)
}

fn validate_wire_size(value: &impl Serialize) -> Result<(), RoutinePlanningWitnessError> {
    #[derive(Default)]
    struct WireBudget(usize);
    impl io::Write for WireBudget {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|size| *size <= ROUTINE_PLANNING_WITNESS_BYTES)
                .ok_or_else(|| io::Error::other("Routine planning wire budget exceeded"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(WireBudget::default(), value)
        .map_err(|_| RoutinePlanningWitnessError::TooLarge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn request_value() -> Value {
        json!({"schema_version":1,"schedule":{
            "as_of":"2026-09-10T09:00:00Z", "horizon_start":"2026-09-10T09:00:00Z",
            "horizon_end":"2026-09-11T09:00:00Z", "timezone_name":"UTC"},
            "expected_source_item_revisions":{Uuid::from_u128(1).to_string():1},
            "terminal_cursor":"synthetic-terminal-cursor"})
    }

    #[test]
    fn witness_request_requires_closed_shape_and_exact_integer_revisions() {
        let value = request_value();
        serde_json::from_value::<RoutinePlanningWitnessRequest>(value.clone())
            .unwrap()
            .validate()
            .unwrap();
        for field in [
            "schema_version",
            "schedule",
            "expected_source_item_revisions",
            "terminal_cursor",
        ] {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<RoutinePlanningWitnessRequest>(missing).is_err());
        }
        let mut extra = value.clone();
        extra["future"] = json!(true);
        assert!(serde_json::from_value::<RoutinePlanningWitnessRequest>(extra).is_err());
        for number in [json!(true), json!(1.0), json!("1"), json!(-1)] {
            let mut invalid = value.clone();
            invalid["expected_source_item_revisions"][Uuid::from_u128(1).to_string()] = number;
            assert!(serde_json::from_value::<RoutinePlanningWitnessRequest>(invalid).is_err());
        }
    }

    #[test]
    fn witness_source_aliases_and_duplicates_cannot_overwrite_revisions() {
        let mut value = request_value();
        value["expected_source_item_revisions"] = json!({
            "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa":1,
            "AAAAAAAA-AAAA-4AAA-AAAA-AAAAAAAAAAAA":2});
        assert!(serde_json::from_value::<RoutinePlanningWitnessRequest>(value).is_err());
        let source = request_value().to_string();
        let duplicate = source.replace(
            "00000000-0000-0000-0000-000000000001\":1",
            "00000000-0000-0000-0000-000000000001\":1,\"00000000-0000-0000-0000-000000000001\":2",
        );
        assert!(serde_json::from_str::<RoutinePlanningWitnessRequest>(&duplicate).is_err());
    }

    #[test]
    fn witness_validation_rejects_nonportable_sources_and_cursor_bounds() {
        let base: RoutinePlanningWitnessRequest = serde_json::from_value(request_value()).unwrap();
        for revision in [0, i64::MAX as u64 + 1] {
            let mut invalid = base.clone();
            invalid
                .expected_source_item_revisions
                .insert(Uuid::from_u128(1), revision);
            assert_eq!(
                invalid.validate(),
                Err(RoutinePlanningWitnessError::Invalid)
            );
        }
        let mut invalid = base.clone();
        invalid
            .expected_source_item_revisions
            .insert(Uuid::nil(), 1);
        assert_eq!(
            invalid.validate(),
            Err(RoutinePlanningWitnessError::Invalid)
        );
        for cursor in [String::new(), "non-ascii-⏱".to_owned()] {
            let mut invalid = base.clone();
            invalid.terminal_cursor = cursor;
            assert_eq!(
                invalid.validate(),
                Err(RoutinePlanningWitnessError::Invalid)
            );
        }
        let mut invalid = base.clone();
        invalid.terminal_cursor = "x".repeat(MAX_CURSOR_BYTES + 1);
        assert_eq!(
            invalid.validate(),
            Err(RoutinePlanningWitnessError::TooLarge)
        );
        let mut invalid = base;
        invalid.schema_version = 2;
        assert_eq!(
            invalid.validate(),
            Err(RoutinePlanningWitnessError::Invalid)
        );
    }

    #[test]
    fn witness_validation_keeps_compose_and_serialized_budgets() {
        let mut request: RoutinePlanningWitnessRequest =
            serde_json::from_value(request_value()).unwrap();
        request.schedule.horizon_end = request.schedule.horizon_start;
        assert_eq!(
            request.validate(),
            Err(RoutinePlanningWitnessError::Invalid)
        );
        assert_eq!(
            validate_wire_size(&"x".repeat(ROUTINE_PLANNING_WITNESS_BYTES)),
            Err(RoutinePlanningWitnessError::TooLarge)
        );
        assert!(validate_wire_size(&"x".repeat(ROUTINE_PLANNING_WITNESS_BYTES - 2)).is_ok());
    }

    #[test]
    fn witness_remote_required_is_typed_closed_and_has_no_planning_authority() {
        let response = RoutinePlanningWitnessResponse {
            schema_version: 1,
            result: RoutinePlanningWitnessResult::RemoteRequired {
                reason: RoutinePlanningRemoteReason::ExecutionEvidenceRequired,
            },
        };
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(
            value,
            json!({"schema_version":1,"result":{"status":"remote_required","reason":"execution_evidence_required"}})
        );
        for extra in [
            json!({"status":"remote_required","reason":"execution_evidence_required","witness":{}}),
            json!({"status":"remote_required","reason":"unknown"}),
        ] {
            assert!(serde_json::from_value::<RoutinePlanningWitnessResult>(extra).is_err());
        }
    }
}
