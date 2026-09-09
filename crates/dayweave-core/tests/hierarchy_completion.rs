use std::collections::BTreeMap;

use dayweave_core::{
    HierarchyCompletionAction as Action, HierarchyCompletionCounts as Counts,
    HierarchyCompletionDecision as Decision, HierarchyCompletionError as Error,
    HierarchyCompletionItem as Item, HierarchyCompletionOverride as Override,
    HierarchyCompletionProvenance as Provenance, HierarchyCompletionReadiness as Readiness,
    HierarchyCompletionScope as Scope, HierarchyProgressStatus as Status, ItemId, OccurrenceId,
    evaluate_hierarchy_completion,
};
use uuid::Uuid;

fn id(value: u128) -> ItemId {
    ItemId(Uuid::from_u128(value))
}

fn item(value: u128, parent: Option<u128>, status: Status) -> Item {
    Item {
        id: id(value),
        parent_id: parent.map(id),
        status,
        required_for_parent: true,
        recurs: false,
        has_children_outside_plan: false,
        manual_override: Override::Automatic,
        provenance: None,
    }
}

fn evaluate(items: &[Item]) -> BTreeMap<ItemId, Decision> {
    let before = items.to_vec();
    let result = evaluate_hierarchy_completion(items, Scope::OneOff).unwrap();
    assert_eq!(result.scope, Scope::OneOff);
    assert_eq!(items, before, "the evaluator must not mutate its input");
    let mut reversed = items.to_vec();
    reversed.reverse();
    assert_eq!(
        evaluate_hierarchy_completion(&reversed, Scope::OneOff).unwrap(),
        result
    );
    result.decisions
}

fn occurrence(root: u128) -> Scope {
    Scope::Occurrence {
        root_id: id(root),
        occurrence_id: OccurrenceId(Uuid::from_u128(9000)),
    }
}

#[test]
fn required_descendants_complete_in_one_bottom_up_pass_and_retain_reopening_status() {
    let items = [
        item(1, None, Status::Blocked),
        item(2, Some(1), Status::Inbox),
        item(3, Some(2), Status::Completed),
    ];
    let result = evaluate(&items);
    for (value, prior) in [(1, Status::Blocked), (2, Status::Inbox)] {
        assert_eq!(result[&id(value)].status, Status::Completed);
        assert_eq!(result[&id(value)].action, Action::AutomaticallyCompleted);
        assert_eq!(
            result[&id(value)].provenance,
            Some(Provenance::Automatic {
                prior_open_status: prior
            })
        );
    }
    assert_eq!(
        result[&id(1)].counts,
        Counts {
            required_descendants: 2,
            completed: 2,
            ..Counts::default()
        }
    );
    assert_eq!(result[&id(3)].action, Action::Unchanged);
    assert_eq!(result[&id(3)].provenance, None);
}

#[test]
fn every_nonterminal_status_can_be_restored_exactly_after_regression() {
    for prior in [
        Status::Inbox,
        Status::Planned,
        Status::Scheduled,
        Status::InProgress,
        Status::Paused,
        Status::Blocked,
    ] {
        let mut parent = item(1, None, Status::Completed);
        parent.provenance = Some(Provenance::Automatic {
            prior_open_status: prior,
        });
        let result = evaluate(&[parent, item(2, Some(1), Status::Planned)]);
        assert_eq!(result[&id(1)].status, prior);
        assert_eq!(result[&id(1)].provenance, None);
        assert_eq!(result[&id(1)].action, Action::AutomaticallyReopened);
    }
}

