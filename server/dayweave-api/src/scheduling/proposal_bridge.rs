use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::{
    items::{DeadlineKind, DurationKind, Item, ItemKind, ItemStatus, NewItem, ReplaceItem},
    proposals::{
        NewProposal, Proposal, ProposalChangeSet, ProposalCommand, ProposalKind, ProposalSource,
    },
};

use super::{
    PlanOperation, PlanOperationKind, ProposalSubmissionSpec, SchedulingPortError,
    SimulationProposalEvidence, has_postgres_timestamp_precision,
};

const UPDATE_ITEM_FIELDS: &[&str] = &[
    "kind",
    "status",
    "title",
    "notes",
    "timezone_name",
    "duration_kind",
    "duration_seconds",
    "duration_min_seconds",
    "duration_max_seconds",
    "duration_source",
    "deadline_kind",
    "deadline_date",
    "deadline_at",
    "deadline_strength",
    "deadline_soft_weight",
    "earliest_start_at",
    "recurrence",
    "flexible_constraints",
    "has_own_effort",
    "split_policy",
    "importance",
    "urgency",
    "parent_id",
    "sibling_order",
    "blocked_reason_kind",
    "blocked_by_item_id",
    "blocked_reason",
];

const UPDATE_CONSTRAINT_FIELDS: &[&str] = &[
    "timezone_name",
    "duration_kind",
    "duration_seconds",
    "duration_min_seconds",
    "duration_max_seconds",
    "duration_source",
    "deadline_kind",
    "deadline_date",
    "deadline_at",
    "deadline_strength",
    "deadline_soft_weight",
    "earliest_start_at",
    "recurrence",
    "flexible_constraints",
    "has_own_effort",
    "split_policy",
];

const DURATION_COMPANION_FIELDS: &[&str] = &[
    "duration_kind",
    "duration_min_seconds",
    "duration_max_seconds",
    "duration_source",
];
const DEADLINE_COMPANION_FIELDS: &[&str] = &[
    "deadline_kind",
    "deadline_date",
    "deadline_strength",
    "deadline_soft_weight",
];
const BLOCKER_FIELDS: &[&str] = &[
    "blocked_reason_kind",
    "blocked_by_item_id",
    "blocked_reason",
];

