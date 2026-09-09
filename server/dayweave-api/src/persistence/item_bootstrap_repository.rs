//! Durable bounded manifests pin exact append-only change rows, never a live OFFSET view.
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Postgres, QueryBuilder, Row, Transaction};
use uuid::Uuid;

use super::{DatabaseScope, lock_canonical_item_space};
use crate::items::{
    DeltaChange, ItemBootstrapPage, ItemBootstrapPosition, ItemRepositoryError,
    bootstrap::{
        BOOTSTRAP_TRASH_RETENTION, BOOTSTRAP_TTL, MAX_BOOTSTRAP_MEMBERS, MAX_BOOTSTRAP_TICKETS,
        page_prefix, validate_members,
    },
};

fn internal(_: impl std::fmt::Debug) -> ItemRepositoryError {
    ItemRepositoryError::Internal
}

pub(crate) async fn bootstrap(
    pool: &PgPool,
    scope: DatabaseScope,
    position: Option<ItemBootstrapPosition>,
) -> Result<ItemBootstrapPage, ItemRepositoryError> {
    let mut tx = pool.begin().await.map_err(internal)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .execute(&mut *tx)
        .await
        .map_err(internal)?;
    sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtextextended('dayweave.account-deletion.global-mutation-barrier.v1',0))")
        .execute(&mut *tx).await.map_err(internal)?;
    let permitted: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workspaces workspace \
        JOIN users owner ON owner.id=workspace.owner_user_id JOIN workspace_members member \
          ON member.workspace_id=workspace.id AND member.user_id=owner.id \
        WHERE workspace.id=$1 AND owner.id=$2 AND member.role='owner' AND member.removed_at IS NULL \
          AND workspace.trashed_at IS NULL AND workspace.tombstoned_at IS NULL \
          AND owner.trashed_at IS NULL AND owner.tombstoned_at IS NULL) \
        AND NOT EXISTS(SELECT 1 FROM account_deletion_fences WHERE workspace_id=$1 OR user_id=$2)")
        .bind(scope.workspace_id).bind(scope.user_id).fetch_one(&mut *tx).await.map_err(internal)?;
    if !permitted {
        return Err(ItemRepositoryError::BootstrapExpired);
    }
    let position = match position {
        Some(position) => position,
        None => capture(&mut tx, scope).await?,
    };
    let page = read_page(&mut tx, scope, position).await?;
    tx.commit().await.map_err(internal)?;
    Ok(page)
}

