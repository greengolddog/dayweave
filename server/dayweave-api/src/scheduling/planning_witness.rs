//! Authenticated current-source qualification, not a publication capability.
//! Every capture is rolled back: no first admission, rebase, receipt or proof
//! is written, and no caller terminal checkpoint is advanced by this endpoint.
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    io,
};

use dayweave_compose::{CanonicalItem, ComposeScheduleRequest, prepare_canonical_schedule};
use dayweave_core::OccurrenceLifecycleContext;
use dayweave_scheduler_helper::{PlanPreflightError, preflight_plan_request};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::{PgPool, Postgres, Transaction};

use crate::{
    items::ItemRepositoryError,
    persistence::{
        AuthoritativeHabitRecurrence, DatabaseScope, authoritative_habit_recurrence_tx,
        list_active_completion_items_tx, lock_habit_change_space, lock_routine_occurrence_space,
        lock_routine_planning_witness_owner_tx, lock_routine_planning_witness_sources_tx,
        routine_occurrence_planning_evidence_tx, routine_occurrence_terminal_head_tx,
    },
    routine_occurrences::{
        RoutineOccurrenceError, RoutinePlanningRemoteReason as RemoteReason,
        RoutinePlanningWitness, RoutinePlanningWitnessError as WitnessError,
        RoutinePlanningWitnessRequest, RoutinePlanningWitnessResponse,
        RoutinePlanningWitnessResult,
    },
};

use super::{
    CalendarProjectionStamp, ComposeScheduleError, ComposeScheduleResult,
    compose::{
        compose_items_with_lifecycle_for_schema, contained_moved_occurrence_ids,
        discard_managed_occurrence_claims, into_canonical_item,
        normalize_authoritative_schedule_request, publication_schema_for_lifecycle,
    },
    occurrence::{lifecycle_from_evidence, planning_identities},
    postgres::{
        AuthoritativePlanningEvidence, SchedulePublicationError,
        authoritative_planning_evidence_tx, capture_current_calendar_projection_tx,
    },
};

#[cfg(test)]
#[path = "planning_witness_fixtures.rs"]
mod shared_fixtures;

#[derive(Debug)]
enum CaptureFailure {
    Error(WitnessError),
    Remote(RemoteReason),
}

impl From<WitnessError> for CaptureFailure {
    fn from(error: WitnessError) -> Self {
        Self::Error(error)
    }
}

pub(super) async fn capture(
    pool: &PgPool,
    scope: DatabaseScope,
    request: &RoutinePlanningWitnessRequest,
) -> Result<RoutinePlanningWitnessResponse, WitnessError> {
    request.validate()?;
    let mut tx = pool.begin().await.map_err(|_| WitnessError::Unavailable)?;
    let result = capture_locked(&mut tx, scope, request).await;
    // Explicit rollback also releases all capture locks before returning a
    // response. There is deliberately no durable witness/lease table.
    tx.rollback().await.map_err(|_| WitnessError::Unavailable)?;
    let result = match result {
        Ok(witness) => RoutinePlanningWitnessResult::Qualified {
            witness: Box::new(witness),
        },
        Err(CaptureFailure::Remote(reason)) => {
            RoutinePlanningWitnessResult::RemoteRequired { reason }
        }
        Err(CaptureFailure::Error(error)) => return Err(error),
    };
    let response = RoutinePlanningWitnessResponse {
        schema_version: 1,
        result,
    };
    response.validate_size()?;
    Ok(response)
}

