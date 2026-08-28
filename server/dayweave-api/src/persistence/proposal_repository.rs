use async_trait::async_trait;
use serde_json::json;
use sqlx::{PgPool, Postgres, QueryBuilder, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::proposals::{
    Proposal, ProposalKind, ProposalQuery, ProposalRepository, ProposalSource, ProposalStatus,
    RepositoryError,
};

use super::DatabaseScope;

#[derive(Clone, Debug)]
pub struct PostgresProposalRepository {
    pool: PgPool,
    scope: DatabaseScope,
}

impl PostgresProposalRepository {
    #[must_use]
    pub fn new(pool: PgPool, scope: DatabaseScope) -> Self {
        Self { pool, scope }
    }
}

#[async_trait]
impl ProposalRepository for PostgresProposalRepository {
    async fn insert(&self, proposal: Proposal) -> Result<Proposal, RepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        let revision = revision_to_i64(proposal.revision)?;
        let result = sqlx::query(
            "INSERT INTO proposals (id, workspace_id, revision, submitted_by_user_id, \
             submitted_by_subject, source, source_reference, kind, status, title, explanation, \
             payload, decision_note, created_at, updated_at, expires_at, decided_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
        )
        .bind(proposal.id)
        .bind(self.scope.workspace_id)
        .bind(revision)
        .bind(self.scope.user_id)
        .bind(&proposal.submitted_by)
        .bind(source_name(proposal.source))
        .bind(&proposal.source_reference)
        .bind(kind_name(proposal.kind))
        .bind(status_name(proposal.status))
        .bind(&proposal.title)
        .bind(&proposal.explanation)
        .bind(&proposal.payload)
        .bind(&proposal.decision_note)
        .bind(proposal.created_at)
        .bind(proposal.updated_at)
        .bind(proposal.expires_at)
        .bind(proposal.decided_at)
        .execute(&mut *transaction)
        .await;
        if let Err(error) = result {
            return Err(map_insert_error(&error, proposal.id));
        }
        record_mutation(
            &mut transaction,
            self.scope,
            &proposal,
            "proposal.created",
            None,
        )
        .await?;
        transaction.commit().await.map_err(internal)?;
        Ok(proposal)
    }

    async fn get(&self, id: Uuid) -> Result<Proposal, RepositoryError> {
        let row = sqlx::query(
            "SELECT id, revision, submitted_by_subject, source, source_reference, kind, \
             status, title, explanation, payload, decision_note, created_at, updated_at, \
             expires_at, decided_at FROM proposals \
             WHERE workspace_id = $1 AND id = $2 AND trashed_at IS NULL",
        )
        .bind(self.scope.workspace_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?
        .ok_or(RepositoryError::NotFound(id))?;
        proposal_from_row(&row)
    }

    async fn list(&self, query: ProposalQuery) -> Result<Vec<Proposal>, RepositoryError> {
        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT id, revision, submitted_by_subject, source, source_reference, kind, \
             status, title, explanation, payload, decision_note, created_at, updated_at, \
             expires_at, decided_at FROM proposals WHERE workspace_id = ",
        );
        builder.push_bind(self.scope.workspace_id);
        builder.push(" AND trashed_at IS NULL");
        if let Some(status) = query.status {
            builder
                .push(" AND status = ")
                .push_bind(status_name(status));
        }
        if let Some(source) = query.source {
            builder
                .push(" AND source = ")
                .push_bind(source_name(source));
        }
        let limit = i64::try_from(query.limit).unwrap_or(i64::MAX);
        builder
            .push(" ORDER BY created_at DESC, id DESC LIMIT ")
            .push_bind(limit);
        builder
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(internal)?
            .iter()
            .map(proposal_from_row)
            .collect()
    }

    async fn replace(
        &self,
        proposal: Proposal,
        expected_revision: u64,
    ) -> Result<Proposal, RepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_expected_revision(
            &mut transaction,
            self.scope.workspace_id,
            proposal.id,
            expected_revision,
        )
        .await?;
        let revision = revision_to_i64(proposal.revision)?;
        sqlx::query(
            "UPDATE proposals SET revision = $3, submitted_by_subject = $4, source = $5, \
             source_reference = $6, kind = $7, status = $8, title = $9, explanation = $10, \
             payload = $11, decision_note = $12, updated_at = $13, expires_at = $14, decided_at = $15 \
             WHERE workspace_id = $1 AND id = $2 AND trashed_at IS NULL",
        )
        .bind(self.scope.workspace_id)
        .bind(proposal.id)
        .bind(revision)
        .bind(&proposal.submitted_by)
        .bind(source_name(proposal.source))
        .bind(&proposal.source_reference)
        .bind(kind_name(proposal.kind))
        .bind(status_name(proposal.status))
        .bind(&proposal.title)
        .bind(&proposal.explanation)
        .bind(&proposal.payload)
        .bind(&proposal.decision_note)
        .bind(proposal.updated_at)
        .bind(proposal.expires_at)
        .bind(proposal.decided_at)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        record_mutation(
            &mut transaction,
            self.scope,
            &proposal,
            "proposal.updated",
            Some(expected_revision),
        )
        .await?;
        transaction.commit().await.map_err(internal)?;
        Ok(proposal)
    }

    async fn delete(&self, id: Uuid, expected_revision: u64) -> Result<(), RepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(internal)?;
        lock_expected_revision(
            &mut transaction,
            self.scope.workspace_id,
            id,
            expected_revision,
        )
        .await?;
        sqlx::query(
            "UPDATE proposals SET trashed_at = clock_timestamp(), updated_at = clock_timestamp() \
             WHERE workspace_id = $1 AND id = $2 AND trashed_at IS NULL",
        )
        .bind(self.scope.workspace_id)
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(internal)?;
        record_deletion(&mut transaction, self.scope, id, expected_revision).await?;
        transaction.commit().await.map_err(internal)
    }
}