pub(crate) enum OperationCompilation {
    Command(Box<ProposalCommand>),
    ManualReview(&'static str),
}

pub(crate) enum RequestCompilation {
    Actionable(ProposalKind),
    ManualReview(&'static str),
}

pub(crate) fn materialize_proposal(
    submitted_by: &str,
    spec: &ProposalSubmissionSpec,
    evidence: &SimulationProposalEvidence,
    now: DateTime<Utc>,
) -> Result<Proposal, SchedulingPortError> {
    if !evidence.is_valid() {
        return Err(SchedulingPortError::Unavailable(
            "simulation proposal evidence is invalid".to_owned(),
        ));
    }
    let (kind, payload) = match (evidence.proposal_kind(), evidence.change_set()) {
        (Some(kind), Some(change_set)) => (
            kind,
            serde_json::to_value(change_set).map_err(|_| {
                SchedulingPortError::Unavailable(
                    "simulation proposal evidence cannot be encoded".to_owned(),
                )
            })?,
        ),
        (None, None) => (
            ProposalKind::SchedulePlan,
            json!({
                "schema_version": 1,
                "base_revision": spec.request.base_revision,
                "assumptions": spec.request.assumptions,
                "operations": spec.request.operations,
                "source": {
                    "client": spec.source_client_label,
                    "conversation": spec.source_conversation_label,
                    "request_id": spec.source_request_id,
                },
                "safety": {
                    "proposal_only": true,
                    "requires_app_review": true,
                    "canonical_state_mutated": false,
                    "application_ready": false,
                    "manual_review_reasons": evidence.manual_review_reasons(),
                }
            }),
        ),
        _ => {
            return Err(SchedulingPortError::Unavailable(
                "simulation proposal evidence is inconsistent".to_owned(),
            ));
        }
    };
    Proposal::new(
        NewProposal {
            submitted_by: submitted_by.to_owned(),
            source: ProposalSource::ExternalMcp,
            source_reference: Some(spec.source_conversation_label.clone()),
            kind,
            title: spec.title.clone(),
            explanation: Some(spec.explanation.clone()),
            payload,
            expires_at: spec.expires_at,
        },
        now,
    )
    .map_err(|error| {
        SchedulingPortError::InvalidQuery(format!("proposal metadata is invalid: {error}"))
    })
}

pub(crate) fn classify_request(operations: &[PlanOperation]) -> RequestCompilation {
    let Some(first) = operations.first() else {
        return RequestCompilation::ManualReview("empty_plan");
    };
    if operations
        .iter()
        .any(|operation| operation.kind != first.kind)
    {
        return RequestCompilation::ManualReview("mixed_operation_kinds");
    }
    match first.kind {
        PlanOperationKind::CreateItem if operations.len() == 1 => {
            RequestCompilation::Actionable(ProposalKind::CreateItem)
        }
        PlanOperationKind::CreateItem => RequestCompilation::ManualReview("multiple_create_items"),
        PlanOperationKind::CreateEvent => {
            RequestCompilation::Actionable(ProposalKind::CalendarEvent)
        }
        PlanOperationKind::CompleteItem | PlanOperationKind::DeleteItem => {
            RequestCompilation::Actionable(ProposalKind::UpdateItem)
        }
        PlanOperationKind::UpdateConstraint => {
            RequestCompilation::Actionable(ProposalKind::ConstraintChange)
        }
        PlanOperationKind::UpdateItem => {
            RequestCompilation::ManualReview("unsupported_update_item")
        }
        PlanOperationKind::MoveBlock => RequestCompilation::ManualReview("unsupported_move_block"),
        PlanOperationKind::GoalBreakdown => {
            RequestCompilation::ManualReview("unsupported_goal_breakdown")
        }
        PlanOperationKind::ReplaceSchedule => {
            RequestCompilation::ManualReview("unsupported_replace_schedule")
        }
    }
}

pub(crate) fn compile_operation(
    operation: &PlanOperation,
    current: Option<&Item>,
    now: DateTime<Utc>,
) -> Result<OperationCompilation, SchedulingPortError> {
    match operation.kind {
        PlanOperationKind::CreateItem | PlanOperationKind::CreateEvent => {
            compile_create(operation, now)
        }
        PlanOperationKind::UpdateItem => {
            compile_replace(operation, current, now, UPDATE_ITEM_FIELDS)
        }
        PlanOperationKind::UpdateConstraint => {
            compile_replace(operation, current, now, UPDATE_CONSTRAINT_FIELDS)
        }
        PlanOperationKind::CompleteItem => compile_complete(operation, current, now),
        PlanOperationKind::DeleteItem => compile_delete(operation, current),
        PlanOperationKind::MoveBlock => {
            Ok(OperationCompilation::ManualReview("unsupported_move_block"))
        }
        PlanOperationKind::GoalBreakdown => Ok(OperationCompilation::ManualReview(
            "unsupported_goal_breakdown",
        )),
        PlanOperationKind::ReplaceSchedule => Ok(OperationCompilation::ManualReview(
            "unsupported_replace_schedule",
        )),
    }
}

pub(crate) fn finish_evidence(
    proposal_kind: ProposalKind,
    compilations: Vec<OperationCompilation>,
) -> Result<SimulationProposalEvidence, SchedulingPortError> {
    let mut commands = Vec::new();
    let mut reasons = BTreeSet::new();
    for compilation in compilations {
        match compilation {
            OperationCompilation::Command(command) => commands.push(*command),
            OperationCompilation::ManualReview(reason) => {
                reasons.insert(reason.to_owned());
            }
        }
    }
    if reasons.is_empty() {
        let change_set = ProposalChangeSet::new(commands).map_err(|error| {
            SchedulingPortError::InvalidQuery(format!(
                "operations cannot form one atomic proposal: {error}"
            ))
        })?;
        Ok(SimulationProposalEvidence::actionable(
            proposal_kind,
            change_set,
        ))
    } else {
        Ok(SimulationProposalEvidence::manual_review(
            reasons.into_iter().collect(),
        ))
    }
}

pub(crate) fn target_item_id(
    operation: &PlanOperation,
) -> Result<Option<Uuid>, SchedulingPortError> {
    match operation.kind {
        PlanOperationKind::CreateItem | PlanOperationKind::CreateEvent => {
            if operation.target_id.is_some() {
                return Err(invalid(&format!(
                    "{} must not include target_id",
                    operation_kind_name(operation.kind)
                )));
            }
            Ok(None)
        }
        PlanOperationKind::UpdateItem
        | PlanOperationKind::CompleteItem
        | PlanOperationKind::DeleteItem
        | PlanOperationKind::UpdateConstraint => {
            let value = operation.target_id.as_deref().ok_or_else(|| {
                invalid(&format!(
                    "{} requires target_id",
                    operation_kind_name(operation.kind)
                ))
            })?;
            Uuid::parse_str(value).map(Some).map_err(|_| {
                invalid(&format!(
                    "{} target_id must be a UUID",
                    operation_kind_name(operation.kind)
                ))
            })
        }
        PlanOperationKind::MoveBlock
        | PlanOperationKind::GoalBreakdown
        | PlanOperationKind::ReplaceSchedule => Ok(None),
    }
}

pub(crate) fn parent_item_id(
    operation: &PlanOperation,
) -> Result<Option<Uuid>, SchedulingPortError> {
    if !matches!(
        operation.kind,
        PlanOperationKind::CreateItem | PlanOperationKind::CreateEvent
    ) {
        return Ok(None);
    }
    match operation.parameters.get("parent_id") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Uuid::parse_str(value).map(Some).map_err(|_| {
            invalid(&format!(
                "{} parent_id must be a UUID or null",
                operation_kind_name(operation.kind)
            ))
        }),
        Some(_) => Err(invalid(&format!(
            "{} parent_id must be a UUID or null",
            operation_kind_name(operation.kind)
        ))),
    }
}

fn compile_create(
    operation: &PlanOperation,
    now: DateTime<Utc>,
) -> Result<OperationCompilation, SchedulingPortError> {
    let operation_name = operation_kind_name(operation.kind);
    if operation.target_id.is_some() {
        return Err(invalid(&format!(
            "{operation_name} must not include target_id"
        )));
    }
    let mut parameters = map_from_parameters(&operation.parameters);
    if parameters.contains_key("id") {
        return Err(invalid(&format!(
            "{operation_name} parameters must not provide a canonical item id"
        )));
    }
    parameters.insert("id".to_owned(), Value::String(Uuid::new_v4().to_string()));
    parameters
        .entry("is_sensitive".to_owned())
        .or_insert(Value::Bool(false));
    if operation.kind == PlanOperationKind::CreateEvent {
        if parameters
            .get("kind")
            .is_some_and(|kind| kind != &Value::String("event".to_owned()))
        {
            return Err(invalid("create_event kind must be event when provided"));
        }
        parameters.insert("kind".to_owned(), Value::String("event".to_owned()));
    }
    let item: NewItem = serde_json::from_value(Value::Object(parameters))
        .map_err(|error| invalid(&format!("{operation_name} parameters are invalid: {error}")))?;
    validate_item_time_precision(item.deadline_at, item.earliest_start_at, operation_name)?;
    if operation.kind == PlanOperationKind::CreateItem && item.kind == ItemKind::Event {
        return Err(invalid(
            "create_item cannot create an event; use create_event so calendar intent is explicit",
        ));
    }
    Item::new(item.clone(), now)
        .map_err(|error| invalid(&format!("{operation_name} parameters are invalid: {error}")))?;
    Ok(OperationCompilation::Command(Box::new(
        ProposalCommand::CreateItem {
            command_id: Uuid::new_v4(),
            item,
        },
    )))
}

fn compile_replace(
    operation: &PlanOperation,
    current: Option<&Item>,
    now: DateTime<Utc>,
    allowed_fields: &[&str],
) -> Result<OperationCompilation, SchedulingPortError> {
    let Some(current) = current else {
        return Ok(OperationCompilation::ManualReview("unknown_item"));
    };
    if operation.parameters.is_empty() {
        return Err(invalid(&format!(
            "{} requires at least one parameter",
            operation_kind_name(operation.kind)
        )));
    }
    let allowed = allowed_fields.iter().copied().collect::<BTreeSet<_>>();
    if let Some(field) = operation
        .parameters
        .keys()
        .find(|field| !allowed.contains(field.as_str()))
    {
        return Err(invalid(&format!(
            "{} cannot change field {field}",
            operation_kind_name(operation.kind)
        )));
    }

    let original = replacement_from_item(current);
    let mut replacement = serde_json::to_value(&original)
        .map_err(|_| SchedulingPortError::Unavailable("item cannot be encoded".to_owned()))?
        .as_object()
        .cloned()
        .ok_or_else(|| SchedulingPortError::Unavailable("item is invalid".to_owned()))?;
    for (field, value) in &operation.parameters {
        replacement.insert(field.clone(), value.clone());
    }
    normalize_legacy_partial_update(&mut replacement, &operation.parameters, current);
    let replacement: ReplaceItem =
        serde_json::from_value(Value::Object(replacement)).map_err(|error| {
            invalid(&format!(
                "{} parameters are invalid: {error}",
                operation_kind_name(operation.kind)
            ))
        })?;
    validate_item_time_precision(
        replacement.deadline_at,
        replacement.earliest_start_at,
        operation_kind_name(operation.kind),
    )?;
    if replacement == original {
        return Err(invalid(&format!(
            "{} must change at least one value",
            operation_kind_name(operation.kind)
        )));
    }
    current
        .replaced(replacement.clone(), now)
        .map_err(|error| {
            invalid(&format!(
                "{} parameters are invalid: {error}",
                operation_kind_name(operation.kind)
            ))
        })?;
    Ok(OperationCompilation::Command(Box::new(
        ProposalCommand::ReplaceItem {
            command_id: Uuid::new_v4(),
            item_id: current.id,
            expected_revision: current.revision,
            item: replacement,
        },
    )))
}

/// The compiler starts from a canonical replacement so unchanged rich
/// metadata survives partial updates. Legacy assistants know only the scalar
/// duration/deadline fields and the JSON own-effort projection, so repair only
/// companion fields that the operation itself omitted.
fn normalize_legacy_partial_update(
    replacement: &mut Map<String, Value>,
    parameters: &BTreeMap<String, Value>,
    current: &Item,
) {
    normalize_legacy_duration_update(replacement, parameters, current.duration_kind);
    normalize_legacy_deadline_update(replacement, parameters, current);
    normalize_legacy_own_effort_update(replacement, parameters);
    normalize_legacy_unblock(replacement, parameters, current.status);
}

fn normalize_legacy_duration_update(
    replacement: &mut Map<String, Value>,
    parameters: &BTreeMap<String, Value>,
    current_kind: DurationKind,
) {
    if parameters.contains_key("duration_seconds")
        && !DURATION_COMPANION_FIELDS
            .iter()
            .any(|field| parameters.contains_key(*field))
    {
        match replacement.get("duration_seconds").cloned() {
            Some(Value::Null) => set_unknown_duration(replacement),
            Some(Value::Number(value)) if value.as_u64().is_some() => {
                match current_kind {
                    DurationKind::Unknown => {
                        replacement.insert("duration_kind".to_owned(), json!("exact"));
                        replacement.insert(
                            "duration_min_seconds".to_owned(),
                            Value::Number(value.clone()),
                        );
                        replacement.insert(
                            "duration_max_seconds".to_owned(),
                            Value::Number(value.clone()),
                        );
                    }
                    DurationKind::Exact => {
                        replacement.insert(
                            "duration_min_seconds".to_owned(),
                            Value::Number(value.clone()),
                        );
                        replacement.insert(
                            "duration_max_seconds".to_owned(),
                            Value::Number(value.clone()),
                        );
                    }
                    DurationKind::Range => {}
                }
                // Every operation entering this compiler is an external-assistant
                // proposal, so changing its estimate must not retain older user,
                // learned, or import provenance.
                replacement.insert("duration_source".to_owned(), json!("assistant"));
            }
            _ => {}
        }
    }
}

fn normalize_legacy_deadline_update(
    replacement: &mut Map<String, Value>,
    parameters: &BTreeMap<String, Value>,
    current: &Item,
) {
    if DEADLINE_COMPANION_FIELDS
        .iter()
        .any(|field| parameters.contains_key(*field))
    {
        return;
    }
    let prospective_kind = replacement
        .get("kind")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or(current.kind);
    let kind_changed = parameters.contains_key("kind") && prospective_kind != current.kind;
    let changes_legacy_scalar = parameters.contains_key("deadline_at");
    if !kind_changed && !changes_legacy_scalar {
        return;
    }

    // Event deadline_at is its exact interval end, never task-deadline intent.
    // Conversely, a legacy Event -> non-Event kind change needs to make that
    // inherited scalar explicit before the new kind is validated.
    if prospective_kind == ItemKind::Event
        || replacement.get("deadline_at").is_some_and(Value::is_null)
    {
        set_no_deadline(replacement);
    } else if changes_legacy_scalar
        && current.deadline_kind == DeadlineKind::DateTime
        && current.kind != ItemKind::Event
    {
        // Preserve a rich hard/soft DateTime's strength when only its instant
        // changes. The assistant is not silently changing deadline policy.
    } else if current.kind == ItemKind::Event
        || (changes_legacy_scalar && current.deadline_kind != DeadlineKind::DateTime)
    {
        replacement.insert("deadline_kind".to_owned(), json!("date_time"));
        replacement.insert("deadline_date".to_owned(), Value::Null);
        replacement.insert("deadline_strength".to_owned(), json!("hard"));
        replacement.insert("deadline_soft_weight".to_owned(), Value::Null);
    }
}

fn normalize_legacy_own_effort_update(
    replacement: &mut Map<String, Value>,
    parameters: &BTreeMap<String, Value>,
) {
    let changes_constraints = parameters.contains_key("flexible_constraints");
    let changes_typed_own_effort = parameters.contains_key("has_own_effort");
    match (changes_constraints, changes_typed_own_effort) {
        (true, false) => {
            if let Some(object) = replacement
                .get("flexible_constraints")
                .and_then(Value::as_object)
            {
                match object.get("has_own_effort") {
                    Some(Value::Bool(value)) => {
                        replacement.insert("has_own_effort".to_owned(), Value::Bool(*value));
                    }
                    None => {
                        replacement.insert("has_own_effort".to_owned(), Value::Bool(false));
                    }
                    Some(_) => {}
                }
            }
        }
        (false, true) => {
            if let Some(object) = replacement
                .get_mut("flexible_constraints")
                .and_then(Value::as_object_mut)
            {
                object.remove("has_own_effort");
            }
        }
        (false, false) | (true, true) => {}
    }
}

fn normalize_legacy_unblock(
    replacement: &mut Map<String, Value>,
    parameters: &BTreeMap<String, Value>,
    current_status: ItemStatus,
) {
    if current_status == ItemStatus::Blocked
        && parameters
            .get("status")
            .is_some_and(|status| status != "blocked")
        && !BLOCKER_FIELDS
            .iter()
            .any(|field| parameters.contains_key(*field))
    {
        replacement.insert("blocked_reason_kind".to_owned(), Value::Null);
        replacement.insert("blocked_by_item_id".to_owned(), Value::Null);
        replacement.insert("blocked_reason".to_owned(), Value::Null);
    }
}

fn set_unknown_duration(replacement: &mut Map<String, Value>) {
    replacement.insert("duration_kind".to_owned(), json!("unknown"));
    replacement.insert("duration_min_seconds".to_owned(), Value::Null);
    replacement.insert("duration_max_seconds".to_owned(), Value::Null);
    replacement.insert("duration_source".to_owned(), Value::Null);
}

fn set_no_deadline(replacement: &mut Map<String, Value>) {
    replacement.insert("deadline_kind".to_owned(), json!("none"));
    replacement.insert("deadline_date".to_owned(), Value::Null);
    replacement.insert("deadline_strength".to_owned(), Value::Null);
    replacement.insert("deadline_soft_weight".to_owned(), Value::Null);
}

fn compile_complete(
    operation: &PlanOperation,
    current: Option<&Item>,
    now: DateTime<Utc>,
) -> Result<OperationCompilation, SchedulingPortError> {
    require_empty_parameters(operation)?;
    let Some(current) = current else {
        return Ok(OperationCompilation::ManualReview("unknown_item"));
    };
    if current.status == ItemStatus::Completed {
        return Err(invalid("complete_item target is already completed"));
    }
    if !current.is_executable {
        return Err(invalid(
            "complete_item target has active descendants and is not executable",
        ));
    }
    let mut replacement = replacement_from_item(current);
    replacement.status = ItemStatus::Completed;
    replacement.blocked_reason_kind = None;
    replacement.blocked_by_item_id = None;
    replacement.blocked_reason = None;
    current
        .replaced(replacement.clone(), now)
        .map_err(|error| invalid(&format!("complete_item target is invalid: {error}")))?;
    Ok(OperationCompilation::Command(Box::new(
        ProposalCommand::ReplaceItem {
            command_id: Uuid::new_v4(),
            item_id: current.id,
            expected_revision: current.revision,
            item: replacement,
        },
    )))
}

fn compile_delete(
    operation: &PlanOperation,
    current: Option<&Item>,
) -> Result<OperationCompilation, SchedulingPortError> {
    require_empty_parameters(operation)?;
    let Some(current) = current else {
        return Ok(OperationCompilation::ManualReview("unknown_item"));
    };
    Ok(OperationCompilation::Command(Box::new(
        ProposalCommand::TrashItem {
            command_id: Uuid::new_v4(),
            item_id: current.id,
            expected_revision: current.revision,
        },
    )))
}

fn require_empty_parameters(operation: &PlanOperation) -> Result<(), SchedulingPortError> {
    if operation.parameters.is_empty() {
        Ok(())
    } else {
        Err(invalid(&format!(
            "{} does not accept parameters",
            operation_kind_name(operation.kind)
        )))
    }
}

fn map_from_parameters(parameters: &BTreeMap<String, Value>) -> Map<String, Value> {
    parameters
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn replacement_from_item(item: &Item) -> ReplaceItem {
    ReplaceItem {
        is_sensitive: item.is_sensitive,
        kind: item.kind,
        status: item.status,
        title: item.title.clone(),
        notes: item.notes.clone(),
        timezone_name: item.timezone_name.clone(),
        duration_kind: Some(item.duration_kind),
        duration_seconds: item.duration_seconds,
        duration_min_seconds: item.duration_min_seconds,
        duration_max_seconds: item.duration_max_seconds,
        duration_source: item.duration_source,
        deadline_kind: Some(item.deadline_kind),
        deadline_date: item.deadline_date,
        deadline_at: item.deadline_at,
        deadline_strength: item.deadline_strength,
        deadline_soft_weight: item.deadline_soft_weight,
        earliest_start_at: item.earliest_start_at,
        recurrence: item.recurrence.clone(),
        flexible_constraints: item.flexible_constraints.clone(),
        has_own_effort: Some(item.has_own_effort),
        split_policy: item.split_policy.clone(),
        importance: item.importance,
        urgency: item.urgency,
        parent_id: item.parent_id,
        sibling_order: item.sibling_order,
        blocked_reason_kind: item.blocked_reason_kind,
        blocked_by_item_id: item.blocked_by_item_id,
        blocked_reason: item.blocked_reason.clone(),
    }
}

fn validate_item_time_precision(
    deadline_at: Option<DateTime<Utc>>,
    earliest_start_at: Option<DateTime<Utc>>,
    operation_name: &str,
) -> Result<(), SchedulingPortError> {
    if deadline_at
        .into_iter()
        .chain(earliest_start_at)
        .any(|value| !has_postgres_timestamp_precision(value))
    {
        return Err(invalid(&format!(
            "{operation_name} timestamps must use PostgreSQL microsecond precision"
        )));
    }
    Ok(())
}

const fn operation_kind_name(kind: PlanOperationKind) -> &'static str {
    match kind {
        PlanOperationKind::CreateItem => "create_item",
        PlanOperationKind::UpdateItem => "update_item",
        PlanOperationKind::MoveBlock => "move_block",
        PlanOperationKind::CompleteItem => "complete_item",
        PlanOperationKind::DeleteItem => "delete_item",
        PlanOperationKind::UpdateConstraint => "update_constraint",
        PlanOperationKind::CreateEvent => "create_event",
        PlanOperationKind::GoalBreakdown => "goal_breakdown",
        PlanOperationKind::ReplaceSchedule => "replace_schedule",
    }
}

fn invalid(message: &str) -> SchedulingPortError {
    SchedulingPortError::InvalidQuery(message.to_owned())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::TimeZone;
    use serde_json::json;

    use super::*;
    use crate::{
        proposals::{PROPOSAL_CHANGE_SET_SCHEMA_V1, ProposalSource},
        scheduling::{ProposalSubmissionSpec, SimulationRequest},
    };

    fn create_operation(kind: PlanOperationKind) -> PlanOperation {
        PlanOperation {
            kind,
            target_id: None,
            parameters: BTreeMap::from([
                ("kind".to_owned(), json!("task")),
                ("title".to_owned(), json!("Write launch notes")),
                ("timezone_name".to_owned(), json!("Europe/Madrid")),
                ("duration_seconds".to_owned(), json!(1800)),
            ]),
        }
    }

    fn current_item(now: DateTime<Utc>) -> Item {
        let OperationCompilation::Command(command) =
            compile_operation(&create_operation(PlanOperationKind::CreateItem), None, now).unwrap()
        else {
            panic!("create fixture must compile");
        };
        let ProposalCommand::CreateItem { item, .. } = *command else {
            panic!("create fixture must yield a create command");
        };
        Item::new(item, now).expect("current item fixture")
    }

    #[test]
    fn create_item_compiles_to_server_identified_typed_change_set() {
        let now = Utc.with_ymd_and_hms(2026, 8, 30, 10, 0, 0).unwrap();
        let operation = create_operation(PlanOperationKind::CreateItem);
        let RequestCompilation::Actionable(kind) =
            classify_request(std::slice::from_ref(&operation))
        else {
            panic!("one create_item must be actionable");
        };
        let evidence = finish_evidence(
            kind,
            vec![compile_operation(&operation, None, now).unwrap()],
        )
        .unwrap();
        assert!(evidence.is_valid());
        assert_eq!(evidence.proposal_kind(), Some(ProposalKind::CreateItem));
        let change_set = evidence.change_set().unwrap();
        assert_eq!(change_set.commands.len(), 1);
        let ProposalCommand::CreateItem { command_id, item } = &change_set.commands[0] else {
            panic!("compiler must emit create_item");
        };
        assert!(!command_id.is_nil());
        assert!(!item.id.is_nil());
        assert_ne!(*command_id, item.id);
        assert_eq!(item.title, "Write launch notes");
    }

    #[test]
    fn legacy_assistant_scalar_edits_preserve_rich_shapes_and_update_provenance() {
        let now = Utc.with_ymd_and_hms(2026, 8, 30, 10, 0, 0).unwrap();
        let mut current = current_item(now);
        current.duration_kind = DurationKind::Range;
        current.duration_seconds = Some(1_800);
        current.duration_min_seconds = Some(1_200);
        current.duration_max_seconds = Some(3_600);
        current.duration_source = Some(crate::items::DurationSource::Imported);
        current.deadline_kind = DeadlineKind::DateTime;
        current.deadline_at = Some(Utc.with_ymd_and_hms(2026, 9, 3, 12, 0, 0).unwrap());
        current.deadline_strength = Some(crate::items::DeadlineStrength::Soft);
        current.deadline_soft_weight = Some(91);
        let operation = PlanOperation {
            kind: PlanOperationKind::UpdateConstraint,
            target_id: Some(current.id.to_string()),
            parameters: BTreeMap::from([
                ("duration_seconds".to_owned(), json!(2_400)),
                ("deadline_at".to_owned(), json!("2026-09-04T12:00:00Z")),
            ]),
        };
        let OperationCompilation::Command(command) =
            compile_operation(&operation, Some(&current), now).expect("legacy partial update")
        else {
            panic!("known item update is actionable");
        };
        let ProposalCommand::ReplaceItem { item, .. } = *command else {
            panic!("update must yield replacement");
        };
        assert_eq!(item.duration_kind, Some(DurationKind::Range));
        assert_eq!(item.duration_min_seconds, Some(1_200));
        assert_eq!(item.duration_seconds, Some(2_400));
        assert_eq!(item.duration_max_seconds, Some(3_600));
        assert_eq!(
            item.duration_source,
            Some(crate::items::DurationSource::Assistant)
        );
        assert_eq!(item.deadline_kind, Some(DeadlineKind::DateTime));
        assert_eq!(
            item.deadline_strength,
            Some(crate::items::DeadlineStrength::Soft)
        );
        assert_eq!(item.deadline_soft_weight, Some(91));
    }

    #[test]
    fn legacy_assistant_exact_duration_edit_updates_exact_bounds() {
        let now = Utc.with_ymd_and_hms(2026, 8, 30, 10, 0, 0).unwrap();
        let current = current_item(now);
        let operation = PlanOperation {
            kind: PlanOperationKind::UpdateConstraint,
            target_id: Some(current.id.to_string()),
            parameters: BTreeMap::from([("duration_seconds".to_owned(), json!(2_700))]),
        };
        let OperationCompilation::Command(command) =
            compile_operation(&operation, Some(&current), now).expect("exact duration update")
        else {
            panic!("known item update is actionable");
        };
        let ProposalCommand::ReplaceItem { item, .. } = *command else {
            panic!("update must yield replacement");
        };
        assert_eq!(item.duration_kind, Some(DurationKind::Exact));
        assert_eq!(item.duration_min_seconds, Some(2_700));
        assert_eq!(item.duration_max_seconds, Some(2_700));
        assert_eq!(
            item.duration_source,
            Some(crate::items::DurationSource::Assistant)
        );
    }

    #[test]
    fn legacy_kind_changes_normalize_event_interval_and_task_deadline_semantics() {
        let now = Utc.with_ymd_and_hms(2026, 8, 30, 10, 0, 0).unwrap();
        let interval_end = Utc.with_ymd_and_hms(2026, 9, 3, 12, 0, 0).unwrap();
        let mut event = current_item(now);
        event.kind = ItemKind::Event;
        event.deadline_kind = DeadlineKind::None;
        event.deadline_at = Some(interval_end);
        event.deadline_strength = None;
        event.deadline_soft_weight = None;
        let event_to_task = PlanOperation {
            kind: PlanOperationKind::UpdateItem,
            target_id: Some(event.id.to_string()),
            parameters: BTreeMap::from([("kind".to_owned(), json!("task"))]),
        };
        let OperationCompilation::Command(command) =
            compile_operation(&event_to_task, Some(&event), now).expect("Event to Task")
        else {
            panic!("kind update must yield replacement");
        };
        let ProposalCommand::ReplaceItem { item, .. } = *command else {
            panic!("kind update must replace item");
        };
        assert_eq!(item.kind, ItemKind::Task);
        assert_eq!(item.deadline_kind, Some(DeadlineKind::DateTime));
        assert_eq!(item.deadline_at, Some(interval_end));
        assert_eq!(
            item.deadline_strength,
            Some(crate::items::DeadlineStrength::Hard)
        );

        let mut task = current_item(now);
        task.deadline_kind = DeadlineKind::DateTime;
        task.deadline_at = Some(interval_end);
        task.deadline_strength = Some(crate::items::DeadlineStrength::Hard);
        let task_to_event = PlanOperation {
            kind: PlanOperationKind::UpdateItem,
            target_id: Some(task.id.to_string()),
            parameters: BTreeMap::from([("kind".to_owned(), json!("event"))]),
        };
        let OperationCompilation::Command(command) =
            compile_operation(&task_to_event, Some(&task), now).expect("Task to Event")
        else {
            panic!("kind update must yield replacement");
        };
        let ProposalCommand::ReplaceItem { item, .. } = *command else {
            panic!("kind update must replace item");
        };
        assert_eq!(item.kind, ItemKind::Event);
        assert_eq!(item.deadline_kind, Some(DeadlineKind::None));
        assert_eq!(item.deadline_at, Some(interval_end));
        assert!(item.deadline_strength.is_none());
        assert!(item.deadline_soft_weight.is_none());
    }

    #[test]
    fn completing_a_blocked_leaf_clears_its_obsolete_blocking_cause() {
        let now = Utc.with_ymd_and_hms(2026, 8, 30, 10, 0, 0).unwrap();
        let mut current = current_item(now);
        current.status = ItemStatus::Blocked;
        current.blocked_reason_kind = Some(crate::items::BlockedReasonKind::Manual);
        current.blocked_reason = Some("Waiting for review".to_owned());
        let operation = PlanOperation {
            kind: PlanOperationKind::CompleteItem,
            target_id: Some(current.id.to_string()),
            parameters: BTreeMap::new(),
        };
        let OperationCompilation::Command(command) =
            compile_operation(&operation, Some(&current), now).expect("complete blocked leaf")
        else {
            panic!("completion must yield replacement");
        };
        let ProposalCommand::ReplaceItem { item, .. } = *command else {
            panic!("completion must replace the item");
        };
        assert_eq!(item.status, ItemStatus::Completed);
        assert!(item.blocked_reason_kind.is_none());
        assert!(item.blocked_by_item_id.is_none());
        assert!(item.blocked_reason.is_none());
    }

    #[test]
    fn mixed_request_is_wholly_manual_review() {
        let operations = vec![
            create_operation(PlanOperationKind::CreateItem),
            PlanOperation {
                kind: PlanOperationKind::MoveBlock,
                target_id: Some(Uuid::new_v4().to_string()),
                parameters: BTreeMap::new(),
            },
        ];
        assert!(matches!(
            classify_request(&operations),
            RequestCompilation::ManualReview("mixed_operation_kinds")
        ));
    }

    #[test]
    fn materialization_uses_only_hidden_typed_evidence_for_executable_payload() {
        let now = Utc.with_ymd_and_hms(2026, 8, 30, 10, 0, 0).unwrap();
        let operation = create_operation(PlanOperationKind::CreateItem);
        let evidence = finish_evidence(
            ProposalKind::CreateItem,
            vec![compile_operation(&operation, None, now).unwrap()],
        )
        .unwrap();
        let spec = ProposalSubmissionSpec {
            idempotency_key: "test-create-001".to_owned(),
            request_fingerprint: [7; 32],
            simulation_token: "sim_test".to_owned(),
            request: SimulationRequest {
                base_revision: "schedule-7".to_owned(),
                operations: vec![operation],
                assumptions: vec!["Keep lunch free".to_owned()],
            },
            title: "Create launch task".to_owned(),
            explanation: "Requested in chat".to_owned(),
            source_conversation_label: "Launch planning".to_owned(),
            source_client_label: Some("ChatGPT".to_owned()),
            source_request_id: "request-7".to_owned(),
            expires_at: now + chrono::Duration::hours(1),
        };
        let proposal = materialize_proposal("owner", &spec, &evidence, now).unwrap();
        assert_eq!(proposal.source, ProposalSource::ExternalMcp);
        assert_eq!(proposal.kind, ProposalKind::CreateItem);
        assert_eq!(
            proposal.payload.get("schema").and_then(Value::as_str),
            Some(PROPOSAL_CHANGE_SET_SCHEMA_V1)
        );
        assert!(proposal.payload.get("source").is_none());
    }

    #[test]
    fn manual_materialization_cannot_parse_as_typed_payload() {
        let now = Utc.with_ymd_and_hms(2026, 8, 30, 10, 0, 0).unwrap();
        let operation = PlanOperation {
            kind: PlanOperationKind::MoveBlock,
            target_id: Some(Uuid::new_v4().to_string()),
            parameters: BTreeMap::new(),
        };
        let spec = ProposalSubmissionSpec {
            idempotency_key: "test-manual-001".to_owned(),
            request_fingerprint: [8; 32],
            simulation_token: "sim_test".to_owned(),
            request: SimulationRequest {
                base_revision: "schedule-7".to_owned(),
                operations: vec![operation],
                assumptions: Vec::new(),
            },
            title: "Move focus block".to_owned(),
            explanation: "Requested in chat".to_owned(),
            source_conversation_label: "Daily plan".to_owned(),
            source_client_label: Some("ChatGPT".to_owned()),
            source_request_id: "request-8".to_owned(),
            expires_at: now + chrono::Duration::hours(1),
        };
        let evidence =
            SimulationProposalEvidence::manual_review(vec!["unsupported_move_block".to_owned()]);
        let proposal = materialize_proposal("owner", &spec, &evidence, now).unwrap();
        assert_eq!(proposal.kind, ProposalKind::SchedulePlan);
        assert!(ProposalChangeSet::from_payload(&proposal.payload).is_err());
        assert_eq!(
            proposal.payload.pointer("/safety/application_ready"),
            Some(&Value::Bool(false))
        );
    }
}