#[test]
fn recorded_terminal_leaves_are_preserved_but_ambiguous_terminal_parents_are_rejected() {
    for terminal in [Status::Completed, Status::Skipped, Status::Cancelled] {
        let result = evaluate(&[item(1, None, terminal)]);
        assert_eq!(result[&id(1)].status, terminal);
        assert_eq!(result[&id(1)].action, Action::Unchanged);
        assert_eq!(result[&id(1)].provenance, None);
        for child_status in [Status::Completed, Status::Planned] {
            assert_eq!(
                evaluate_hierarchy_completion(
                    &[item(1, None, terminal), item(2, Some(1), child_status)],
                    Scope::OneOff
                ),
                Err(Error::AmbiguousTerminalParent(id(1)))
            );
        }
        let optional_child = Item {
            required_for_parent: false,
            ..item(2, Some(1), Status::Completed)
        };
        assert_eq!(
            evaluate_hierarchy_completion(
                &[item(1, None, terminal), optional_child],
                Scope::OneOff
            ),
            Err(Error::AmbiguousTerminalParent(id(1)))
        );
    }
}

#[test]
fn skipped_cancelled_and_every_open_lifecycle_remain_unmet() {
    for status in [
        Status::Inbox,
        Status::Planned,
        Status::Scheduled,
        Status::InProgress,
        Status::Paused,
        Status::Blocked,
        Status::Skipped,
        Status::Cancelled,
    ] {
        let result = evaluate(&[item(1, None, Status::Planned), item(2, Some(1), status)]);
        assert_eq!(result[&id(1)].status, Status::Planned);
        assert_eq!(
            result[&id(1)].readiness,
            Readiness::RequiredDescendantsIncomplete
        );
        assert_eq!(result[&id(1)].counts.incomplete, 1);
        assert_eq!(result[&id(2)].status, status);
    }
}

#[test]
fn optional_edge_cuts_the_entire_branch_but_does_not_disable_its_own_evaluation() {
    let mut optional = item(2, Some(1), Status::Planned);
    optional.required_for_parent = false;
    let result = evaluate(&[
        item(1, None, Status::Inbox),
        optional,
        item(3, Some(2), Status::Completed),
        item(4, Some(1), Status::Completed),
    ]);
    assert_eq!(
        result[&id(1)].counts,
        Counts {
            required_descendants: 1,
            completed: 1,
            ..Counts::default()
        }
    );
    assert_eq!(result[&id(1)].status, Status::Completed);
    assert_eq!(result[&id(2)].status, Status::Completed);
    assert_eq!(result[&id(2)].counts.required_descendants, 1);
}

#[test]
fn empty_and_all_optional_parents_never_complete_vacuously() {
    assert!(evaluate(&[]).is_empty());
    let result = evaluate(&[item(1, None, Status::Inbox)]);
    assert_eq!(result[&id(1)].readiness, Readiness::NoRequiredDescendants);
    assert_eq!(result[&id(1)].status, Status::Inbox);
    let mut optional = item(2, Some(1), Status::Completed);
    optional.required_for_parent = false;
    let result = evaluate(&[item(1, None, Status::Planned), optional]);
    assert_eq!(result[&id(1)].status, Status::Planned);
    assert_eq!(result[&id(1)].counts, Counts::default());
    let mut previously_complete = item(1, None, Status::Completed);
    previously_complete.provenance = Some(Provenance::Automatic {
        prior_open_status: Status::Paused,
    });
    assert_eq!(
        evaluate(&[previously_complete])[&id(1)].status,
        Status::Paused
    );
}

#[test]
fn manual_complete_does_not_waive_open_required_grandchildren() {
    let mut middle = item(2, Some(1), Status::Planned);
    middle.manual_override = Override::Complete;
    let result = evaluate(&[
        item(1, None, Status::Planned),
        middle,
        item(3, Some(2), Status::InProgress),
    ]);
    assert_eq!(result[&id(2)].status, Status::Completed);
    assert_eq!(result[&id(2)].action, Action::ManuallyCompleted);
    assert_eq!(result[&id(3)].status, Status::InProgress);
    assert_eq!(result[&id(1)].status, Status::Planned);
    assert_eq!(
        result[&id(1)].counts,
        Counts {
            required_descendants: 2,
            completed: 1,
            incomplete: 1,
            occurrence_evidence_required: 0
        }
    );
}