async fn lock_expected_revision(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    proposal_id: Uuid,
    expected_revision: u64,
) -> Result<(), RepositoryError> {
    let actual: Option<i64> = sqlx::query_scalar(
        "SELECT revision FROM proposals \
         WHERE workspace_id = $1 AND id = $2 AND trashed_at IS NULL FOR UPDATE",
    )
    .bind(workspace_id)
    .bind(proposal_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(internal)?;
    let Some(actual) = actual else {
        return Err(RepositoryError::NotFound(proposal_id));
    };
    let actual = u64::try_from(actual).map_err(|_| RepositoryError::Internal)?;
    if actual != expected_revision {
        return Err(RepositoryError::RevisionConflict {
            expected: expected_revision,
            actual,
        });
    }
    Ok(())
}

async fn record_mutation(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    proposal: &Proposal,
    event_type: &str,
    base_revision: Option<u64>,
) -> Result<(), RepositoryError> {
    let revision = revision_to_i64(proposal.revision)?;
    let deduplication_key = format!("{event_type}:{}:{}", proposal.id, proposal.revision);
    sqlx::query(
        "INSERT INTO outbox_messages (id, workspace_id, aggregate_type, aggregate_id, \
         aggregate_revision, event_type, deduplication_key, payload) \
         VALUES ($1, $2, 'proposal', $3, $4, $5, $6, $7)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(proposal.id)
    .bind(revision)
    .bind(event_type)
    .bind(deduplication_key)
    .bind(json!({
        "proposal_id": proposal.id,
        "revision": proposal.revision,
        "status": status_name(proposal.status),
    }))
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    record_audit(
        transaction,
        scope,
        proposal.id,
        event_type,
        base_revision,
        Some(proposal.revision),
    )
    .await
}

async fn record_deletion(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    proposal_id: Uuid,
    revision: u64,
) -> Result<(), RepositoryError> {
    let revision_i64 = revision_to_i64(revision)?;
    sqlx::query(
        "INSERT INTO outbox_messages (id, workspace_id, aggregate_type, aggregate_id, \
         aggregate_revision, event_type, deduplication_key, payload) \
         VALUES ($1, $2, 'proposal', $3, $4, 'proposal.trashed', $5, $6)",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(proposal_id)
    .bind(revision_i64)
    .bind(format!("proposal.trashed:{proposal_id}:{revision}"))
    .bind(json!({ "proposal_id": proposal_id, "revision": revision }))
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    record_audit(
        transaction,
        scope,
        proposal_id,
        "proposal.trashed",
        Some(revision),
        Some(revision),
    )
    .await
}

async fn record_audit(
    transaction: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    proposal_id: Uuid,
    operation_type: &str,
    base_revision: Option<u64>,
    result_revision: Option<u64>,
) -> Result<(), RepositoryError> {
    sqlx::query(
        "INSERT INTO audit_operations (id, workspace_id, actor_user_id, operation_type, \
         entity_type, entity_id, base_revision, result_revision, outcome) \
         VALUES ($1, $2, $3, $4, 'proposal', $5, $6, $7, 'succeeded')",
    )
    .bind(Uuid::new_v4())
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .bind(operation_type)
    .bind(proposal_id)
    .bind(base_revision.map(revision_to_i64).transpose()?)
    .bind(result_revision.map(revision_to_i64).transpose()?)
    .execute(&mut **transaction)
    .await
    .map_err(internal)?;
    Ok(())
}

fn proposal_from_row(row: &PgRow) -> Result<Proposal, RepositoryError> {
    let revision: i64 = row.try_get("revision").map_err(internal)?;
    Ok(Proposal {
        id: row.try_get("id").map_err(internal)?,
        revision: u64::try_from(revision).map_err(|_| RepositoryError::Internal)?,
        submitted_by: row.try_get("submitted_by_subject").map_err(internal)?,
        source: parse_source(&row.try_get::<String, _>("source").map_err(internal)?)?,
        source_reference: row.try_get("source_reference").map_err(internal)?,
        kind: parse_kind(&row.try_get::<String, _>("kind").map_err(internal)?)?,
        status: parse_status(&row.try_get::<String, _>("status").map_err(internal)?)?,
        title: row.try_get("title").map_err(internal)?,
        explanation: row.try_get("explanation").map_err(internal)?,
        payload: row.try_get("payload").map_err(internal)?,
        decision_note: row.try_get("decision_note").map_err(internal)?,
        created_at: row.try_get("created_at").map_err(internal)?,
        updated_at: row.try_get("updated_at").map_err(internal)?,
        expires_at: row.try_get("expires_at").map_err(internal)?,
        decided_at: row.try_get("decided_at").map_err(internal)?,
    })
}

const fn source_name(value: ProposalSource) -> &'static str {
    match value {
        ProposalSource::AppAssistant => "app_assistant",
        ProposalSource::ChatGpt => "chat_gpt",
        ProposalSource::Codex => "codex",
        ProposalSource::ExternalMcp => "external_mcp",
    }
}

const fn kind_name(value: ProposalKind) -> &'static str {
    match value {
        ProposalKind::CreateItem => "create_item",
        ProposalKind::UpdateItem => "update_item",
        ProposalKind::GoalBreakdown => "goal_breakdown",
        ProposalKind::ConstraintChange => "constraint_change",
        ProposalKind::CalendarEvent => "calendar_event",
        ProposalKind::SchedulePlan => "schedule_plan",
        ProposalKind::Recommendation => "recommendation",
    }
}

const fn status_name(value: ProposalStatus) -> &'static str {
    match value {
        ProposalStatus::Pending => "pending",
        ProposalStatus::Accepted => "accepted",
        ProposalStatus::Rejected => "rejected",
        ProposalStatus::Expired => "expired",
    }
}

