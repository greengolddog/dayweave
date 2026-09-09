use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use super::{DatabaseScope, lock_canonical_item_space};
use crate::item_progress::{
    ItemProgressCommand, ItemProgressError, ItemProgressMutation, ItemProgressSnapshot,
};

pub(super) async fn get(
    pool: &PgPool,
    scope: DatabaseScope,
    item_id: Uuid,
) -> Result<ItemProgressSnapshot, ItemProgressError> {
    let mut tx = pool.begin().await.map_err(storage)?;
    admit(&mut tx, scope).await?;
    let snapshot = read(&mut tx, scope, item_id).await?;
    tx.commit().await.map_err(storage)?;
    Ok(snapshot)
}

pub(super) async fn put(
    pool: &PgPool,
    scope: DatabaseScope,
    item_id: Uuid,
    command: ItemProgressCommand,
    actor_session_id: Option<Uuid>,
) -> Result<ItemProgressMutation, ItemProgressError> {
    let mut tx = pool.begin().await.map_err(storage)?;
    admit(&mut tx, scope).await?;
    lock_canonical_item_space(&mut tx, scope.workspace_id)
        .await
        .map_err(storage)?;
    let request = serde_json::to_value(&command).map_err(|_| ItemProgressError::Unavailable)?;
    let receipt = sqlx::query(
        "SELECT item_id, request_json, result_json FROM item_progress_operations \
        WHERE workspace_id = $1 AND operation_id = $2",
    )
    .bind(scope.workspace_id)
    .bind(command.operation_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(storage)?;
    if let Some(receipt) = receipt {
        let stored_item: Uuid = receipt.try_get("item_id").map_err(storage)?;
        let stored: Value = receipt.try_get("request_json").map_err(storage)?;
        if stored_item != item_id || stored != request {
            return Err(ItemProgressError::OperationReused);
        }
        let result: Value = receipt.try_get("result_json").map_err(storage)?;
        let progress: ItemProgressSnapshot =
            serde_json::from_value(result).map_err(|_| ItemProgressError::Unavailable)?;
        tx.commit().await.map_err(storage)?;
        return Ok(ItemProgressMutation {
            operation_id: command.operation_id,
            replayed: true,
            progress,
        });
    }
    command.validate(item_id)?;
    // Canonical workspace serialization includes every item writer. Row share also
    // protects the exact item identity without modifying any canonical timestamp.
    let locked: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM items WHERE workspace_id = $1 AND id = $2 AND trashed_at IS NULL FOR SHARE",
    )
    .bind(scope.workspace_id)
    .bind(item_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(storage)?;
    if locked.is_none() {
        return Err(ItemProgressError::ItemMissing);
    }
    let before = read(&mut tx, scope, item_id).await?;
    let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
    let progress = before.replaced(&command, now)?;
    let components =
        serde_json::to_value(&progress.components).map_err(|_| ItemProgressError::Unavailable)?;
    let statement = if before.revision == 0 {
        "INSERT INTO item_progress (workspace_id,item_id,revision,components,updated_at) VALUES ($1,$2,$3,$4,$5)"
    } else {
        "UPDATE item_progress SET revision=$3,components=$4,updated_at=$5 WHERE workspace_id=$1 AND item_id=$2"
    };
    sqlx::query(statement)
        .bind(scope.workspace_id)
        .bind(item_id)
        .bind(revision(progress.revision)?)
        .bind(components)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
    sqlx::query("INSERT INTO item_progress_operations (workspace_id,operation_id,item_id,actor_user_id,actor_session_id, \
        progress_revision,request_json,before_json,result_json,recorded_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
        .bind(scope.workspace_id).bind(command.operation_id).bind(item_id).bind(scope.user_id).bind(actor_session_id)
        .bind(revision(progress.revision)?).bind(request)
        .bind(serde_json::to_value(&before).map_err(|_| ItemProgressError::Unavailable)?)
        .bind(serde_json::to_value(&progress).map_err(|_| ItemProgressError::Unavailable)?)
        .bind(now).execute(&mut *tx).await.map_err(storage)?;
    sqlx::query("INSERT INTO audit_operations (id,workspace_id,actor_user_id,operation_type,entity_type,entity_id,base_revision,result_revision,outcome,request_id,actor_session_id) \
        VALUES ($1,$2,$3,'item.progress_replaced','item_progress',$4,$5,$6,'succeeded',$7,$8)")
        .bind(Uuid::new_v4()).bind(scope.workspace_id).bind(scope.user_id).bind(item_id)
        .bind((before.revision != 0).then_some(revision(before.revision)?)).bind(revision(progress.revision)?)
        .bind(command.operation_id.to_string()).bind(actor_session_id).execute(&mut *tx).await.map_err(storage)?;
    tx.commit().await.map_err(storage)?;
    Ok(ItemProgressMutation {
        operation_id: command.operation_id,
        replayed: false,
        progress,
    })
}

async fn admit(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
) -> Result<(), ItemProgressError> {
    sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtextextended('dayweave.account-deletion.global-mutation-barrier.v1',0))")
        .execute(&mut **tx).await.map_err(storage)?;
    let fenced: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM account_deletion_fences WHERE workspace_id=$1 OR user_id=$2)",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(storage)?;
    if fenced {
        return Err(ItemProgressError::Unavailable);
    }
    let owner: Option<Uuid> = sqlx::query_scalar(
        "SELECT member.workspace_id FROM workspace_members member \
        JOIN workspaces workspace ON workspace.id=member.workspace_id \
        JOIN users owner ON owner.id=workspace.owner_user_id \
        WHERE member.workspace_id=$1 AND member.user_id=$2 AND workspace.owner_user_id=$2 \
        AND workspace.trashed_at IS NULL AND workspace.tombstoned_at IS NULL \
        AND owner.trashed_at IS NULL AND owner.tombstoned_at IS NULL \
        AND member.role='owner' AND member.removed_at IS NULL FOR SHARE",
    )
    .bind(scope.workspace_id)
    .bind(scope.user_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(storage)?;
    if owner.is_none() {
        return Err(ItemProgressError::Unavailable);
    }
    Ok(())
}

async fn read(
    tx: &mut Transaction<'_, Postgres>,
    scope: DatabaseScope,
    item_id: Uuid,
) -> Result<ItemProgressSnapshot, ItemProgressError> {
    let row = sqlx::query("SELECT item.revision AS item_revision,progress.revision,progress.components,progress.updated_at \
        FROM items item LEFT JOIN item_progress progress ON progress.workspace_id=item.workspace_id AND progress.item_id=item.id \
        WHERE item.workspace_id=$1 AND item.id=$2 AND item.trashed_at IS NULL")
        .bind(scope.workspace_id).bind(item_id).fetch_optional(&mut **tx).await.map_err(storage)?
        .ok_or(ItemProgressError::ItemMissing)?;
    let item_revision = u64::try_from(row.try_get::<i64, _>("item_revision").map_err(storage)?)
        .map_err(|_| ItemProgressError::Unavailable)?;
    let progress_revision: Option<i64> = row.try_get("revision").map_err(storage)?;
    let Some(progress_revision) = progress_revision else {
        return Ok(ItemProgressSnapshot::empty(item_id, item_revision));
    };
    let snapshot = ItemProgressSnapshot {
        schema_version: 1,
        item_id,
        item_revision,
        revision: u64::try_from(progress_revision).map_err(|_| ItemProgressError::Unavailable)?,
        components: serde_json::from_value(row.try_get("components").map_err(storage)?)
            .map_err(|_| ItemProgressError::Unavailable)?,
        updated_at: Some(row.try_get("updated_at").map_err(storage)?),
    };
    // Fail closed on unsupported stored content rather than returning trusted zeros.
    ItemProgressCommand {
        schema_version: 1,
        operation_id: item_id,
        expected_item_revision: item_revision,
        expected_progress_revision: snapshot.revision,
        components: snapshot.components.clone(),
    }
    .validate(item_id)
    .map_err(|_| ItemProgressError::Unavailable)?;
    Ok(snapshot)
}

fn revision(value: u64) -> Result<i64, ItemProgressError> {
    i64::try_from(value).map_err(|_| ItemProgressError::Unavailable)
}

fn storage(_: sqlx::Error) -> ItemProgressError {
    ItemProgressError::Unavailable
}