#[test]
fn optional_grandchild_is_cut_without_waiving_a_required_intermediate_node() {
    let mut middle = item(2, Some(1), Status::Planned);
    let mut leaf = item(3, Some(2), Status::InProgress);
    leaf.required_for_parent = false;
    let result = evaluate(&[item(1, None, Status::Planned), middle.clone(), leaf.clone()]);
    assert_eq!(result[&id(1)].counts.incomplete, 1);
    assert_eq!(result[&id(2)].readiness, Readiness::NoRequiredDescendants);
    middle.manual_override = Override::Complete;
    let result = evaluate(&[item(1, None, Status::Planned), middle, leaf]);
    assert_eq!(result[&id(1)].status, Status::Completed);
    assert_eq!(result[&id(1)].counts.required_descendants, 1);
    assert_eq!(result[&id(3)].status, Status::InProgress);
}

#[test]
fn keep_open_blocks_ancestors_even_when_its_own_requirements_are_complete() {
    for provenance in [
        None,
        Some(Provenance::Automatic {
            prior_open_status: Status::Blocked,
        }),
        Some(Provenance::Manual {
            prior_open_status: Status::Blocked,
        }),
    ] {
        let mut middle = item(
            2,
            Some(1),
            if provenance.is_some() {
                Status::Completed
            } else {
                Status::Blocked
            },
        );
        middle.provenance = provenance;
        middle.manual_override = Override::KeepOpen;
        let result = evaluate(&[
            item(1, None, Status::Inbox),
            middle,
            item(3, Some(2), Status::Completed),
        ]);
        assert_eq!(
            result[&id(2)].readiness,
            Readiness::AllRequiredDescendantsCompleted
        );
        assert_eq!(result[&id(2)].status, Status::Blocked);
        assert_eq!(result[&id(2)].provenance, None);
        assert_eq!(result[&id(1)].status, Status::Inbox);
        assert_eq!(result[&id(1)].counts.incomplete, 1);
    }
}

#[test]
fn releasing_manual_completion_requires_retained_prior_status() {
    let mut parent = item(1, None, Status::Completed);
    parent.provenance = Some(Provenance::Manual {
        prior_open_status: Status::Inbox,
    });
    let result = evaluate(&[parent.clone(), item(2, Some(1), Status::Planned)]);
    assert_eq!(result[&id(1)].status, Status::Inbox);
    assert_eq!(result[&id(1)].action, Action::ManualCompletionReleased);
    let result = evaluate(&[parent.clone(), item(2, Some(1), Status::Completed)]);
    assert_eq!(result[&id(1)].status, Status::Completed);
    assert_eq!(
        result[&id(1)].provenance,
        Some(Provenance::Automatic {
            prior_open_status: Status::Inbox
        })
    );
    parent.manual_override = Override::KeepOpen;
    assert_eq!(evaluate(&[parent])[&id(1)].status, Status::Inbox);
}

#[test]
fn repeated_manual_completion_preserves_prior_open_status_and_can_pin_auto_completion() {
    let mut parent = item(1, None, Status::Completed);
    parent.manual_override = Override::Complete;
    parent.provenance = Some(Provenance::Automatic {
        prior_open_status: Status::Paused,
    });
    let first = evaluate(&[parent.clone()])[&id(1)];
    assert_eq!(
        first.provenance,
        Some(Provenance::Manual {
            prior_open_status: Status::Paused
        })
    );
    assert_eq!(first.action, Action::ManuallyCompleted);
    parent.provenance = first.provenance;
    assert_eq!(evaluate(&[parent])[&id(1)].action, Action::Unchanged);
}