fn parse_source(value: &str) -> Result<ProposalSource, RepositoryError> {
    match value {
        "app_assistant" => Ok(ProposalSource::AppAssistant),
        "chat_gpt" => Ok(ProposalSource::ChatGpt),
        "codex" => Ok(ProposalSource::Codex),
        "external_mcp" => Ok(ProposalSource::ExternalMcp),
        _ => Err(RepositoryError::Internal),
    }
}

fn parse_kind(value: &str) -> Result<ProposalKind, RepositoryError> {
    match value {
        "create_item" => Ok(ProposalKind::CreateItem),
        "update_item" => Ok(ProposalKind::UpdateItem),
        "goal_breakdown" => Ok(ProposalKind::GoalBreakdown),
        "constraint_change" => Ok(ProposalKind::ConstraintChange),
        "calendar_event" => Ok(ProposalKind::CalendarEvent),
        "schedule_plan" => Ok(ProposalKind::SchedulePlan),
        "recommendation" => Ok(ProposalKind::Recommendation),
        _ => Err(RepositoryError::Internal),
    }
}

fn parse_status(value: &str) -> Result<ProposalStatus, RepositoryError> {
    match value {
        "pending" => Ok(ProposalStatus::Pending),
        "accepted" => Ok(ProposalStatus::Accepted),
        "rejected" => Ok(ProposalStatus::Rejected),
        "expired" => Ok(ProposalStatus::Expired),
        _ => Err(RepositoryError::Internal),
    }
}

fn revision_to_i64(revision: u64) -> Result<i64, RepositoryError> {
    i64::try_from(revision).map_err(|_| RepositoryError::Internal)
}

fn map_insert_error(error: &sqlx::Error, id: Uuid) -> RepositoryError {
    if error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "23505")
    {
        RepositoryError::Duplicate(id)
    } else {
        RepositoryError::Internal
    }
}

fn internal(_error: sqlx::Error) -> RepositoryError {
    RepositoryError::Internal
}
