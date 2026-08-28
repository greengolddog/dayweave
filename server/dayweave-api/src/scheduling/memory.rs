use std::{collections::HashMap, fmt::Write, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use super::{
    ConflictQuery, ConflictReport, ItemSearchQuery, ItemSearchResult, ItemSummary,
    PlacementExplanation, PlanOperationKind, PlanningSimulationPort, ScheduleAccess,
    ScheduleBlockView, ScheduleConflict, ScheduleDetail, ScheduleQuery, ScheduleQueryPort,
    ScheduleView, SchedulingPortError, SimulatedBlockMove, SimulationIssue, SimulationRequest,
    SimulationResult, StoredItem, StoredSchedule,
};

#[derive(Clone, Debug)]
pub struct InMemoryScheduleQueryPort {
    schedule: Arc<StoredSchedule>,
    items: Arc<Vec<StoredItem>>,
    explanations: Arc<HashMap<String, PlacementExplanation>>,
    conflicts: Arc<Vec<ScheduleConflict>>,
}

impl InMemoryScheduleQueryPort {
    #[must_use]
    pub fn new(
        mut schedule: StoredSchedule,
        mut items: Vec<StoredItem>,
        explanations: Vec<PlacementExplanation>,
        mut conflicts: Vec<ScheduleConflict>,
    ) -> Self {
        schedule.blocks.sort_by(|left, right| {
            left.start
                .cmp(&right.start)
                .then_with(|| left.id.cmp(&right.id))
        });
        items.sort_by(|left, right| left.title.cmp(&right.title).then(left.id.cmp(&right.id)));
        conflicts.sort_by(|left, right| {
            left.start
                .cmp(&right.start)
                .then_with(|| left.id.cmp(&right.id))
        });
        Self {
            schedule: Arc::new(schedule),
            items: Arc::new(items),
            explanations: Arc::new(
                explanations
                    .into_iter()
                    .map(|explanation| (explanation.block_id.clone(), explanation))
                    .collect(),
            ),
            conflicts: Arc::new(conflicts),
        }
    }

    #[must_use]
    pub fn stored_schedule(&self) -> &StoredSchedule {
        &self.schedule
    }
}

#[async_trait]
impl ScheduleQueryPort for InMemoryScheduleQueryPort {
    async fn get_schedule(
        &self,
        access: &ScheduleAccess,
        query: ScheduleQuery,
    ) -> Result<ScheduleView, SchedulingPortError> {
        validate_range(query.start, query.end)?;
        let mut redacted_count = 0;
        let blocks = self
            .schedule
            .blocks
            .iter()
            .filter(|block| block.start < query.end && block.end > query.start)
            .map(|block| {
                let sensitive = block.sensitive && !access.include_sensitive;
                let busy_only = query.detail == ScheduleDetail::BusyOnly;
                if sensitive {
                    redacted_count += 1;
                }
                ScheduleBlockView {
                    id: (!sensitive && !busy_only).then(|| block.id.clone()),
                    item_id: (!sensitive && query.detail == ScheduleDetail::Full)
                        .then(|| block.item_id.clone())
                        .flatten(),
                    title: (!sensitive && !busy_only).then(|| block.title.clone()),
                    start: block.start,
                    end: block.end,
                    kind: if sensitive {
                        "busy".to_owned()
                    } else {
                        block.kind.clone()
                    },
                    status: block.status.clone(),
                    redacted: sensitive || busy_only,
                }
            })
            .collect();
        Ok(ScheduleView {
            revision: self.schedule.revision.clone(),
            timezone: self.schedule.timezone.clone(),
            start: query.start,
            end: query.end,
            blocks,
            redacted_count,
        })
    }

    async fn search_items(
        &self,
        access: &ScheduleAccess,
        query: ItemSearchQuery,
    ) -> Result<ItemSearchResult, SchedulingPortError> {
        if query.limit == 0 || query.limit > 100 {
            return Err(SchedulingPortError::InvalidQuery(
                "limit must be between 1 and 100".to_owned(),
            ));
        }
        if let (Some(start), Some(end)) = (query.start, query.end) {
            validate_range(start, end)?;
        }
        let text = query.text.as_ref().map(|value| value.to_lowercase());
        let mut redacted_count = 0;
        let mut items = Vec::new();
        for item in self.items.iter() {
            if item.sensitive && !access.include_sensitive {
                redacted_count += 1;
                continue;
            }
            if text
                .as_ref()
                .is_some_and(|text| !item.title.to_lowercase().contains(text))
                || query
                    .status
                    .as_ref()
                    .is_some_and(|value| &item.status != value)
                || query.kind.as_ref().is_some_and(|value| &item.kind != value)
                || query
                    .project_id
                    .as_ref()
                    .is_some_and(|value| item.project_id.as_ref() != Some(value))
                || query
                    .goal_id
                    .as_ref()
                    .is_some_and(|value| item.goal_id.as_ref() != Some(value))
                || query
                    .start
                    .is_some_and(|start| item.scheduled_start.is_none_or(|value| value < start))
                || query
                    .end
                    .is_some_and(|end| item.scheduled_start.is_none_or(|value| value >= end))
            {
                continue;
            }
            items.push(ItemSummary {
                id: item.id.clone(),
                title: item.title.clone(),
                status: item.status.clone(),
                kind: item.kind.clone(),
                project_id: item.project_id.clone(),
                goal_id: item.goal_id.clone(),
                deadline: item.deadline,
                scheduled_start: item.scheduled_start,
            });
            if items.len() == query.limit {
                break;
            }
        }
        Ok(ItemSearchResult {
            revision: self.schedule.revision.clone(),
            items,
            redacted_count,
        })
    }

    async fn explain_placement(
        &self,
        access: &ScheduleAccess,
        block_id: &str,
    ) -> Result<PlacementExplanation, SchedulingPortError> {
        let explanation = self
            .explanations
            .get(block_id)
            .ok_or(SchedulingPortError::NotFound)?;
        if explanation.sensitive && !access.include_sensitive {
            return Err(SchedulingPortError::NotFound);
        }
        Ok(explanation.clone())
    }

    async fn get_conflicts(
        &self,
        access: &ScheduleAccess,
        query: ConflictQuery,
    ) -> Result<ConflictReport, SchedulingPortError> {
        validate_range(query.start, query.end)?;
        let mut redacted_count = 0;
        let conflicts = self
            .conflicts
            .iter()
            .filter(|conflict| {
                conflict.start.is_none_or(|start| start < query.end)
                    && conflict.end.is_none_or(|end| end > query.start)
            })
            .filter_map(|conflict| {
                if conflict.sensitive && !access.include_sensitive {
                    redacted_count += 1;
                    None
                } else {
                    Some(conflict.clone())
                }
            })
            .collect();
        Ok(ConflictReport {
            revision: self.schedule.revision.clone(),
            conflicts,
            redacted_count,
        })
    }
}

#[derive(Clone, Debug)]
pub struct InMemorySimulationPort {
    schedule: Arc<StoredSchedule>,
    simulations: Arc<RwLock<HashMap<String, (String, SimulationResult)>>>,
}

impl InMemorySimulationPort {
    #[must_use]
    pub fn new(schedule: StoredSchedule) -> Self {
        Self {
            schedule: Arc::new(schedule),
            simulations: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    #[must_use]
    pub fn stored_schedule(&self) -> &StoredSchedule {
        &self.schedule
    }
}

#[async_trait]
impl PlanningSimulationPort for InMemorySimulationPort {
    async fn simulate(
        &self,
        access: &ScheduleAccess,
        request: SimulationRequest,
    ) -> Result<SimulationResult, SchedulingPortError> {
        if request.base_revision != self.schedule.revision {
            return Err(SchedulingPortError::RevisionConflict {
                current_revision: self.schedule.revision.clone(),
            });
        }
        if request.operations.is_empty() || request.operations.len() > 100 {
            return Err(SchedulingPortError::InvalidQuery(
                "operations must contain between 1 and 100 entries".to_owned(),
            ));
        }

        let request_digest = simulation_request_digest(&request)?;
        let token = simulation_token(&access.subject, &request_digest);
        if let Some((subject, result)) = self.simulations.read().await.get(&token)
            && subject == &access.subject
        {
            return Ok(result.clone());
        }

        let mut moved_blocks = Vec::new();
        let mut warnings = Vec::new();
        for operation in &request.operations {
            match operation.kind {
                PlanOperationKind::MoveBlock => {
                    let Some(block_id) = operation.target_id.as_ref() else {
                        return Err(SchedulingPortError::InvalidQuery(
                            "move_block requires target_id".to_owned(),
                        ));
                    };
                    let Some(block) = self
                        .schedule
                        .blocks
                        .iter()
                        .find(|block| &block.id == block_id)
                    else {
                        warnings.push(issue(
                            "unknown_block",
                            "The requested block no longer exists.",
                            vec![block_id.clone()],
                        ));
                        continue;
                    };
                    if block.sensitive && !access.include_sensitive {
                        warnings.push(issue(
                            "redacted_block",
                            "A private block cannot be changed through this integration.",
                            Vec::new(),
                        ));
                        continue;
                    }
                    let start = operation
                        .parameters
                        .get("start")
                        .and_then(ValueExt::as_datetime)
                        .ok_or_else(|| {
                            SchedulingPortError::InvalidQuery(
                                "move_block parameters.start must be an RFC 3339 instant"
                                    .to_owned(),
                            )
                        })?;
                    let duration = block.end - block.start;
                    moved_blocks.push(SimulatedBlockMove {
                        block_id: block.id.clone(),
                        previous_start: block.start,
                        previous_end: block.end,
                        proposed_start: start,
                        proposed_end: start + duration,
                    });
                }
                PlanOperationKind::DeleteItem => warnings.push(issue(
                    "confirmation_required",
                    "Deletion remains a proposal and requires explicit confirmation in DayWeave.",
                    operation.target_id.clone().into_iter().collect(),
                )),
                _ => {}
            }
        }

        let result = SimulationResult {
            simulation_token: token.clone(),
            request_digest,
            base_revision: request.base_revision,
            moved_blocks,
            unscheduled_item_ids: Vec::new(),
            violations: Vec::new(),
            warnings,
        };
        self.simulations
            .write()
            .await
            .insert(token, (access.subject.clone(), result.clone()));
        Ok(result)
    }

    async fn consume_simulation(
        &self,
        access: &ScheduleAccess,
        token: &str,
        expected_request_digest: &str,
    ) -> Result<SimulationResult, SchedulingPortError> {
        let mut simulations = self.simulations.write().await;
        if let Some((subject, result)) = simulations.get(token)
            && subject == &access.subject
        {
            if result.request_digest != expected_request_digest {
                return Err(SchedulingPortError::InvalidQuery(
                    "simulation token does not match the submitted operations".to_owned(),
                ));
            }
            return simulations
                .remove(token)
                .map(|(_, result)| result)
                .ok_or(SchedulingPortError::NotFound);
        }
        Err(SchedulingPortError::NotFound)
    }
}

fn validate_range(start: DateTime<Utc>, end: DateTime<Utc>) -> Result<(), SchedulingPortError> {
    if end <= start {
        return Err(SchedulingPortError::InvalidQuery(
            "end must be after start".to_owned(),
        ));
    }
    if end - start > chrono::Duration::days(90) {
        return Err(SchedulingPortError::InvalidQuery(
            "date range must not exceed 90 days".to_owned(),
        ));
    }
    Ok(())
}

/// Returns a stable digest binding a simulation to its exact operations and
/// assumptions.
///
/// # Errors
///
/// Returns [`SchedulingPortError`] when the typed request cannot be encoded.
pub fn simulation_request_digest(
    request: &SimulationRequest,
) -> Result<String, SchedulingPortError> {
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(request).map_err(|_| {
        SchedulingPortError::InvalidQuery("simulation request cannot be encoded".to_owned())
    })?);
    let digest = hasher.finalize();
    Ok(digest[..16]
        .iter()
        .fold(String::with_capacity(32), |mut encoded, byte| {
            let _ = write!(encoded, "{byte:02x}");
            encoded
        }))
}

fn simulation_token(subject: &str, request_digest: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(subject.as_bytes());
    hasher.update([0]);
    hasher.update(request_digest.as_bytes());
    let digest = hasher.finalize();
    let token = digest[..16]
        .iter()
        .fold(String::with_capacity(32), |mut encoded, byte| {
            let _ = write!(encoded, "{byte:02x}");
            encoded
        });
    format!("sim_{token}")
}

fn issue(code: &str, message: &str, related_ids: Vec<String>) -> SimulationIssue {
    SimulationIssue {
        code: code.to_owned(),
        message: message.to_owned(),
        related_ids,
    }
}

trait ValueExt {
    fn as_datetime(&self) -> Option<DateTime<Utc>>;
}

impl ValueExt for serde_json::Value {
    fn as_datetime(&self) -> Option<DateTime<Utc>> {
        self.as_str()?.parse().ok()
    }
}