#[allow(clippy::too_many_lines)] // Keep lock order, complete capture and final rechecks visible together.
async fn capture_locked(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    request: &RoutinePlanningWitnessRequest,
) -> Result<RoutinePlanningWitness, CaptureFailure> {
    lock_routine_planning_witness_sources_tx(tx, scope)
        .await
        .map_err(map_occurrence)?;
    lock_habit_change_space(tx, scope.workspace_id)
        .await
        .map_err(unavailable)?;
    lock_routine_occurrence_space(tx, scope.workspace_id)
        .await
        .map_err(map_occurrence)?;
    lock_routine_planning_witness_owner_tx(tx, scope)
        .await
        .map_err(map_occurrence)?;

    let items = list_active_completion_items_tx(tx, scope.workspace_id)
        .await
        .map_err(|error| {
            CaptureFailure::Error(match error {
                ItemRepositoryError::DeltaGroupTooLarge
                | ItemRepositoryError::BootstrapTooLarge => WitnessError::TooLarge,
                _ => WitnessError::Unavailable,
            })
        })?;
    let source_item_revisions = items
        .iter()
        .map(|item| (item.id, item.revision))
        .collect::<BTreeMap<_, _>>();
    if request.expected_source_item_revisions != source_item_revisions {
        return Err(WitnessError::SourceChanged.into());
    }
    let head = routine_occurrence_terminal_head_tx(tx, scope, &request.terminal_cursor)
        .await
        .map_err(map_occurrence)?;
    let planning = authoritative_planning_evidence_tx(tx, scope.workspace_id)
        .await
        .map_err(unavailable)?;
    require_representable_authority(&request.schedule, &planning)?;
    let moved = contained_moved_occurrence_ids(&request.schedule);
    let habit = authoritative_habit_recurrence_tx(
        tx,
        scope.workspace_id,
        request.schedule.horizon_start,
        request.schedule.horizon_end,
        &moved,
    )
    .await
    .map_err(unavailable)?;
    let calendar = capture_current_calendar_projection_tx(
        tx,
        scope,
        request.schedule.horizon_start,
        request.schedule.horizon_end,
    )
    .await
    .map_err(map_calendar)?;

    let mut schedule = request.schedule.clone();
    let normalized =
        normalize_authoritative_schedule_request(&mut schedule, &items, Some(&habit), &planning)
            .map_err(|error| map_compose(&error))?;
    let mut probe = schedule.clone();
    probe.recurrence_context.completed_occurrence_ids.clear();
    probe.recurrence_context.partial_progress.clear();
    let canonical_items = items
        .iter()
        .cloned()
        .map(into_canonical_item)
        .collect::<Vec<_>>();
    let prepared = prepare_canonical_schedule(canonical_items.clone(), probe)
        .map_err(|_| CaptureFailure::Remote(RemoteReason::SourceIneligible))?;
    if !prepared.rejected_items.is_empty() {
        return Err(CaptureFailure::Remote(RemoteReason::SourceIneligible));
    }
    preflight_plan_request(&prepared.plan_request).map_err(|error| map_preflight(&error))?;
    let identities = planning_identities(&prepared.plan_request).map_err(map_occurrence)?;
    let evidence = routine_occurrence_planning_evidence_tx(tx, scope, &identities)
        .await
        .map_err(map_occurrence)?;
    if evidence.change_head != head {
        return Err(WitnessError::CursorChanged.into());
    }
    let captured = evidence
        .instances
        .iter()
        .map(|instance| {
            (
                instance.aggregate.manifest.series_item_id,
                instance.aggregate.manifest.occurrence_id,
            )
        })
        .collect::<BTreeSet<_>>();
    if captured != identities.into_iter().collect() {
        return Err(CaptureFailure::Remote(
            RemoteReason::FirstPublicationRequired,
        ));
    }
    let lifecycle = lifecycle_from_evidence(evidence).map_err(map_occurrence)?;
    if lifecycle
        .instances
        .iter()
        .flat_map(|instance| &instance.members)
        .any(|member| source_item_revisions.get(&member.item_id.0) != Some(&member.source_revision))
    {
        return Err(WitnessError::SourceChanged.into());
    }
    discard_managed_occurrence_claims(&mut schedule, &lifecycle);
    let composition = compose_items_with_lifecycle_for_schema(
        items,
        schedule.clone(),
        publication_schema_for_lifecycle(&lifecycle),
        calendar.clone(),
        planning.clone(),
        habit.change_head,
        normalized.untrusted_assignments,
        lifecycle.clone(),
    )
    .map_err(|error| map_compose(&error))?;
    let local_input_fingerprint =
        require_helper_parity(&canonical_items, &schedule, &lifecycle, &composition)?;

    // A Calendar freshness interval may expire while bounded composition runs.
    // Reuse publication's precise policy, not a weaker stamp-only comparison.
    let calendar_after = capture_current_calendar_projection_tx(
        tx,
        scope,
        request.schedule.horizon_start,
        request.schedule.horizon_end,
    )
    .await
    .map_err(map_calendar)?;
    if calendar_after != calendar {
        return Err(CaptureFailure::Remote(
            RemoteReason::CalendarProjectionIncomplete,
        ));
    }
    if authoritative_planning_evidence_tx(tx, scope.workspace_id)
        .await
        .map_err(unavailable)?
        != planning
        || authoritative_habit_recurrence_tx(
            tx,
            scope.workspace_id,
            request.schedule.horizon_start,
            request.schedule.horizon_end,
            &moved,
        )
        .await
        .map_err(unavailable)?
            != habit
    {
        return Err(WitnessError::Unavailable.into());
    }
    if routine_occurrence_terminal_head_tx(tx, scope, &request.terminal_cursor)
        .await
        .map_err(map_occurrence)?
        != head
    {
        return Err(WitnessError::CursorChanged.into());
    }
    // Full current-source equality is held by the canonical mutex throughout;
    // reread the bounded forest as a fail-closed check across all awaited work.
    let current = list_active_completion_items_tx(tx, scope.workspace_id)
        .await
        .map_err(unavailable)?;
    if current
        .iter()
        .map(|item| (item.id, item.revision))
        .collect::<std::collections::BTreeMap<_, _>>()
        != source_item_revisions
    {
        return Err(WitnessError::SourceChanged.into());
    }
    make_witness(
        scope,
        request,
        schedule,
        lifecycle,
        local_input_fingerprint,
        &canonical_items,
        &planning,
        &habit,
        &calendar,
        &composition,
    )
}

