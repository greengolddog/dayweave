use std::{sync::Arc, time::Duration as StdDuration};

use chrono::{DateTime, Duration, Utc};
use thiserror::Error;
use uuid::Uuid;

use super::{
    DecisionKind, EditProposal, NewProposal, Proposal, ProposalDomainError, ProposalQuery,
    ProposalRepository, RepositoryError,
};

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Clone, Copy, Debug)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

pub struct ProposalService {
    repository: Arc<dyn ProposalRepository>,
    clock: Arc<dyn Clock>,
    default_ttl: Duration,
}

impl std::fmt::Debug for ProposalService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProposalService")
            .field("default_ttl", &self.default_ttl)
            .finish_non_exhaustive()
    }
}

impl ProposalService {
    /// Creates a proposal service over repository and clock ports.
    ///
    /// # Panics
    ///
    /// Panics only when `default_ttl` exceeds the range supported by
    /// [`chrono::Duration`]. Configuration constrains it to a practical value.
    #[must_use]
    pub fn new(
        repository: Arc<dyn ProposalRepository>,
        clock: Arc<dyn Clock>,
        default_ttl: StdDuration,
    ) -> Self {
        Self {
            repository,
            clock,
            default_ttl: Duration::from_std(default_ttl)
                .expect("proposal TTL must fit in chrono::Duration"),
        }
    }

    #[must_use]
    pub fn default_expiration(&self) -> DateTime<Utc> {
        self.clock.now() + self.default_ttl
    }

    /// Validates and stores a proposal.
    ///
    /// # Errors
    ///
    /// Returns a domain or repository error when validation or persistence fails.
    pub async fn create(&self, input: NewProposal) -> Result<Proposal, ProposalServiceError> {
        let now = self.clock.now();
        let proposal = Proposal::new(input, now)?;
        Ok(self.repository.insert(proposal).await?)
    }

    /// Fetches a proposal and durably expires it when its TTL has elapsed.
    ///
    /// # Errors
    ///
    /// Returns a repository error when the proposal is missing or persistence fails.
    pub async fn get(&self, id: Uuid) -> Result<Proposal, ProposalServiceError> {
        let proposal = self.repository.get(id).await?;
        self.normalize_expiration(proposal).await
    }

    /// Lists matching proposals and normalizes expiration state.
    ///
    /// # Errors
    ///
    /// Returns a repository error when reading or persisting expiration fails.
    pub async fn list(&self, query: ProposalQuery) -> Result<Vec<Proposal>, ProposalServiceError> {
        // Normalize every source-matching candidate before applying status and
        // limit; otherwise newly due proposals can be absent from an expired query.
        let proposals = self
            .repository
            .list(ProposalQuery {
                status: None,
                source: query.source,
                limit: usize::MAX,
            })
            .await?;
        let mut normalized = Vec::with_capacity(proposals.len());
        for proposal in proposals {
            normalized.push(self.normalize_expiration(proposal).await?);
        }
        normalized.retain(|proposal| query.status.is_none_or(|status| proposal.status == status));
        normalized.truncate(query.limit);
        Ok(normalized)
    }

    /// Edits a pending proposal using optimistic concurrency.
    ///
    /// # Errors
    ///
    /// Returns a domain error for invalid edits or a repository error for missing
    /// proposals, stale revisions, and persistence failures.
    pub async fn edit(
        &self,
        id: Uuid,
        expected_revision: u64,
        edit: EditProposal,
    ) -> Result<Proposal, ProposalServiceError> {
        let mut proposal = self.get(id).await?;
        ensure_revision(&proposal, expected_revision)?;
        proposal.edit(edit, self.clock.now())?;
        Ok(self.repository.replace(proposal, expected_revision).await?)
    }

    /// Accepts or rejects a pending proposal using optimistic concurrency.
    ///
    /// # Errors
    ///
    /// Returns a domain error for invalid transitions or a repository error for
    /// missing proposals, stale revisions, and persistence failures.
    pub async fn decide(
        &self,
        id: Uuid,
        expected_revision: u64,
        decision: DecisionKind,
        note: Option<String>,
    ) -> Result<Proposal, ProposalServiceError> {
        let mut proposal = self.get(id).await?;
        ensure_revision(&proposal, expected_revision)?;
        proposal.decide(decision, note, self.clock.now())?;
        Ok(self.repository.replace(proposal, expected_revision).await?)
    }

    /// Deletes a proposal at the expected revision.
    ///
    /// # Errors
    ///
    /// Returns a repository error when the proposal is missing, stale, or cannot
    /// be deleted.
    pub async fn delete(
        &self,
        id: Uuid,
        expected_revision: u64,
    ) -> Result<(), ProposalServiceError> {
        Ok(self.repository.delete(id, expected_revision).await?)
    }

    async fn normalize_expiration(
        &self,
        mut proposal: Proposal,
    ) -> Result<Proposal, ProposalServiceError> {
        let expected_revision = proposal.revision;
        if !proposal.expire_if_due(self.clock.now()) {
            return Ok(proposal);
        }

        let proposal_id = proposal.id;
        match self.repository.replace(proposal, expected_revision).await {
            Ok(updated) => Ok(updated),
            Err(RepositoryError::RevisionConflict { .. }) => {
                Ok(self.repository.get(proposal_id).await?)
            }
            Err(error) => Err(error.into()),
        }
    }
}

fn ensure_revision(
    proposal: &Proposal,
    expected_revision: u64,
) -> Result<(), ProposalServiceError> {
    if proposal.revision == expected_revision {
        Ok(())
    } else {
        Err(RepositoryError::RevisionConflict {
            expected: expected_revision,
            actual: proposal.revision,
        }
        .into())
    }
}

#[derive(Debug, Error)]
pub enum ProposalServiceError {
    #[error(transparent)]
    Domain(#[from] ProposalDomainError),
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}