#[test]
fn unknown_prior_status_is_never_invented_for_manual_overrides() {
    for status in [Status::Completed, Status::Skipped, Status::Cancelled] {
        for requested in [Override::Complete, Override::KeepOpen] {
            let mut candidate = item(1, None, status);
            candidate.manual_override = requested;
            assert_eq!(
                evaluate_hierarchy_completion(&[candidate], Scope::OneOff),
                Err(Error::MissingPriorOpenStatus(id(1)))
            );
        }
    }
}

#[test]
fn provenance_must_match_recorded_completion_and_preserve_a_nonterminal_status() {
    for prior in [Status::Completed, Status::Skipped, Status::Cancelled] {
        for provenance in [
            Provenance::Automatic {
                prior_open_status: prior,
            },
            Provenance::Manual {
                prior_open_status: prior,
            },
        ] {
            let mut candidate = item(1, None, Status::Completed);
            candidate.provenance = Some(provenance);
            assert_eq!(
                evaluate_hierarchy_completion(&[candidate], Scope::OneOff),
                Err(Error::InvalidProvenance(id(1)))
            );
        }
    }
    let mut candidate = item(1, None, Status::Planned);
    candidate.provenance = Some(Provenance::Automatic {
        prior_open_status: Status::Inbox,
    });
    assert_eq!(
        evaluate_hierarchy_completion(&[candidate], Scope::OneOff),
        Err(Error::InvalidProvenance(id(1)))
    );
}

#[test]
fn recurring_template_statuses_do_not_prove_one_off_completion() {
    let mut recurring = item(2, Some(1), Status::Completed);
    recurring.recurs = true;
    let result = evaluate(&[
        item(1, None, Status::Planned),
        recurring.clone(),
        item(3, Some(2), Status::Completed),
    ]);
    assert_eq!(
        result[&id(1)].readiness,
        Readiness::OccurrenceEvidenceRequired
    );
    assert_eq!(result[&id(1)].status, Status::Planned);
    assert_eq!(result[&id(1)].counts.occurrence_evidence_required, 2);
    assert!(result[&id(3)].occurrence_evidence_required);
    recurring.required_for_parent = false;
    let result = evaluate(&[
        item(1, None, Status::Planned),
        recurring,
        item(3, Some(2), Status::Completed),
        item(4, Some(1), Status::Completed),
    ]);
    assert_eq!(result[&id(1)].status, Status::Completed);
    assert_eq!(result[&id(1)].counts.required_descendants, 1);
}

#[test]
fn unqualified_template_manual_completion_changes_neither_template_nor_ancestor() {
    let mut recurring = item(2, Some(1), Status::Planned);
    recurring.recurs = true;
    recurring.manual_override = Override::Complete;
    let result = evaluate(&[item(1, None, Status::Planned), recurring]);
    assert_eq!(result[&id(2)].status, Status::Planned);
    assert_eq!(result[&id(2)].provenance, None);
    assert_eq!(result[&id(2)].action, Action::OccurrenceEvidenceRequired);
    assert_eq!(result[&id(1)].status, Status::Planned);
    assert_eq!(
        result[&id(1)].readiness,
        Readiness::OccurrenceEvidenceRequired
    );
}

#[test]
fn explicit_occurrence_scope_qualifies_only_its_exact_complete_tree() {
    let mut root = item(1, None, Status::Planned);
    root.recurs = true;
    let items = [root.clone(), item(2, Some(1), Status::Completed)];
    assert_eq!(evaluate(&items)[&id(1)].status, Status::Planned);
    let result = evaluate_hierarchy_completion(&items, occurrence(1)).unwrap();
    assert_eq!(result.scope, occurrence(1));
    assert_eq!(result.decisions[&id(1)].status, Status::Completed);
    assert!(!result.decisions[&id(1)].occurrence_evidence_required);
    assert_eq!(items[0].status, Status::Planned);
    for invalid in [
        vec![],
        vec![item(1, None, Status::Planned)],
        vec![root.clone(), item(3, None, Status::Completed)],
        vec![
            item(4, None, Status::Planned),
            Item {
                parent_id: Some(id(4)),
                ..root
            },
        ],
    ] {
        assert_eq!(
            evaluate_hierarchy_completion(&invalid, occurrence(1)),
            Err(Error::InvalidOccurrenceScope)
        );
    }
}

