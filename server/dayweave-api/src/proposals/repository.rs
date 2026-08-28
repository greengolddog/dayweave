use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::RwLock;
use uuid::Uuid;

use super::{Proposal, ProposalSource, ProposalStatus};

#[derive(Clone, Copy, Debug, Default)]
pub struct ProposalQuery {
    pub status: Option<ProposalStatus>,
    pub source: Option<ProposalSource>,
    pub limit: usize,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RepositoryError {
    #[error("proposal {0} was not found")]
    NotFound(Uuid),
    #[error("proposal {0} already exists")]
    Duplicate(Uuid),
    #[error("revision conflict: expected {expected}, found {actual}")]
    RevisionConflict { expected: u64, actual: u64 },
    #[error("repository operation failed")]
    Internal,
}

#[async_trait]
pub trait ProposalRepository: Send + Sync {
    async fn insert(&self, proposal: Proposal) -> Result<Proposal, RepositoryError>;
    async fn get(&self, id: Uuid) -> Result<Proposal, RepositoryError>;
    async fn list(&self, query: ProposalQuery) -> Result<Vec<Proposal>, RepositoryError>;
    async fn replace(
        &self,
        proposal: Proposal,
        expected_revision: u64,
    ) -> Result<Proposal, RepositoryError>;
    async fn delete(&self, id: Uuid, expected_revision: u64) -> Result<(), RepositoryError>;
}

#[derive(Clone, Debug, Default)]
pub struct InMemoryProposalRepository {
    proposals: Arc<RwLock<HashMap<Uuid, Proposal>>>,
}

#[async_trait]
impl ProposalRepository for InMemoryProposalRepository {
    async fn insert(&self, proposal: Proposal) -> Result<Proposal, RepositoryError> {
        let mut proposals = self.proposals.write().await;
        if proposals.contains_key(&proposal.id) {
            return Err(RepositoryError::Duplicate(proposal.id));
        }
        proposals.insert(proposal.id, proposal.clone());
        Ok(proposal)
    }

    async fn get(&self, id: Uuid) -> Result<Proposal, RepositoryError> {
        self.proposals
            .read()
            .await
            .get(&id)
            .cloned()
            .ok_or(RepositoryError::NotFound(id))
    }

    async fn list(&self, query: ProposalQuery) -> Result<Vec<Proposal>, RepositoryError> {
        let mut proposals: Vec<_> = self
            .proposals
            .read()
            .await
            .values()
            .filter(|proposal| query.status.is_none_or(|status| proposal.status == status))
            .filter(|proposal| query.source.is_none_or(|source| proposal.source == source))
            .cloned()
            .collect();
        proposals.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        proposals.truncate(query.limit);
        Ok(proposals)
    }

    async fn replace(
        &self,
        proposal: Proposal,
        expected_revision: u64,
    ) -> Result<Proposal, RepositoryError> {
        let mut proposals = self.proposals.write().await;
        let current = proposals
            .get(&proposal.id)
            .ok_or(RepositoryError::NotFound(proposal.id))?;
        if current.revision != expected_revision {
            return Err(RepositoryError::RevisionConflict {
                expected: expected_revision,
                actual: current.revision,
            });
        }
        proposals.insert(proposal.id, proposal.clone());
        Ok(proposal)
    }

    async fn delete(&self, id: Uuid, expected_revision: u64) -> Result<(), RepositoryError> {
        let mut proposals = self.proposals.write().await;
        let current = proposals.get(&id).ok_or(RepositoryError::NotFound(id))?;
        if current.revision != expected_revision {
            return Err(RepositoryError::RevisionConflict {
                expected: expected_revision,
                actual: current.revision,
            });
        }
        proposals.remove(&id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use serde_json::json;

    use crate::proposals::{NewProposal, ProposalKind};

    use super::*;

    fn proposal(title: &str, source: ProposalSource) -> Proposal {
        let now = Utc::now();
        Proposal::new(
            NewProposal {
                submitted_by: "test-user".to_owned(),
                source,
                source_reference: None,
                kind: ProposalKind::Recommendation,
                title: title.to_owned(),
                explanation: None,
                payload: json!({}),
                expires_at: now + Duration::days(7),
            },
            now,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn protects_replacements_with_revision_check() {
        let repository = InMemoryProposalRepository::default();
        let original = repository
            .insert(proposal("Original", ProposalSource::Codex))
            .await
            .unwrap();
        let mut changed = original.clone();
        changed.title = "Changed".to_owned();
        changed.revision = 2;

        assert_eq!(
            repository.replace(changed.clone(), 99).await.unwrap_err(),
            RepositoryError::RevisionConflict {
                expected: 99,
                actual: 1
            }
        );
        assert_eq!(
            repository.replace(changed, 1).await.unwrap().title,
            "Changed"
        );
    }

    #[tokio::test]
    async fn filters_and_limits_results() {
        let repository = InMemoryProposalRepository::default();
        repository
            .insert(proposal("Codex", ProposalSource::Codex))
            .await
            .unwrap();
        repository
            .insert(proposal("ChatGPT", ProposalSource::ChatGpt))
            .await
            .unwrap();

        let results = repository
            .list(ProposalQuery {
                source: Some(ProposalSource::Codex),
                limit: 1,
                ..ProposalQuery::default()
            })
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Codex");
    }
}
