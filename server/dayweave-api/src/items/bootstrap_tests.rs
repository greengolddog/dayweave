use super::*;
use crate::items::NewItem;
use serde_json::json;

fn item(id: u128) -> Item {
    let input: NewItem = serde_json::from_value(json!({"id":Uuid::from_u128(id),
        "is_sensitive":false,"kind":"task","status":"planned","title":"Synthetic bounded item",
        "notes":null,"timezone_name":"UTC","duration_seconds":60,"deadline_at":null,
        "earliest_start_at":null,"recurrence":null,"flexible_constraints":{},
        "split_policy":{"type":"indivisible"},"importance":1,"urgency":1,"parent_id":null,"sibling_order":0})).unwrap();
    Item::new(input, Utc::now()).unwrap()
}

fn change(item: Item) -> DeltaChange {
    DeltaChange::Upsert {
        item: Box::new(item),
    }
}

#[test]
fn exact_member_bound_accepts_twenty_thousand_but_never_a_truncated_extra_item() {
    let template = item(1);
    let mut changes = (1..=20_000)
        .map(|id| {
            let mut value = template.clone();
            value.id = Uuid::from_u128(id);
            change(value)
        })
        .collect::<Vec<_>>();
    assert!(validate_members(&changes).is_ok());
    changes.push(change(item(20_001)));
    assert_eq!(
        validate_members(&changes),
        Err(ItemRepositoryError::BootstrapTooLarge)
    );
    assert_eq!(
        MemoryBootstrap::capture(
            changes.into_iter().map(|value| match value {
                DeltaChange::Upsert { item } => *item,
                DeltaChange::Tombstone { .. } => unreachable!(),
            }),
            9,
            Utc::now()
        )
        .unwrap_err(),
        ItemRepositoryError::BootstrapTooLarge
    );
}

#[test]
fn total_byte_and_page_byte_bounds_count_wire_payloads_before_claiming_completion() {
    let mut template = item(1);
    template.notes = Some("n".repeat(100_000));
    let changes = (1..=350)
        .map(|id| {
            let mut value = template.clone();
            value.id = Uuid::from_u128(id);
            change(value)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        validate_members(&changes),
        Err(ItemRepositoryError::BootstrapTooLarge)
    );
    let prefix = page_prefix(&changes).unwrap();
    assert!(prefix > 0 && prefix < 300);
    let bytes = changes[..prefix]
        .iter()
        .map(|value| serde_json::to_vec(value).unwrap().len())
        .sum::<usize>();
    assert!(bytes <= super::super::repository::MAX_ITEM_DELTA_PAGE_PAYLOAD_BYTES);
    assert!(
        bytes + serde_json::to_vec(&changes[prefix]).unwrap().len()
            > super::super::repository::MAX_ITEM_DELTA_PAGE_PAYLOAD_BYTES
    );
    let small = (1..=301).map(|id| change(item(id))).collect::<Vec<_>>();
    assert_eq!(page_prefix(&small).unwrap(), 300);
}

#[test]
fn malformed_duplicate_missing_and_cyclic_forests_are_never_complete_snapshots() {
    let root = item(1);
    let mut missing = item(2);
    missing.parent_id = Some(Uuid::from_u128(99));
    let mut self_parent = root.clone();
    self_parent.parent_id = Some(self_parent.id);
    let mut cyclic_root = root.clone();
    cyclic_root.parent_id = Some(Uuid::from_u128(2));
    let mut cyclic_child = item(2);
    cyclic_child.parent_id = Some(root.id);
    for changes in [
        vec![change(root.clone()), change(root.clone())],
        vec![change(missing)],
        vec![change(self_parent)],
        vec![change(cyclic_root), change(cyclic_child)],
    ] {
        assert_eq!(
            validate_members(&changes),
            Err(ItemRepositoryError::Internal)
        );
    }
    let mut nil = root;
    nil.id = Uuid::nil();
    assert_eq!(
        validate_members(&[change(nil)]),
        Err(ItemRepositoryError::Internal)
    );
}

#[test]
fn cutoff_is_fixed_at_capture_and_includes_every_retained_terminal_item() {
    let now = Utc::now();
    let mut boundary = item(2);
    boundary.deleted_at = Some(now - BOOTSTRAP_TRASH_RETENTION);
    let mut ancient = item(3);
    ancient.deleted_at = Some(now - BOOTSTRAP_TRASH_RETENTION - Duration::microseconds(1));
    let mut terminal = item(1);
    terminal.status = crate::items::ItemStatus::Cancelled;
    let captured =
        MemoryBootstrap::capture(vec![terminal, boundary, ancient].into_iter(), 99, now).unwrap();
    assert_eq!(captured.changes.len(), 2);
    assert!(matches!(captured.changes[1], DeltaChange::Tombstone { .. }));
    assert_eq!(captured.page(0).unwrap().head, 99);
    assert_eq!(
        captured.page(2),
        Err(ItemRepositoryError::BootstrapCursorInvalid)
    );
}