#[test]
fn nested_independent_recurrence_is_not_qualified_by_outer_occurrence() {
    let mut root = item(1, None, Status::Planned);
    root.recurs = true;
    let mut nested = item(2, Some(1), Status::Completed);
    nested.recurs = true;
    let result = evaluate_hierarchy_completion(
        &[
            root.clone(),
            nested.clone(),
            item(3, Some(2), Status::Completed),
        ],
        occurrence(1),
    )
    .unwrap();
    assert_eq!(
        result.decisions[&id(1)].readiness,
        Readiness::OccurrenceEvidenceRequired
    );
    assert_eq!(
        result.decisions[&id(1)].counts.occurrence_evidence_required,
        2
    );
    nested.required_for_parent = false;
    let result = evaluate_hierarchy_completion(
        &[root, nested, item(3, Some(1), Status::Completed)],
        occurrence(1),
    )
    .unwrap();
    assert_eq!(result.decisions[&id(1)].status, Status::Completed);
}

#[test]
fn unqualified_nested_occurrence_fences_every_override_and_retains_provenance() {
    for requested in [Override::Automatic, Override::KeepOpen, Override::Complete] {
        let mut root = item(1, None, Status::Completed);
        root.recurs = true;
        root.provenance = Some(Provenance::Automatic {
            prior_open_status: Status::Inbox,
        });
        let mut nested = item(2, Some(1), Status::Completed);
        nested.recurs = true;
        nested.manual_override = requested;
        nested.provenance = Some(Provenance::Automatic {
            prior_open_status: Status::Paused,
        });
        let result = evaluate_hierarchy_completion(
            &[root, nested.clone(), item(3, Some(2), Status::Planned)],
            occurrence(1),
        )
        .unwrap();
        assert_eq!(result.decisions[&id(1)].status, Status::Inbox);
        assert_eq!(
            result.decisions[&id(1)].action,
            Action::AutomaticallyReopened
        );
        assert_eq!(result.decisions[&id(2)].status, nested.status);
        assert_eq!(result.decisions[&id(2)].provenance, nested.provenance);
        assert_eq!(
            result.decisions[&id(2)].action,
            Action::OccurrenceEvidenceRequired
        );
        assert_eq!(result.decisions[&id(3)].status, Status::Planned);
    }
}

#[test]
fn unqualified_one_off_branch_reopens_only_its_qualified_ancestor() {
    let parent = Item {
        provenance: Some(Provenance::Automatic {
            prior_open_status: Status::Blocked,
        }),
        ..item(1, None, Status::Completed)
    };
    let recurring = Item {
        recurs: true,
        provenance: Some(Provenance::Automatic {
            prior_open_status: Status::Paused,
        }),
        ..item(2, Some(1), Status::Completed)
    };
    let result = evaluate(&[parent, recurring.clone()]);
    assert_eq!(result[&id(1)].status, Status::Blocked);
    assert_eq!(result[&id(2)].status, Status::Completed);
    assert_eq!(result[&id(2)].provenance, recurring.provenance);
    assert_eq!(result[&id(2)].action, Action::OccurrenceEvidenceRequired);
}

