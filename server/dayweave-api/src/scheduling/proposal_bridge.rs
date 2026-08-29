use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::{
    items::{Item, ItemKind, ItemStatus, NewItem, ReplaceItem},
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
    "duration_seconds",
    "deadline_at",
    "earliest_start_at",
    "recurrence",
    "flexible_constraints",
    "split_policy",
    "importance",
    "urgency",
    "parent_id",
    "sibling_order",
];

const UPDATE_CONSTRAINT_FIELDS: &[&str] = &[
    "timezone_name",
    "duration_seconds",
    "deadline_at",
    "earliest_start_at",
    "recurrence",
    "flexible_constraints",
    "split_policy",
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
        duration_seconds: item.duration_seconds,
        deadline_at: item.deadline_at,
        earliest_start_at: item.earliest_start_at,
        recurrence: item.recurrence.clone(),
        flexible_constraints: item.flexible_constraints.clone(),
        split_policy: item.split_policy.clone(),
        importance: item.importance,
        urgency: item.urgency,
        parent_id: item.parent_id,
        sibling_order: item.sibling_order,
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
