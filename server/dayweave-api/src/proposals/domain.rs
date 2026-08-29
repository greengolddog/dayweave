use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

const MAX_TITLE_LENGTH: usize = 200;
const MAX_EXPLANATION_LENGTH: usize = 4_000;
const MAX_SOURCE_REFERENCE_LENGTH: usize = 500;
const MAX_DECISION_NOTE_LENGTH: usize = 1_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalSource {
    AppAssistant,
    ChatGpt,
    Codex,
    ExternalMcp,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalKind {
    CreateItem,
    UpdateItem,
    GoalBreakdown,
    ConstraintChange,
    CalendarEvent,
    SchedulePlan,
    Recommendation,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Pending,
    Accepted,
    Rejected,
    Expired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecisionKind {
    Accept,
    Reject,
}

#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
pub struct Proposal {
    pub id: Uuid,
    pub revision: u64,
    pub submitted_by: String,
    pub source: ProposalSource,
    pub source_reference: Option<String>,
    pub kind: ProposalKind,
    pub status: ProposalStatus,
    pub title: String,
    pub explanation: Option<String>,
    /// Type-specific proposal content. It must be a JSON object so the API can
    /// add versioned schemas for each proposal kind without changing storage.
    #[schema(value_type = Object)]
    pub payload: Value,
    pub decision_note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct NewProposal {
    pub submitted_by: String,
    pub source: ProposalSource,
    pub source_reference: Option<String>,
    pub kind: ProposalKind,
    pub title: String,
    pub explanation: Option<String>,
    pub payload: Value,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Default)]
pub struct EditProposal {
    pub title: Option<String>,
    pub explanation: Option<String>,
    pub payload: Option<Value>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl Proposal {
    /// Creates a validated pending proposal.
    ///
    /// # Errors
    ///
    /// Returns [`ProposalDomainError`] when a field is invalid, the payload is
    /// not an object, or the expiration is not in the future.
    pub fn new(input: NewProposal, now: DateTime<Utc>) -> Result<Self, ProposalDomainError> {
        validate_nonempty(
            "submitted_by",
            &input.submitted_by,
            MAX_SOURCE_REFERENCE_LENGTH,
        )?;
        validate_nonempty("title", &input.title, MAX_TITLE_LENGTH)?;
        validate_optional_length(
            "source_reference",
            input.source_reference.as_deref(),
            MAX_SOURCE_REFERENCE_LENGTH,
        )?;
        validate_optional_length(
            "explanation",
            input.explanation.as_deref(),
            MAX_EXPLANATION_LENGTH,
        )?;
        validate_payload(&input.payload)?;
        if input.expires_at <= now {
            return Err(ProposalDomainError::ExpirationMustBeFuture);
        }

        Ok(Self {
            id: Uuid::new_v4(),
            revision: 1,
            submitted_by: input.submitted_by,
            source: input.source,
            source_reference: input.source_reference,
            kind: input.kind,
            status: ProposalStatus::Pending,
            title: input.title,
            explanation: input.explanation,
            payload: input.payload,
            decision_note: None,
            created_at: now,
            updated_at: now,
            expires_at: input.expires_at,
            decided_at: None,
        })
    }

    /// Applies changes to a pending proposal and advances its revision.
    ///
    /// # Errors
    ///
    /// Returns [`ProposalDomainError`] when the proposal is no longer pending,
    /// no changes were supplied, or a changed field is invalid.
    pub fn edit(
        &mut self,
        edit: EditProposal,
        now: DateTime<Utc>,
    ) -> Result<(), ProposalDomainError> {
        self.ensure_pending(now)?;
        if edit.title.is_none()
            && edit.explanation.is_none()
            && edit.payload.is_none()
            && edit.expires_at.is_none()
        {
            return Err(ProposalDomainError::EmptyEdit);
        }

        if let Some(title) = edit.title.as_deref() {
            validate_nonempty("title", title, MAX_TITLE_LENGTH)?;
        }
        if let Some(explanation) = edit.explanation.as_deref() {
            validate_nonempty("explanation", explanation, MAX_EXPLANATION_LENGTH)?;
        }
        if let Some(payload) = edit.payload.as_ref() {
            validate_payload(payload)?;
        }
        if let Some(expires_at) = edit.expires_at
            && expires_at <= now
        {
            return Err(ProposalDomainError::ExpirationMustBeFuture);
        }

        if let Some(title) = edit.title {
            self.title = title;
        }
        if let Some(explanation) = edit.explanation {
            self.explanation = Some(explanation);
        }
        if let Some(payload) = edit.payload {
            self.payload = payload;
        }
        if let Some(expires_at) = edit.expires_at {
            self.expires_at = expires_at;
        }
        self.bump_revision(now);
        Ok(())
    }

    /// Accepts or rejects a pending proposal.
    ///
    /// # Errors
    ///
    /// Returns [`ProposalDomainError`] when the proposal is no longer pending
    /// or the optional decision note is too long.
    pub fn decide(
        &mut self,
        decision: DecisionKind,
        note: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<(), ProposalDomainError> {
        self.ensure_pending(now)?;
        validate_optional_length("decision_note", note.as_deref(), MAX_DECISION_NOTE_LENGTH)?;

        self.status = match decision {
            DecisionKind::Accept => ProposalStatus::Accepted,
            DecisionKind::Reject => ProposalStatus::Rejected,
        };
        self.decision_note = note;
        self.decided_at = Some(now);
        self.bump_revision(now);
        Ok(())
    }

    /// Moves a due pending proposal into its durable terminal state.
    ///
    /// Returns whether a transition occurred.
    pub fn expire_if_due(&mut self, now: DateTime<Utc>) -> bool {
        if self.status == ProposalStatus::Pending && self.expires_at <= now {
            self.status = ProposalStatus::Expired;
            self.bump_revision(now);
            return true;
        }
        false
    }

    fn ensure_pending(&mut self, now: DateTime<Utc>) -> Result<(), ProposalDomainError> {
        self.expire_if_due(now);
        if self.status != ProposalStatus::Pending {
            return Err(ProposalDomainError::NotPending(self.status));
        }
        Ok(())
    }

    fn bump_revision(&mut self, now: DateTime<Utc>) {
        self.revision += 1;
        self.updated_at = now;
    }
}

fn validate_payload(payload: &Value) -> Result<(), ProposalDomainError> {
    if !payload.is_object() {
        return Err(ProposalDomainError::PayloadMustBeObject);
    }
    if json_has_unsupported_text(payload, 0) {
        return Err(ProposalDomainError::UnsupportedText("payload"));
    }
    Ok(())
}

fn json_has_unsupported_text(value: &Value, depth: usize) -> bool {
    if depth > 64 {
        return true;
    }
    match value {
        Value::String(value) => value.chars().any(char::is_control),
        Value::Array(values) => values
            .iter()
            .any(|value| json_has_unsupported_text(value, depth + 1)),
        Value::Object(values) => values.iter().any(|(key, value)| {
            key.chars().any(char::is_control) || json_has_unsupported_text(value, depth + 1)
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn validate_nonempty(
    field: &'static str,
    value: &str,
    max_length: usize,
) -> Result<(), ProposalDomainError> {
    if value.trim().is_empty() {
        return Err(ProposalDomainError::Required(field));
    }
    validate_length(field, value, max_length)
}

fn validate_optional_length(
    field: &'static str,
    value: Option<&str>,
    max_length: usize,
) -> Result<(), ProposalDomainError> {
    value.map_or(Ok(()), |value| validate_length(field, value, max_length))
}

fn validate_length(
    field: &'static str,
    value: &str,
    max_length: usize,
) -> Result<(), ProposalDomainError> {
    if value.chars().count() > max_length {
        return Err(ProposalDomainError::TooLong { field, max_length });
    }
    if value.chars().any(char::is_control) {
        return Err(ProposalDomainError::UnsupportedText(field));
    }
    Ok(())
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ProposalDomainError {
    #[error("{0} is required")]
    Required(&'static str),
    #[error("{field} must contain no more than {max_length} characters")]
    TooLong {
        field: &'static str,
        max_length: usize,
    },
    #[error("{0} contains unsupported control characters")]
    UnsupportedText(&'static str),
    #[error("payload must be a JSON object")]
    PayloadMustBeObject,
    #[error("expires_at must be in the future")]
    ExpirationMustBeFuture,
    #[error("at least one editable field is required")]
    EmptyEdit,
    #[error("proposal is not pending (current status: {0:?})")]
    NotPending(ProposalStatus),
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use serde_json::json;

    use super::*;

    fn proposal(now: DateTime<Utc>) -> Proposal {
        Proposal::new(
            NewProposal {
                submitted_by: "test-user".to_owned(),
                source: ProposalSource::Codex,
                source_reference: Some("conversation-1".to_owned()),
                kind: ProposalKind::CreateItem,
                title: "Prepare review".to_owned(),
                explanation: Some("A focused review block".to_owned()),
                payload: json!({"duration_minutes": 30}),
                expires_at: now + Duration::days(7),
            },
            now,
        )
        .expect("valid proposal")
    }

    #[test]
    fn validates_required_fields_and_object_payload() {
        let now = Utc::now();
        let mut input = NewProposal {
            submitted_by: "test-user".to_owned(),
            source: ProposalSource::Codex,
            source_reference: None,
            kind: ProposalKind::Recommendation,
            title: " ".to_owned(),
            explanation: None,
            payload: json!([]),
            expires_at: now + Duration::hours(1),
        };

        assert_eq!(
            Proposal::new(input.clone(), now).unwrap_err(),
            ProposalDomainError::Required("title")
        );
        input.title = "A title".to_owned();
        assert_eq!(
            Proposal::new(input, now).unwrap_err(),
            ProposalDomainError::PayloadMustBeObject
        );
    }

    #[test]
    fn decision_is_terminal_and_revisioned() {
        let now = Utc::now();
        let mut proposal = proposal(now);

        proposal
            .decide(
                DecisionKind::Accept,
                Some("Looks right".to_owned()),
                now + Duration::minutes(1),
            )
            .expect("accept pending proposal");

        assert_eq!(proposal.status, ProposalStatus::Accepted);
        assert_eq!(proposal.revision, 2);
        assert_eq!(
            proposal
                .decide(DecisionKind::Reject, None, now + Duration::minutes(2))
                .unwrap_err(),
            ProposalDomainError::NotPending(ProposalStatus::Accepted)
        );
    }

    #[test]
    fn due_proposal_expires_before_edit() {
        let now = Utc::now();
        let mut proposal = proposal(now);

        let error = proposal
            .edit(
                EditProposal {
                    title: Some("Too late".to_owned()),
                    ..EditProposal::default()
                },
                now + Duration::days(8),
            )
            .unwrap_err();

        assert_eq!(
            error,
            ProposalDomainError::NotPending(ProposalStatus::Expired)
        );
        assert_eq!(proposal.status, ProposalStatus::Expired);
        assert_eq!(proposal.revision, 2);
    }

    #[test]
    fn invalid_edit_does_not_partially_mutate_proposal() {
        let now = Utc::now();
        let mut proposal = proposal(now);
        let original_title = proposal.title.clone();

        assert_eq!(
            proposal
                .edit(
                    EditProposal {
                        title: Some("A title that must not leak through".to_owned()),
                        expires_at: Some(now),
                        ..EditProposal::default()
                    },
                    now + Duration::minutes(1),
                )
                .unwrap_err(),
            ProposalDomainError::ExpirationMustBeFuture
        );
        assert_eq!(proposal.title, original_title);
        assert_eq!(proposal.revision, 1);
    }
}