#[test]
fn required_edge_combinations_match_an_independent_descendant_walk() {
    for mask in 0_u8..16 {
        for leaf_status in [
            Status::Completed,
            Status::Planned,
            Status::Skipped,
            Status::Cancelled,
        ] {
            let mut items = [
                item(1, None, Status::Inbox),
                item(2, Some(1), Status::Planned),
                item(3, Some(1), Status::Completed),
                item(4, Some(2), leaf_status),
                item(5, Some(2), Status::Completed),
            ];
            for (bit, node) in items.iter_mut().skip(1).enumerate() {
                node.required_for_parent = mask & (1 << bit) != 0;
            }
            let result = evaluate(&items);
            for candidate in &items {
                let mut ready = vec![candidate.id];
                let mut descendants = Vec::new();
                while let Some(parent) = ready.pop() {
                    for child in items
                        .iter()
                        .filter(|node| node.parent_id == Some(parent) && node.required_for_parent)
                    {
                        descendants.push(child.id);
                        ready.push(child.id);
                    }
                }
                let completed = descendants
                    .iter()
                    .filter(|id| result[id].status == Status::Completed)
                    .count() as u64;
                assert_eq!(
                    result[&candidate.id].counts,
                    Counts {
                        required_descendants: descendants.len() as u64,
                        completed,
                        incomplete: descendants.len() as u64 - completed,
                        occurrence_evidence_required: 0,
                    }
                );
                if candidate.status == Status::Inbox || candidate.status == Status::Planned {
                    assert_eq!(
                        result[&candidate.id].status == Status::Completed,
                        !descendants.is_empty() && completed == descendants.len() as u64
                    );
                }
            }
        }
    }
}

#[test]
fn malformed_forest_or_scope_returns_no_partial_decisions() {
    let missing = item(1, Some(99), Status::Planned);
    let incomplete = Item {
        has_children_outside_plan: true,
        ..item(1, None, Status::Planned)
    };
    for (items, expected) in [
        (vec![item(0, None, Status::Planned)], Error::InvalidId),
        (vec![item(1, Some(0), Status::Planned)], Error::InvalidId),
        (
            vec![
                item(1, None, Status::Planned),
                item(1, None, Status::Completed),
            ],
            Error::DuplicateItem(id(1)),
        ),
        (
            vec![missing],
            Error::MissingParent {
                item: id(1),
                parent: id(99),
            },
        ),
        (vec![incomplete], Error::IncompleteTopology(id(1))),
        (vec![item(1, Some(1), Status::Planned)], Error::Cycle(id(1))),
        (
            vec![
                item(1, Some(2), Status::Planned),
                item(2, Some(1), Status::Completed),
                item(3, None, Status::Planned),
            ],
            Error::Cycle(id(1)),
        ),
    ] {
        assert_eq!(
            evaluate_hierarchy_completion(&items, Scope::OneOff),
            Err(expected)
        );
    }
    for scope in [
        occurrence(0),
        Scope::Occurrence {
            root_id: id(1),
            occurrence_id: OccurrenceId(Uuid::nil()),
        },
    ] {
        assert_eq!(
            evaluate_hierarchy_completion(&[], scope),
            Err(Error::InvalidId)
        );
    }
}

#[test]
fn five_thousand_levels_complete_and_reopen_iteratively_without_double_counting() {
    let mut items: Vec<_> = (1..=5_000)
        .map(|value| {
            item(
                value,
                (value > 1).then_some(value - 1),
                if value == 5_000 {
                    Status::Completed
                } else {
                    Status::Planned
                },
            )
        })
        .collect();
    let result = evaluate(&items);
    assert_eq!(
        result[&id(1)].counts,
        Counts {
            required_descendants: 4_999,
            completed: 4_999,
            ..Counts::default()
        }
    );
    for candidate in &mut items {
        let decision = result[&candidate.id];
        candidate.status = decision.status;
        candidate.provenance = decision.provenance;
    }
    assert!(
        evaluate(&items)
            .values()
            .all(|decision| decision.action == Action::Unchanged)
    );
    items.last_mut().unwrap().status = Status::Paused;
    let result = evaluate(&items);
    assert_eq!(result[&id(1)].status, Status::Planned);
    assert_eq!(result[&id(1)].counts.incomplete, 4_999);
    assert!(
        result
            .values()
            .all(|decision| decision.status != Status::Completed)
    );
}