fn require_representable_authority(
    schedule: &ComposeScheduleRequest,
    planning: &AuthoritativePlanningEvidence,
) -> Result<(), CaptureFailure> {
    // v2's default execution context cannot express credits, dispositions,
    // consumed physical indices or reservations, even with no active timer.
    if !planning.execution.work_units.is_empty() {
        return Err(CaptureFailure::Remote(
            RemoteReason::ExecutionEvidenceRequired,
        ));
    }
    if !schedule.manual_placements.is_empty()
        || !schedule.manual_placement_releases.is_empty()
        || !planning.retained_manual_placements.is_empty()
    {
        return Err(CaptureFailure::Remote(
            RemoteReason::RetainedManualPlacementRequired,
        ));
    }
    Ok(())
}

/// Exercise the exact closed helper-v2 boundary in-process, including complete
/// Inbox/member joins and resource bounds. No external helper is invoked. A
/// matching plan alone is insufficient: require all source accounting too.
fn require_helper_parity(
    canonical_items: &[CanonicalItem],
    schedule: &ComposeScheduleRequest,
    lifecycle: &OccurrenceLifecycleContext,
    composition: &ComposeScheduleResult,
) -> Result<String, CaptureFailure> {
    let prepared = prepare_canonical_schedule(canonical_items.to_vec(), schedule.clone())
        .map_err(|_| CaptureFailure::Remote(RemoteReason::CompositionUnsupported))?;
    if prepared.plan_request != composition.planning_request {
        return Err(CaptureFailure::Remote(RemoteReason::CompositionUnsupported));
    }
    let envelope = json!({
        "protocol":"dayweave.scheduler.helper", "version":2, "operation":"compose",
        "request": { "canonical_items":canonical_items, "schedule":schedule, "occurrence_lifecycle":lifecycle }
    });
    let mut input = BoundedBuffer(Vec::new());
    serde_json::to_writer(&mut input, &envelope).map_err(|_| WitnessError::TooLarge)?;
    let output = dayweave_scheduler_helper::process_bytes(&input.0);
    let decoded: Value = serde_json::from_slice(&output.stdout).map_err(unavailable)?;
    if output.exit_code != 0 {
        return Err(match decoded["result"]["error"]["code"].as_str() {
            Some("request_too_large" | "resource_limit_exceeded" | "response_too_large") => {
                WitnessError::TooLarge.into()
            }
            _ => CaptureFailure::Remote(RemoteReason::CompositionUnsupported),
        });
    }
    let helper = &decoded["result"]["composition"];
    let expected = serde_json::to_value(composition).map_err(unavailable)?;
    if decoded["protocol"] != "dayweave.scheduler.helper"
        || decoded["version"] != 2
        || decoded["result"]["type"] != "composition"
        || helper["occurrence_snapshot_revision"] != lifecycle.snapshot_revision
        || helper["ignored_previous_assignments"]
            != serde_json::to_value(&prepared.ignored_previous_assignments).map_err(unavailable)?
        || [
            "source_item_count",
            "source_item_revisions",
            "accepted_item_count",
            "rejected_items",
            "plan",
        ]
        .into_iter()
        .any(|key| helper[key] != expected[key])
    {
        return Err(CaptureFailure::Remote(RemoteReason::CompositionUnsupported));
    }
    let fingerprint = helper["local_input_fingerprint"]
        .as_str()
        .filter(|value| {
            value.strip_prefix("local-sha256:").is_some_and(|hex| {
                hex.len() == 64
                    && hex
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        })
        .ok_or(CaptureFailure::Remote(RemoteReason::CompositionUnsupported))?;
    Ok(fingerprint.to_owned())
}

#[allow(clippy::too_many_arguments)] // The fingerprint explicitly binds each separate captured authority.
fn make_witness(
    scope: DatabaseScope,
    request: &RoutinePlanningWitnessRequest,
    schedule: ComposeScheduleRequest,
    occurrence_lifecycle: OccurrenceLifecycleContext,
    local_input_fingerprint: String,
    canonical_items: &[CanonicalItem],
    planning: &AuthoritativePlanningEvidence,
    habit: &AuthoritativeHabitRecurrence,
    calendar: &[CalendarProjectionStamp],
    composition: &ComposeScheduleResult,
) -> Result<RoutinePlanningWitness, CaptureFailure> {
    let scope_binding = (scope.workspace_id, scope.user_id);
    let request_fingerprint = fingerprint("request", &(scope_binding, request))?;
    let calendar_projection_fingerprint = fingerprint("calendar", &(scope_binding, calendar))?;
    let mut witness = RoutinePlanningWitness {
        workspace_id: scope.workspace_id,
        user_id: scope.user_id,
        request_fingerprint,
        witness_fingerprint: String::new(),
        calendar_projection_fingerprint,
        local_input_fingerprint,
        source_item_revisions: composition.source_item_revisions.clone(),
        terminal_cursor: request.terminal_cursor.clone(),
        schedule,
        occurrence_lifecycle,
        execution_snapshot_revision: planning.execution.snapshot_revision,
        habit_change_head: habit.change_head,
        published_schedule_revision_id: planning.published_revision_id,
    };
    witness.witness_fingerprint = fingerprint(
        "capture",
        &(
            &witness,
            request,
            canonical_items,
            planning,
            &habit.context,
            calendar,
            &composition.planning_request,
        ),
    )?;
    Ok(witness)
}

/// These domains/prefixes are intentionally incompatible with publication's
/// sha256 digest and the helper's local-sha256 fingerprint.
fn fingerprint(kind: &str, value: &impl Serialize) -> Result<String, CaptureFailure> {
    struct HashWriter(Sha256);
    impl io::Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(
        &mut writer,
        &("dayweave.routine-planning-witness.v1", kind, value),
    )
    .map_err(unavailable)?;
    let mut encoded = format!("routine-witness-{kind}-sha256:");
    for byte in writer.0.finalize() {
        write!(&mut encoded, "{byte:02x}").map_err(unavailable)?;
    }
    Ok(encoded)
}

struct BoundedBuffer(Vec<u8>);
impl io::Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|size| size > dayweave_scheduler_helper::MAX_INPUT_BYTES)
        {
            return Err(io::Error::other("helper input exceeds byte budget"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn unavailable<T>(_: T) -> CaptureFailure {
    WitnessError::Unavailable.into()
}

fn map_occurrence(error: RoutineOccurrenceError) -> CaptureFailure {
    match error {
        RoutineOccurrenceError::InvalidCursor => WitnessError::CursorChanged.into(),
        RoutineOccurrenceError::TooLarge => WitnessError::TooLarge.into(),
        RoutineOccurrenceError::DefinitionChanged
        | RoutineOccurrenceError::SourceIneligible
        | RoutineOccurrenceError::OccurrenceEvidenceRequired => {
            CaptureFailure::Remote(RemoteReason::SourceIneligible)
        }
        _ => WitnessError::Unavailable.into(),
    }
}

fn map_calendar(error: SchedulePublicationError) -> CaptureFailure {
    match error {
        SchedulePublicationError::StaleComposition => {
            CaptureFailure::Remote(RemoteReason::CalendarProjectionIncomplete)
        }
        _ => WitnessError::Unavailable.into(),
    }
}

fn map_compose(error: &ComposeScheduleError) -> CaptureFailure {
    match error {
        ComposeScheduleError::TooManyItems | ComposeScheduleError::SchedulerResourceLimit => {
            WitnessError::TooLarge.into()
        }
        ComposeScheduleError::CalendarProjectionIncomplete => {
            CaptureFailure::Remote(RemoteReason::CalendarProjectionIncomplete)
        }
        ComposeScheduleError::AuthoritativeManualPlacementChanged(_) => {
            CaptureFailure::Remote(RemoteReason::RetainedManualPlacementRequired)
        }
        ComposeScheduleError::InvalidRequest(_) | ComposeScheduleError::Scheduler(_) => {
            CaptureFailure::Remote(RemoteReason::CompositionUnsupported)
        }
        _ => WitnessError::Unavailable.into(),
    }
}

fn map_preflight(error: &PlanPreflightError) -> CaptureFailure {
    match error {
        PlanPreflightError::ResourceLimitExceeded => WitnessError::TooLarge.into(),
        _ => CaptureFailure::Remote(RemoteReason::CompositionUnsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::{Item, NewItem};
    use dayweave_core::{ExecutionWorkUnit, ItemId};
    use uuid::Uuid;

    fn schedule() -> ComposeScheduleRequest {
        serde_json::from_value(json!({
            "as_of":"2026-09-10T09:00:00Z", "horizon_start":"2026-09-10T09:00:00Z",
            "horizon_end":"2026-09-11T09:00:00Z", "timezone_name":"UTC"
        }))
        .unwrap()
    }

    fn items() -> Vec<Item> {
        ["planned", "inbox"]
            .into_iter()
            .enumerate()
            .map(|(index, status)| {
                let input: NewItem = serde_json::from_value(json!({
                    "id":Uuid::from_u128(index as u128 + 1), "is_sensitive":true,
                    "kind":"task", "status":status, "title":"Synthetic private source",
                    "notes":null, "timezone_name":"UTC", "duration_seconds":300,
                    "deadline_at":null, "earliest_start_at":null, "recurrence":null,
                    "flexible_constraints":{}, "split_policy":{"type":"indivisible"},
                    "importance":1, "urgency":1, "parent_id":null, "sibling_order":0
                }))
                .unwrap();
                Item::new(input, schedule().as_of).unwrap()
            })
            .collect()
    }

    fn composition(
        head: u64,
    ) -> (
        Vec<CanonicalItem>,
        OccurrenceLifecycleContext,
        ComposeScheduleResult,
    ) {
        let sources = items();
        let canonical = sources.iter().cloned().map(into_canonical_item).collect();
        let lifecycle = OccurrenceLifecycleContext {
            snapshot_revision: head,
            instances: Vec::new(),
        };
        let composition = compose_items_with_lifecycle_for_schema(
            sources,
            schedule(),
            publication_schema_for_lifecycle(&lifecycle),
            Vec::new(),
            AuthoritativePlanningEvidence::default(),
            0,
            Vec::new(),
            lifecycle.clone(),
        )
        .unwrap();
        (canonical, lifecycle, composition)
    }

    #[test]
    fn helper_parity_retains_positive_empty_lifecycle_head() {
        let (canonical, lifecycle, composed) = composition(9);
        let fingerprint =
            require_helper_parity(&canonical, &schedule(), &lifecycle, &composed).unwrap();
        assert!(fingerprint.starts_with("local-sha256:"));
        assert_eq!(
            composed.publication_schema(),
            super::super::OCCURRENCE_PUBLICATION_SCHEMA
        );
        let (_, empty, initial) = composition(0);
        assert_eq!(
            serde_json::to_value(&initial.plan).unwrap(),
            serde_json::to_value(&composed.plan).unwrap()
        );
        assert_ne!(
            fingerprint,
            require_helper_parity(&canonical, &schedule(), &empty, &initial).unwrap()
        );
    }

    #[test]
    fn helper_parity_requires_inbox_revisions_and_exact_source_accounting() {
        let (mut canonical, lifecycle, mut composed) = composition(1);
        canonical[1].revision += 1;
        assert!(matches!(
            require_helper_parity(&canonical, &schedule(), &lifecycle, &composed),
            Err(CaptureFailure::Remote(RemoteReason::CompositionUnsupported))
        ));
        canonical[1].revision -= 1;
        composed.accepted_item_count -= 1;
        assert!(matches!(
            require_helper_parity(&canonical, &schedule(), &lifecycle, &composed),
            Err(CaptureFailure::Remote(RemoteReason::CompositionUnsupported))
        ));
    }

    #[test]
    fn helper_parity_rejects_plan_equivalent_but_different_normalized_input() {
        let (canonical, lifecycle, composed) = composition(1);
        let mut changed = schedule();
        changed.config.stability_weight += 1;
        assert!(matches!(
            require_helper_parity(&canonical, &changed, &lifecycle, &composed),
            Err(CaptureFailure::Remote(RemoteReason::CompositionUnsupported))
        ));
    }

    #[test]
    fn inactive_execution_history_is_not_default_execution() {
        let mut planning = AuthoritativePlanningEvidence::default();
        planning.execution.snapshot_revision = 8;
        assert!(require_representable_authority(&schedule(), &planning).is_ok());
        planning.execution.work_units.push(ExecutionWorkUnit {
            item_id: ItemId(Uuid::from_u128(1)),
            occurrence_id: None,
            progress_epoch: 1,
            credited_seconds: 0,
            disposition: None,
            used_session_indices: vec![0],
            reservations: Vec::new(),
        });
        assert!(matches!(
            require_representable_authority(&schedule(), &planning),
            Err(CaptureFailure::Remote(
                RemoteReason::ExecutionEvidenceRequired
            ))
        ));
    }

    #[test]
    fn explicit_manual_release_requires_remote_policy() {
        let mut request = schedule();
        request
            .manual_placement_releases
            .push(dayweave_compose::ManualPlacementReleaseInput {
                id: Uuid::from_u128(10),
                placement_id: Uuid::from_u128(11),
                source_schedule_revision_id: Uuid::from_u128(12),
            });
        assert!(matches!(
            require_representable_authority(&request, &AuthoritativePlanningEvidence::default()),
            Err(CaptureFailure::Remote(
                RemoteReason::RetainedManualPlacementRequired
            ))
        ));
    }

    #[test]
    fn retained_manual_policy_requires_remote_even_when_caller_policy_is_empty() {
        let request = schedule();
        assert!(request.manual_placements.is_empty());
        assert!(request.manual_placement_releases.is_empty());
        assert!(
            require_representable_authority(&request, &AuthoritativePlanningEvidence::default())
                .is_ok()
        );
        let revision_id = Uuid::from_u128(10);
        let retained = super::super::postgres::PersistedManualPlacementState {
            placement: dayweave_compose::ManualPlacementInput {
                id: Uuid::from_u128(11),
                source_schedule_revision_id: Some(revision_id),
                assignments: vec![dayweave_compose::ManualPlacementAssignmentInput {
                    item_id: Uuid::from_u128(1),
                    item_revision: 1,
                    occurrence_id: None,
                    blocks: vec![dayweave_compose::PreviousBlockInput {
                        start: request.as_of,
                        end: request.as_of + chrono::Duration::minutes(5),
                        session_index: 0,
                    }],
                }],
            },
            environment_digest: format!("sha256:{}", "11".repeat(32)),
            assessment_digest: format!("sha256:{}", "22".repeat(32)),
            authorized_violations: Vec::new(),
            authorization: super::super::postgres::ManualPlacementAuthorization::ConflictFree,
        };
        let planning = AuthoritativePlanningEvidence {
            published_revision_id: Some(revision_id),
            retained_manual_placements: vec![retained],
            ..AuthoritativePlanningEvidence::default()
        };
        assert!(matches!(
            require_representable_authority(&request, &planning),
            Err(CaptureFailure::Remote(
                RemoteReason::RetainedManualPlacementRequired
            ))
        ));
    }

    #[test]
    #[allow(clippy::too_many_lines)] // Keep request, scope and captured-authority fingerprint comparisons in one regression.
    fn witness_domains_bind_full_request_scope_and_captured_authority() {
        let (canonical, lifecycle, composed) = composition(1);
        let local = require_helper_parity(&canonical, &schedule(), &lifecycle, &composed).unwrap();
        let scope = DatabaseScope {
            workspace_id: Uuid::from_u128(30),
            user_id: Uuid::from_u128(31),
        };
        let request = RoutinePlanningWitnessRequest {
            schema_version: 1,
            schedule: schedule(),
            expected_source_item_revisions: composed.source_item_revisions.clone(),
            terminal_cursor: "synthetic-terminal".to_owned(),
        };
        let mut planning = AuthoritativePlanningEvidence::default();
        let habit = AuthoritativeHabitRecurrence::default();
        let first = make_witness(
            scope,
            &request,
            schedule(),
            lifecycle.clone(),
            local.clone(),
            &canonical,
            &planning,
            &habit,
            &[],
            &composed,
        )
        .unwrap();
        assert!(
            first
                .request_fingerprint
                .starts_with("routine-witness-request-sha256:")
        );
        assert!(
            first
                .witness_fingerprint
                .starts_with("routine-witness-capture-sha256:")
        );
        assert!(
            first
                .calendar_projection_fingerprint
                .starts_with("routine-witness-calendar-sha256:")
        );
        assert_ne!(first.witness_fingerprint, composed.input_digest);
        planning.execution.snapshot_revision = 3;
        let second = make_witness(
            scope,
            &request,
            schedule(),
            lifecycle.clone(),
            local.clone(),
            &canonical,
            &planning,
            &habit,
            &[],
            &composed,
        )
        .unwrap();
        assert_eq!(
            first.local_input_fingerprint,
            second.local_input_fingerprint
        );
        assert_eq!(first.request_fingerprint, second.request_fingerprint);
        assert_ne!(first.witness_fingerprint, second.witness_fingerprint);
        let mut changed = request.clone();
        changed.schedule.config.stability_weight += 1;
        let third = make_witness(
            scope,
            &changed,
            schedule(),
            lifecycle.clone(),
            local.clone(),
            &canonical,
            &planning,
            &habit,
            &[],
            &composed,
        )
        .unwrap();
        assert_ne!(second.request_fingerprint, third.request_fingerprint);
        assert_ne!(second.witness_fingerprint, third.witness_fingerprint);
        let foreign = DatabaseScope {
            user_id: Uuid::from_u128(32),
            ..scope
        };
        let fourth = make_witness(
            foreign,
            &request,
            schedule(),
            lifecycle,
            local,
            &canonical,
            &planning,
            &habit,
            &[],
            &composed,
        )
        .unwrap();
        assert_ne!(second.request_fingerprint, fourth.request_fingerprint);
        assert_ne!(
            second.calendar_projection_fingerprint,
            fourth.calendar_projection_fingerprint
        );
        assert_ne!(second.witness_fingerprint, fourth.witness_fingerprint);
    }

    #[test]
    fn qualification_errors_do_not_turn_missing_or_stale_authority_into_empty_context() {
        assert!(matches!(
            map_occurrence(RoutineOccurrenceError::DefinitionChanged),
            CaptureFailure::Remote(RemoteReason::SourceIneligible)
        ));
        assert!(matches!(
            map_occurrence(RoutineOccurrenceError::InvalidCursor),
            CaptureFailure::Error(WitnessError::CursorChanged)
        ));
        assert!(matches!(
            map_occurrence(RoutineOccurrenceError::Unavailable),
            CaptureFailure::Error(WitnessError::Unavailable)
        ));
    }
}
