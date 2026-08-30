//! Strict structural pass for permissive core input types.

#![allow(dead_code)] // Shape fields are intentionally consumed and discarded by Serde.

use serde::Deserialize;
use serde::de::IgnoredAny;

pub(crate) fn validate(value: &serde_json::Value) -> Result<(), ()> {
    PlanRequestShape::deserialize(value)
        .map(|_| ())
        .map_err(|_| ())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanRequestShape {
    as_of: IgnoredAny,
    horizon_start: IgnoredAny,
    horizon_end: IgnoredAny,
    items: Vec<WorkItemShape>,
    availability: Vec<AvailabilityShape>,
    fixed_blocks: Vec<FixedBlockShape>,
    previous_assignments: Vec<PreviousAssignmentShape>,
    config: SchedulerConfigShape,
    #[serde(default)]
    recurrence_context: Option<IgnoredAny>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkItemShape {
    id: IgnoredAny,
    is_sensitive: IgnoredAny,
    revision: IgnoredAny,
    title: IgnoredAny,
    kind: ItemKindShape,
    status: IgnoredAny,
    parent_id: Option<IgnoredAny>,
    sibling_order: Option<IgnoredAny>,
    has_own_effort: IgnoredAny,
    goal_ids: IgnoredAny,
    priority: PriorityShape,
    duration: Option<DurationEstimateShape>,
    constraints: IgnoredAny,
    split_policy: SplitPolicyShape,
    energy: Option<IgnoredAny>,
    tags: IgnoredAny,
    created_at: IgnoredAny,
    updated_at: IgnoredAny,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PriorityShape {
    importance: IgnoredAny,
    urgency: IgnoredAny,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DurationEstimateShape {
    minimum: IgnoredAny,
    expected: IgnoredAny,
    maximum: IgnoredAny,
    remaining: Option<IgnoredAny>,
    source: IgnoredAny,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ItemKindShape {
    Task {},
    RecurringTask {
        recurrence: IgnoredAny,
    },
    Habit {
        recurrence: IgnoredAny,
        target: Option<IgnoredAny>,
        preserves_streak_when_paused: IgnoredAny,
    },
    Routine {
        ordered: IgnoredAny,
        recurrence: Option<IgnoredAny>,
    },
    Goal {
        measures: IgnoredAny,
        weekly_allocation: Option<IgnoredAny>,
    },
    Break {
        category: IgnoredAny,
        mandatory: IgnoredAny,
        prompt_to_resume: IgnoredAny,
    },
    CalendarEvent {
        start: IgnoredAny,
        end: IgnoredAny,
        immutable: IgnoredAny,
        all_day: IgnoredAny,
        source_calendar_id: Option<IgnoredAny>,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum SplitPolicyShape {
    Indivisible {},
    Splittable {
        minimum_session: IgnoredAny,
        maximum_session: IgnoredAny,
        maximum_sessions: IgnoredAny,
        minimum_gap: IgnoredAny,
        maximum_days: Option<IgnoredAny>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AvailabilityShape {
    start: IgnoredAny,
    end: IgnoredAny,
    contexts: IgnoredAny,
    location: Option<IgnoredAny>,
    energy: IgnoredAny,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FixedBlockShape {
    id: IgnoredAny,
    is_sensitive: IgnoredAny,
    title: IgnoredAny,
    start: IgnoredAny,
    end: IgnoredAny,
    source: IgnoredAny,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviousAssignmentShape {
    item_id: IgnoredAny,
    occurrence_id: Option<IgnoredAny>,
    blocks: Vec<PreviousBlockShape>,
    pinned: IgnoredAny,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviousBlockShape {
    start: IgnoredAny,
    end: IgnoredAny,
    session_index: IgnoredAny,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SchedulerConfigShape {
    slot_granularity: IgnoredAny,
    stability_weight: IgnoredAny,
    default_soft_weight: IgnoredAny,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_extra_fields_buffered_by_internally_tagged_enums() {
        let value = serde_json::json!({
            "as_of": null,
            "horizon_start": null,
            "horizon_end": null,
            "items": [{
                "id": null,
                "is_sensitive": null,
                "revision": null,
                "title": null,
                "kind": {"type": "task", "extra": true},
                "status": null,
                "parent_id": null,
                "sibling_order": null,
                "has_own_effort": null,
                "goal_ids": null,
                "priority": {"importance": null, "urgency": null},
                "duration": null,
                "constraints": null,
                "split_policy": {"type": "indivisible"},
                "energy": null,
                "tags": null,
                "created_at": null,
                "updated_at": null
            }],
            "availability": [],
            "fixed_blocks": [],
            "previous_assignments": [],
            "config": {
                "slot_granularity": null,
                "stability_weight": null,
                "default_soft_weight": null
            }
        });
        assert_eq!(validate(&value), Err(()));
    }
}