#[allow(clippy::too_many_lines)] // One ordered bounded capture keeps metadata admission before payload reads and commit sealing.
async fn capture(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<ItemBootstrapPosition, ItemRepositoryError> {
    lock_canonical_item_space(tx, scope.workspace_id)
        .await
        .map_err(internal)?;
    sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtextextended('dayweave.item-bootstrap.history-immutability.v1',0))")
        .execute(&mut **tx).await.map_err(internal)?;
    sqlx::query("DELETE FROM item_bootstrap_members AS member USING item_bootstrap_snapshots AS snapshot \
        WHERE member.snapshot_id=snapshot.id AND snapshot.workspace_id=$1 AND snapshot.user_id=$2 AND snapshot.expires_at<=clock_timestamp()")
        .bind(scope.workspace_id).bind(scope.user_id).execute(&mut **tx).await.map_err(internal)?;
    sqlx::query("DELETE FROM item_bootstrap_snapshots WHERE workspace_id=$1 AND user_id=$2 AND expires_at<=clock_timestamp()")
        .bind(scope.workspace_id).bind(scope.user_id).execute(&mut **tx).await.map_err(internal)?;
    let head: i64 = sqlx::query_scalar(
        "SELECT COALESCE(max(sequence),0) FROM item_changes WHERE workspace_id=$1",
    )
    .bind(scope.workspace_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(internal)?;
    if let Some(id) = sqlx::query_scalar::<_, Uuid>("SELECT id FROM item_bootstrap_snapshots \
        WHERE workspace_id=$1 AND user_id=$2 AND head_sequence=$3 AND expires_at>clock_timestamp() ORDER BY created_at DESC LIMIT 1")
        .bind(scope.workspace_id).bind(scope.user_id).bind(head).fetch_optional(&mut **tx).await.map_err(internal)? {
        return Ok(ItemBootstrapPosition { snapshot_id: id, after: 0 });
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM item_bootstrap_snapshots WHERE workspace_id=$1 AND expires_at>clock_timestamp()")
        .bind(scope.workspace_id).fetch_one(&mut **tx).await.map_err(internal)?;
    if count >= i64::try_from(MAX_BOOTSTRAP_TICKETS).map_err(internal)? {
        return Err(ItemRepositoryError::BootstrapCapacity);
    }
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await
        .map_err(internal)?;
    let cutoff = now - BOOTSTRAP_TRASH_RETENTION;
    let rows = sqlx::query(
        "SELECT item.id, change.sequence, \
        octet_length(change.payload::text) AS payload_bytes FROM items AS item \
        LEFT JOIN item_changes AS change ON change.workspace_id=item.workspace_id \
          AND change.item_id=item.id AND change.item_revision=item.revision \
        WHERE item.workspace_id=$1 AND (item.trashed_at IS NULL OR item.trashed_at >= $2) \
        ORDER BY item.id LIMIT $3",
    )
    .bind(scope.workspace_id)
    .bind(cutoff)
    .bind(i64::try_from(MAX_BOOTSTRAP_MEMBERS + 1).map_err(internal)?)
    .fetch_all(&mut **tx)
    .await
    .map_err(internal)?;
    if rows.len() > MAX_BOOTSTRAP_MEMBERS {
        return Err(ItemRepositoryError::BootstrapTooLarge);
    }
    let mut sequences = Vec::with_capacity(rows.len());
    let mut payload_bytes = 0_i64;
    for row in rows {
        let sequence: i64 = row.try_get("sequence").map_err(internal)?;
        if sequence > head {
            return Err(ItemRepositoryError::Internal);
        }
        sequences.push(sequence);
        payload_bytes = payload_bytes
            .checked_add(i64::from(
                row.try_get::<i32, _>("payload_bytes").map_err(internal)?,
            ))
            .ok_or(ItemRepositoryError::BootstrapTooLarge)?;
    }
    // PostgreSQL's spaced jsonb text upper-bounds compact payloads; 64 bytes per
    // row also covers the existing DeltaChange wrapper before loading any bodies.
    if payload_bytes + i64::try_from(sequences.len()).map_err(internal)? * 64 > 32 * 1024 * 1024 {
        return Err(ItemRepositoryError::BootstrapTooLarge);
    }
    let mut changes = Vec::with_capacity(sequences.len());
    for chunk in sequences.chunks(300) {
        let payloads = sqlx::query("SELECT item_id,item_revision,change_kind,payload FROM item_changes WHERE workspace_id=$1 AND sequence=ANY($2) ORDER BY sequence")
            .bind(scope.workspace_id).bind(chunk).fetch_all(&mut **tx).await.map_err(internal)?;
        if payloads.len() != chunk.len() {
            return Err(ItemRepositoryError::Internal);
        }
        for row in payloads {
            changes.push(decode(
                row.try_get("change_kind").map_err(internal)?,
                row.try_get("payload").map_err(internal)?,
                row.try_get("item_id").map_err(internal)?,
                row.try_get("item_revision").map_err(internal)?,
            )?);
        }
    }
    validate_members(&changes)?;
    sequences.sort_unstable();
    if payload_bytes > 32 * 1024 * 1024 {
        return Err(ItemRepositoryError::BootstrapTooLarge);
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO item_bootstrap_snapshots \
        (id,workspace_id,user_id,head_sequence,created_at,cutoff_at,expires_at,member_count,payload_bytes) \
        VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
        .bind(id).bind(scope.workspace_id).bind(scope.user_id).bind(head).bind(now).bind(cutoff)
        .bind(now + BOOTSTRAP_TTL).bind(i32::try_from(sequences.len()).map_err(internal)?).bind(payload_bytes)
        .execute(&mut **tx).await.map_err(internal)?;
    for (chunk_index, chunk) in sequences.chunks(1_000).enumerate() {
        let mut insert = QueryBuilder::new(
            "INSERT INTO item_bootstrap_members(snapshot_id,workspace_id,ordinal,change_sequence) ",
        );
        insert.push_values(
            chunk.iter().enumerate(),
            |mut values, (offset, sequence)| {
                values
                    .push_bind(id)
                    .push_bind(scope.workspace_id)
                    .push_bind(
                        i32::try_from(chunk_index * 1_000 + offset + 1)
                            .expect("bounded manifest ordinal"),
                    )
                    .push_bind(*sequence);
            },
        );
        insert.build().execute(&mut **tx).await.map_err(internal)?;
    }
    Ok(ItemBootstrapPosition {
        snapshot_id: id,
        after: 0,
    })
}

async fn read_page(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    position: ItemBootstrapPosition,
) -> Result<ItemBootstrapPage, ItemRepositoryError> {
    let header = sqlx::query(
        "SELECT head_sequence,member_count,expires_at FROM item_bootstrap_snapshots \
        WHERE id=$1 AND workspace_id=$2 AND user_id=$3 FOR SHARE",
    )
    .bind(position.snapshot_id)
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(internal)?
    .ok_or(ItemRepositoryError::BootstrapExpired)?;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await
        .map_err(internal)?;
    if header
        .try_get::<DateTime<Utc>, _>("expires_at")
        .map_err(internal)?
        <= now
    {
        return Err(ItemRepositoryError::BootstrapExpired);
    }
    let total = usize::try_from(header.try_get::<i32, _>("member_count").map_err(internal)?)
        .map_err(internal)?;
    if position.after > total || (position.after == total && total != 0) {
        return Err(ItemRepositoryError::BootstrapCursorInvalid);
    }
    let rows = sqlx::query("SELECT member.ordinal,change.item_id,change.item_revision,change.change_kind,change.payload FROM item_bootstrap_members AS member \
        JOIN item_changes AS change ON change.sequence=member.change_sequence AND change.workspace_id=member.workspace_id \
        WHERE member.snapshot_id=$1 AND member.workspace_id=$2 AND member.ordinal>$3 ORDER BY member.ordinal LIMIT 300")
        .bind(position.snapshot_id).bind(scope.workspace_id).bind(i32::try_from(position.after).map_err(internal)?)
        .fetch_all(&mut **tx).await.map_err(internal)?;
    let mut changes = Vec::with_capacity(rows.len());
    for (index, row) in rows.into_iter().enumerate() {
        if usize::try_from(row.try_get::<i32, _>("ordinal").map_err(internal)?).map_err(internal)?
            != position.after + index + 1
        {
            return Err(ItemRepositoryError::Internal);
        }
        changes.push(decode(
            row.try_get("change_kind").map_err(internal)?,
            row.try_get("payload").map_err(internal)?,
            row.try_get("item_id").map_err(internal)?,
            row.try_get("item_revision").map_err(internal)?,
        )?);
    }
    if changes.len() != (total - position.after).min(300) {
        return Err(ItemRepositoryError::Internal);
    }
    let count = page_prefix(&changes)?;
    changes.truncate(count);
    let after = position.after + count;
    Ok(ItemBootstrapPage {
        changes,
        head: u64::try_from(
            header
                .try_get::<i64, _>("head_sequence")
                .map_err(internal)?,
        )
        .map_err(internal)?,
        continuation: (after < total).then_some(ItemBootstrapPosition { after, ..position }),
    })
}

fn decode(
    kind: &str,
    payload: Value,
    id: Uuid,
    revision: i64,
) -> Result<DeltaChange, ItemRepositoryError> {
    let change = match kind {
        "upsert" => Ok(DeltaChange::Upsert {
            item: Box::new(serde_json::from_value(payload).map_err(internal)?),
        }),
        "tombstone" => Ok(DeltaChange::Tombstone {
            tombstone: serde_json::from_value(payload).map_err(internal)?,
        }),
        _ => Err(ItemRepositoryError::Internal),
    }?;
    let revision = u64::try_from(revision).map_err(internal)?;
    let valid = match &change {
        DeltaChange::Upsert { item } => {
            item.id == id && item.revision == revision && item.deleted_at.is_none()
        }
        DeltaChange::Tombstone { tombstone } => {
            tombstone.id == id && tombstone.revision == revision
        }
    };
    if !valid || revision == 0 || id.is_nil() {
        return Err(ItemRepositoryError::Internal);
    }
    Ok(change)
}
