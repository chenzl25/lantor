use serde_json::json;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::agent_inbox_wake::enqueue_agent_work_if_available;
use crate::app::{to_string, CommandResult};
use crate::context_tool::compact_message_body;
use crate::events::activity::record_agent_activity;
use crate::message_store::load_message_patch_in_tx;
use crate::runtime::lifecycle::mark_work_item_held_in_tx;
use crate::ui_notifications::{
    enqueue_ui_event_in_tx, enqueue_ui_work_item_changed_in_tx, reconcile_work_item_change, UiEvent,
};

const FRESHNESS_RETRY_CONTEXT_LIMIT: i64 = 16;
const FRESHNESS_RETRY_BODY_LIMIT: usize = 900;
const FRESHNESS_RETRY_DRAFT_LIMIT: usize = 4000;
const FRESHNESS_RETRY_ORIGINAL_CONTEXT_LIMIT: usize = 5000;
// With N agents racing on one surface, each hold round publishes exactly one
// winner, so convergence needs at most ~N generations. Keep the cap above the
// realistic concurrent-writer count; it only exists as a runaway backstop.
const FRESHNESS_RETRY_MAX_GENERATIONS: i64 = 8;

#[derive(Clone, Debug)]
pub(crate) struct WorkItemFreshnessScope {
    pub(crate) channel_id: Uuid,
    pub(crate) thread_root_id: Option<Uuid>,
    pub(crate) seen_seq: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct StaleOutput {
    pub(crate) latest_message_id: Uuid,
    pub(crate) latest_seq: i64,
    pub(crate) seen_seq: i64,
}

enum FreshnessRetryDisposition {
    Requeued(Uuid),
    Suppressed,
}

struct FreshnessRetrySource {
    work_item_id: Uuid,
    source_message_id: Option<Uuid>,
    task_id: Option<Uuid>,
    title: String,
    context: String,
}

pub(crate) async fn advance_agent_target_watermark(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Option<Uuid>,
    thread_root_id: Option<Uuid>,
    last_seen_seq: i64,
) -> CommandResult<()> {
    let Some(channel_id) = channel_id else {
        return Ok(());
    };
    if last_seen_seq <= 0 {
        return Ok(());
    }

    if let Some(thread_root_id) = thread_root_id {
        sqlx::query(
            r#"
            insert into agent_target_watermarks (
                agent_id, channel_id, thread_root_id, last_seen_seq
            )
            values ($1, $2, $3, $4)
            on conflict(agent_id, channel_id, thread_root_id) where thread_root_id is not null
            do update set
                last_seen_seq = max(agent_target_watermarks.last_seen_seq, excluded.last_seen_seq),
                updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .bind(thread_root_id)
        .bind(last_seen_seq)
        .execute(pool)
        .await
        .map_err(to_string)?;
    } else {
        sqlx::query(
            r#"
            insert into agent_target_watermarks (
                agent_id, channel_id, thread_root_id, last_seen_seq
            )
            values ($1, $2, null, $3)
            on conflict(agent_id, channel_id) where thread_root_id is null
            do update set
                last_seen_seq = max(agent_target_watermarks.last_seen_seq, excluded.last_seen_seq),
                updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .bind(last_seen_seq)
        .execute(pool)
        .await
        .map_err(to_string)?;
    }

    Ok(())
}

pub(crate) async fn advance_agent_target_watermark_for_work_item(
    pool: &SqlitePool,
    agent_id: Uuid,
    work_item_id: Uuid,
    extra_seen_seq: i64,
) -> CommandResult<()> {
    let row = sqlx::query(
        r#"
        select channel_id, thread_root_id, context_max_seq
        from agent_work_items
        where id = $1 and agent_id = $2
        "#,
    )
    .bind(work_item_id)
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    let Some(row) = row else {
        return Ok(());
    };
    let channel_id: Option<Uuid> = row.get("channel_id");
    let thread_root_id: Option<Uuid> = row.get("thread_root_id");
    let context_max_seq: i64 = row.get("context_max_seq");
    advance_agent_target_watermark(
        pool,
        agent_id,
        channel_id,
        thread_root_id,
        context_max_seq.max(extra_seen_seq),
    )
    .await
}

pub(crate) async fn load_work_item_freshness_scope(
    pool: &SqlitePool,
    agent_id: Uuid,
    work_item_id: Uuid,
) -> CommandResult<Option<WorkItemFreshnessScope>> {
    let Some(row) = sqlx::query(
        r#"
        select channel_id, thread_root_id, context_max_seq
        from agent_work_items
        where id = $1 and agent_id = $2
        "#,
    )
    .bind(work_item_id)
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?
    else {
        return Ok(None);
    };
    let Some(channel_id) = row.get::<Option<Uuid>, _>("channel_id") else {
        return Ok(None);
    };
    let thread_root_id: Option<Uuid> = row.get("thread_root_id");
    let context_max_seq: i64 = row.get("context_max_seq");
    let watermark_seq = load_agent_target_watermark(pool, agent_id, channel_id, thread_root_id)
        .await?
        .unwrap_or(0);
    let seen_seq = context_max_seq.max(watermark_seq);
    if seen_seq <= 0 {
        return Ok(None);
    }

    Ok(Some(WorkItemFreshnessScope {
        channel_id,
        thread_root_id,
        seen_seq,
    }))
}

pub(crate) async fn stale_output_for_scope(
    pool: &SqlitePool,
    agent_id: Uuid,
    scope: &WorkItemFreshnessScope,
) -> CommandResult<Option<StaleOutput>> {
    let row = if let Some(thread_root_id) = scope.thread_root_id {
        sqlx::query(
            r#"
            select id, seq
            from messages
            where channel_id = $1
              and (id = $2 or thread_root_id = $2)
              and seq > $3
              and delivery_state = 'complete'
              and length(trim(body)) > 0
              and (sender_role = 'owner' or sender_agent_id is not null)
              and (sender_agent_id is null or sender_agent_id <> $4)
            order by seq desc
            limit 1
            "#,
        )
        .bind(scope.channel_id)
        .bind(thread_root_id)
        .bind(scope.seen_seq)
        .bind(agent_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?
    } else {
        sqlx::query(
            r#"
            select id, seq
            from messages
            where channel_id = $1
              and thread_root_id is null
              and seq > $2
              and delivery_state = 'complete'
              and length(trim(body)) > 0
              and (sender_role = 'owner' or sender_agent_id is not null)
              and (sender_agent_id is null or sender_agent_id <> $3)
            order by seq desc
            limit 1
            "#,
        )
        .bind(scope.channel_id)
        .bind(scope.seen_seq)
        .bind(agent_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?
    };

    Ok(row.map(|row| StaleOutput {
        latest_message_id: row.get("id"),
        latest_seq: row.get("seq"),
        seen_seq: scope.seen_seq,
    }))
}

pub(crate) async fn stale_output_for_work_item(
    pool: &SqlitePool,
    agent_id: Uuid,
    work_item_id: Uuid,
) -> CommandResult<Option<StaleOutput>> {
    let Some(scope) = load_work_item_freshness_scope(pool, agent_id, work_item_id).await? else {
        return Ok(None);
    };
    stale_output_for_scope(pool, agent_id, &scope).await
}

pub(crate) async fn try_complete_streaming_message_if_fresh(
    pool: &SqlitePool,
    agent_id: Uuid,
    work_item_id: Uuid,
    stream_key: &str,
) -> CommandResult<Option<Uuid>> {
    let Some(scope) = load_work_item_freshness_scope(pool, agent_id, work_item_id).await? else {
        return Ok(None);
    };
    let mut transaction = pool.begin().await.map_err(to_string)?;
    let completed = if let Some(thread_root_id) = scope.thread_root_id {
        sqlx::query_scalar(
            r#"
            update messages as current
            set delivery_state = 'complete'
            where current.stream_key = $1
              and current.delivery_state = 'streaming'
              and not exists (
                  select 1
                  from messages newer
                  where newer.channel_id = current.channel_id
                    and (newer.id = $2 or newer.thread_root_id = $2)
                    and newer.seq > $3
                    and newer.delivery_state = 'complete'
                    and length(trim(newer.body)) > 0
                    and (newer.sender_role = 'owner' or newer.sender_agent_id is not null)
                    and (newer.sender_agent_id is null or newer.sender_agent_id <> $4)
              )
            returning id
            "#,
        )
        .bind(stream_key)
        .bind(thread_root_id)
        .bind(scope.seen_seq)
        .bind(agent_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(to_string)?
    } else {
        sqlx::query_scalar(
            r#"
            update messages as current
            set delivery_state = 'complete'
            where current.stream_key = $1
              and current.delivery_state = 'streaming'
              and not exists (
                  select 1
                  from messages newer
                  where newer.channel_id = current.channel_id
                    and newer.thread_root_id is null
                    and newer.seq > $2
                    and newer.delivery_state = 'complete'
                    and length(trim(newer.body)) > 0
                    and (newer.sender_role = 'owner' or newer.sender_agent_id is not null)
                    and (newer.sender_agent_id is null or newer.sender_agent_id <> $3)
              )
            returning id
            "#,
        )
        .bind(stream_key)
        .bind(scope.seen_seq)
        .bind(agent_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(to_string)?
    };

    if let Some(message_id) = completed {
        let message = load_message_patch_in_tx(&mut transaction, message_id).await?;
        enqueue_ui_event_in_tx(
            &mut transaction,
            &UiEvent::MessageUpsert {
                reason: "stream_finish",
                message: &message,
            },
        )
        .await?;
    }
    transaction.commit().await.map_err(to_string)?;
    Ok(completed)
}

pub(crate) async fn hold_work_item_output_if_stale(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    work_item_id: Uuid,
    output_kind: &str,
    body: &str,
) -> CommandResult<bool> {
    let body = body.trim();
    if body.is_empty() {
        return Ok(false);
    }
    expire_old_held_outputs(pool).await?;

    let Some(scope) = load_work_item_freshness_scope(pool, agent_id, work_item_id).await? else {
        return Ok(false);
    };
    let Some(stale) = stale_output_for_scope(pool, agent_id, &scope).await? else {
        return Ok(false);
    };
    let mut transaction = pool.begin().await.map_err(to_string)?;
    let held_output_id: Uuid = sqlx::query_scalar(
        r#"
        insert into agent_held_outputs (
            agent_id, run_id, work_item_id, channel_id, thread_root_id,
            output_kind, body, seen_seq, latest_seq, latest_message_id, state, reason
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'held', $11)
        returning id
        "#,
    )
    .bind(agent_id)
    .bind(run_id)
    .bind(work_item_id)
    .bind(scope.channel_id)
    .bind(scope.thread_root_id)
    .bind(output_kind.trim())
    .bind(body)
    .bind(stale.seen_seq)
    .bind(stale.latest_seq)
    .bind(stale.latest_message_id)
    .bind("newer completed messages arrived before publish")
    .fetch_one(&mut *transaction)
    .await
    .map_err(to_string)?;

    mark_work_item_held_in_tx(&mut transaction, work_item_id).await?;
    transaction.commit().await.map_err(to_string)?;
    reconcile_work_item_change(pool, work_item_id, "work_item_held").await?;

    let retry_disposition =
        queue_freshness_retry_work_item(pool, agent_id, work_item_id, held_output_id, body, &stale)
            .await?;
    let retry_work_item_id = match retry_disposition {
        FreshnessRetryDisposition::Requeued(retry_work_item_id) => {
            sqlx::query(
                r#"
                update agent_held_outputs
                set state = 'requeued',
                    retry_work_item_id = $2,
                    resolved_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
                where id = $1
                "#,
            )
            .bind(held_output_id)
            .bind(retry_work_item_id)
            .execute(pool)
            .await
            .map_err(to_string)?;
            Some(retry_work_item_id)
        }
        FreshnessRetryDisposition::Suppressed => {
            sqlx::query(
                r#"
                update agent_held_outputs
                set state = 'suppressed',
                    reason = 'freshness retry generation limit reached',
                    resolved_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
                where id = $1
                "#,
            )
            .bind(held_output_id)
            .execute(pool)
            .await
            .map_err(to_string)?;
            None
        }
    };

    record_agent_activity(
        pool,
        Some(agent_id),
        Some(run_id),
        "dispatch",
        "Reply held for newer context",
        json!({
            "work_item_id": work_item_id,
            "held_output_id": held_output_id,
            "retry_work_item_id": retry_work_item_id,
            "seen_seq": stale.seen_seq,
            "latest_seq": stale.latest_seq,
            "latest_message_id": stale.latest_message_id,
            "output_kind": output_kind.trim(),
        })
        .to_string(),
    )
    .await?;

    Ok(true)
}

pub(crate) async fn hold_run_event_for_target_if_stale(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    target_channel_id: Uuid,
    target_thread_root_id: Option<Uuid>,
    output_kind: &str,
    body: &str,
) -> CommandResult<bool> {
    let work_item_id: Option<Uuid> =
        sqlx::query_scalar("select work_item_id from agent_runs where id = $1 and agent_id = $2")
            .bind(run_id)
            .bind(agent_id)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?
            .flatten();
    let Some(work_item_id) = work_item_id else {
        return Ok(false);
    };
    let Some(row) = sqlx::query(
        r#"
        select channel_id, thread_root_id
        from agent_work_items
        where id = $1 and agent_id = $2
        "#,
    )
    .bind(work_item_id)
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?
    else {
        return Ok(false);
    };
    if row.get::<Option<Uuid>, _>("channel_id") != Some(target_channel_id)
        || row.get::<Option<Uuid>, _>("thread_root_id") != target_thread_root_id
    {
        return Ok(false);
    }
    hold_work_item_output_if_stale(pool, agent_id, run_id, work_item_id, output_kind, body).await
}

async fn load_agent_target_watermark(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
) -> CommandResult<Option<i64>> {
    if let Some(thread_root_id) = thread_root_id {
        sqlx::query_scalar(
            r#"
            select last_seen_seq
            from agent_target_watermarks
            where agent_id = $1 and channel_id = $2 and thread_root_id = $3
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .bind(thread_root_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)
    } else {
        sqlx::query_scalar(
            r#"
            select last_seen_seq
            from agent_target_watermarks
            where agent_id = $1 and channel_id = $2 and thread_root_id is null
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)
    }
}

async fn expire_old_held_outputs(pool: &SqlitePool) -> CommandResult<()> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    let expired = sqlx::query(
        r#"
        update agent_held_outputs
        set state = 'expired',
            reason = case
                when reason = '' then 'expired after 60 minutes'
                else reason
            end,
            resolved_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where state = 'held'
          and expires_at <= strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        "#,
    )
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?
    .rows_affected();
    if expired > 0 {
        enqueue_ui_event_in_tx(
            &mut transaction,
            &UiEvent::Refresh {
                reason: "held_outputs_expired",
            },
        )
        .await?;
    }
    transaction.commit().await.map_err(to_string)?;
    Ok(())
}

async fn queue_freshness_retry_work_item(
    pool: &SqlitePool,
    agent_id: Uuid,
    work_item_id: Uuid,
    held_output_id: Uuid,
    held_body: &str,
    stale: &StaleOutput,
) -> CommandResult<FreshnessRetryDisposition> {
    if let Some(existing_retry_work_item_id) =
        load_existing_pending_freshness_retry(pool, work_item_id).await?
    {
        return Ok(FreshnessRetryDisposition::Requeued(
            existing_retry_work_item_id,
        ));
    }

    let Some(row) = sqlx::query(
        r#"
        select
            w.channel_id,
            c.name as channel_name,
            w.thread_root_id,
            w.freshness_generation
        from agent_work_items w
        left join channels c on c.id = w.channel_id
        where w.id = $1 and w.agent_id = $2
        "#,
    )
    .bind(work_item_id)
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?
    else {
        return Ok(FreshnessRetryDisposition::Suppressed);
    };
    let Some(channel_id) = row.get::<Option<Uuid>, _>("channel_id") else {
        return Ok(FreshnessRetryDisposition::Suppressed);
    };
    let thread_root_id: Option<Uuid> = row.get("thread_root_id");
    let channel_name: Option<String> = row.get("channel_name");
    let current_generation: i64 = row.get("freshness_generation");
    if current_generation >= FRESHNESS_RETRY_MAX_GENERATIONS {
        return Ok(FreshnessRetryDisposition::Suppressed);
    }
    let Some(source) = load_freshness_retry_source(pool, agent_id, work_item_id).await? else {
        return Ok(FreshnessRetryDisposition::Suppressed);
    };
    let rendered = render_surface_context_delta(
        pool,
        channel_id,
        channel_name.as_deref(),
        thread_root_id,
        stale.seen_seq,
    )
    .await?;
    let context_max_seq = stale.latest_seq.max(rendered.max_seq);
    let context = freshness_retry_context(
        source.work_item_id,
        work_item_id,
        held_output_id,
        channel_name.as_deref(),
        thread_root_id,
        stale,
        held_body,
        &rendered.body,
        &source.context,
    );
    let retry_title = format!("Refresh held reply: {}", source.title);
    let mut transaction = pool.begin().await.map_err(to_string)?;
    let retry_work_item_id: Uuid = sqlx::query_scalar(
        r#"
        insert into agent_work_items (
            agent_id, channel_id, thread_root_id, source_message_id, task_id,
            source_kind, title, context, context_max_seq, freshness_generation, status
        )
        values ($1, $2, $3, $4, $5, 'freshness_retry', $6, $7, $8, $9, 'queued')
        returning id
        "#,
    )
    .bind(agent_id)
    .bind(channel_id)
    .bind(thread_root_id)
    .bind(source.source_message_id)
    .bind(source.task_id)
    .bind(retry_title)
    .bind(context)
    .bind(context_max_seq)
    .bind(current_generation + 1)
    .fetch_one(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_work_item_changed_in_tx(&mut transaction, retry_work_item_id, "work_item_created")
        .await?;
    transaction.commit().await.map_err(to_string)?;
    reconcile_work_item_change(pool, retry_work_item_id, "work_item_created").await?;
    let scheduled = enqueue_agent_work_if_available(pool, agent_id, retry_work_item_id).await?;
    record_agent_activity(
        pool,
        Some(agent_id),
        None,
        "dispatch",
        if scheduled {
            "Freshness retry dispatched"
        } else {
            "Freshness retry queued"
        },
        json!({
            "held_output_id": held_output_id,
            "old_work_item_id": work_item_id,
            "retry_work_item_id": retry_work_item_id,
        })
        .to_string(),
    )
    .await?;
    Ok(FreshnessRetryDisposition::Requeued(retry_work_item_id))
}

async fn load_freshness_retry_source(
    pool: &SqlitePool,
    agent_id: Uuid,
    work_item_id: Uuid,
) -> CommandResult<Option<FreshnessRetrySource>> {
    let row = sqlx::query(
        r#"
        with recursive ancestry(work_item_id) as (
            select $1
            union
            select held.work_item_id
            from agent_held_outputs held
            join ancestry on held.retry_work_item_id = ancestry.work_item_id
            where held.work_item_id is not null
        )
        select
            source.id,
            source.source_message_id,
            source.task_id,
            source.title,
            source.context
        from ancestry
        join agent_work_items source on source.id = ancestry.work_item_id
        where source.agent_id = $2
        order by source.freshness_generation asc
        limit 1
        "#,
    )
    .bind(work_item_id)
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;

    Ok(row.map(|row| FreshnessRetrySource {
        work_item_id: row.get("id"),
        source_message_id: row.get("source_message_id"),
        task_id: row.get("task_id"),
        title: row.get("title"),
        context: row.get("context"),
    }))
}

async fn load_existing_pending_freshness_retry(
    pool: &SqlitePool,
    work_item_id: Uuid,
) -> CommandResult<Option<Uuid>> {
    sqlx::query_scalar(
        r#"
        select h.retry_work_item_id
        from agent_held_outputs h
        join agent_work_items retry on retry.id = h.retry_work_item_id
        where h.work_item_id = $1
          and h.retry_work_item_id is not null
          and retry.status in ('queued', 'running', 'cancelling')
        order by h.created_at desc
        limit 1
        "#,
    )
    .bind(work_item_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)
}

struct RenderedSurfaceContext {
    body: String,
    max_seq: i64,
}

async fn render_surface_context_delta(
    pool: &SqlitePool,
    channel_id: Uuid,
    channel_name: Option<&str>,
    thread_root_id: Option<Uuid>,
    after_seq: i64,
) -> CommandResult<RenderedSurfaceContext> {
    let rows = if let Some(thread_root_id) = thread_root_id {
        sqlx::query(
            r#"
            select id, sender_name, sender_role, body, seq, created_at
            from messages
            where channel_id = $1
              and (id = $2 or thread_root_id = $2)
              and seq > $3
              and delivery_state = 'complete'
              and length(trim(body)) > 0
            order by seq desc
            limit $4
            "#,
        )
        .bind(channel_id)
        .bind(thread_root_id)
        .bind(after_seq)
        .bind(FRESHNESS_RETRY_CONTEXT_LIMIT)
        .fetch_all(pool)
        .await
        .map_err(to_string)?
    } else {
        sqlx::query(
            r#"
            select id, sender_name, sender_role, body, seq, created_at
            from messages
            where channel_id = $1
              and thread_root_id is null
              and seq > $2
              and delivery_state = 'complete'
              and length(trim(body)) > 0
            order by seq desc
            limit $3
            "#,
        )
        .bind(channel_id)
        .bind(after_seq)
        .bind(FRESHNESS_RETRY_CONTEXT_LIMIT)
        .fetch_all(pool)
        .await
        .map_err(to_string)?
    };
    let max_seq = rows
        .iter()
        .map(|row| row.get::<i64, _>("seq"))
        .max()
        .unwrap_or(0);
    let target = surface_target(channel_name, thread_root_id);
    let lines = rows
        .into_iter()
        .rev()
        .map(|row| {
            let message_id: Uuid = row.get("id");
            let created_at: String = row.get("created_at");
            let sender_role: String = row.get("sender_role");
            let sender_name: String = row.get("sender_name");
            let body: String = row.get("body");
            format!(
                "[target={target} msg={} time={} type={}] {}: {}",
                short_id(message_id),
                created_at,
                sender_role,
                sender_name,
                compact_message_body(&body, FRESHNESS_RETRY_BODY_LIMIT, message_id)
                    .replace('\n', " ")
            )
        })
        .collect::<Vec<_>>();
    Ok(RenderedSurfaceContext {
        body: lines.join("\n"),
        max_seq,
    })
}

#[allow(clippy::too_many_arguments)]
fn freshness_retry_context(
    source_work_item_id: Uuid,
    previous_work_item_id: Uuid,
    held_output_id: Uuid,
    channel_name: Option<&str>,
    thread_root_id: Option<Uuid>,
    stale: &StaleOutput,
    held_body: &str,
    recent_context: &str,
    source_context: &str,
) -> String {
    let target = surface_target(channel_name, thread_root_id);
    let mut lines = vec![
        "Freshness retry for held agent output.".to_owned(),
        "Lantor held your previous visible output because newer complete messages arrived on the same target after your prompt context was rendered.".to_owned(),
        "Do not repost the held draft blindly. Use the recent thread context below and produce a new response only if it is still appropriate; otherwise reply silently.".to_owned(),
        format!("Default reply target for normal assistant text: {target}"),
        format!("source_work_item_id: {source_work_item_id}"),
        format!("previous_work_item_id: {previous_work_item_id}"),
        format!("held_output_id: {held_output_id}"),
        format!("seen_seq: {}", stale.seen_seq),
        format!("latest_seq: {}", stale.latest_seq),
        format!("latest_message_id: {}", stale.latest_message_id),
        String::new(),
        "Held draft:".to_owned(),
        compact_chars_middle(held_body, FRESHNESS_RETRY_DRAFT_LIMIT),
    ];
    if !recent_context.trim().is_empty() {
        lines.extend([
            String::new(),
            "Recent same-target context (oldest to newest):".to_owned(),
            recent_context.trim().to_owned(),
        ]);
    }
    if !source_context.trim().is_empty() {
        lines.extend([
            String::new(),
            "Source work-item context excerpt (generation 0):".to_owned(),
            compact_chars_middle(source_context, FRESHNESS_RETRY_ORIGINAL_CONTEXT_LIMIT),
        ]);
    }
    lines.join("\n")
}

fn surface_target(channel_name: Option<&str>, thread_root_id: Option<Uuid>) -> String {
    let channel = channel_name
        .filter(|name| !name.trim().is_empty())
        .map(|name| format!("#{name}"))
        .unwrap_or_else(|| "#channel".to_owned());
    if let Some(thread_root_id) = thread_root_id {
        format!("{channel}:{}", short_id(thread_root_id))
    } else {
        channel
    }
}

fn short_id(id: Uuid) -> String {
    id.to_string().chars().take(8).collect()
}

fn compact_chars_middle(value: &str, limit: usize) -> String {
    let count = value.chars().count();
    if count <= limit {
        return value.to_owned();
    }
    if limit <= 20 {
        return value.chars().take(limit).collect();
    }
    let head = (limit - 5) / 2;
    let tail = limit - 5 - head;
    let prefix: String = value.chars().take(head).collect();
    let suffix: String = value.chars().skip(count - tail).collect();
    format!("{prefix} ... {suffix}")
}

#[cfg(test)]
mod tests {
    use super::{
        advance_agent_target_watermark, advance_agent_target_watermark_for_work_item,
        hold_work_item_output_if_stale, stale_output_for_work_item,
    };
    use crate::test_support::{
        drop_test_schema, insert_test_agent, insert_test_channel, test_pool,
    };
    use sqlx::Row;
    use uuid::Uuid;

    #[tokio::test]
    async fn watermark_only_moves_forward() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "watermark-agent").await?;
            let channel_id = insert_test_channel(&pool, "watermark-channel").await?;

            advance_agent_target_watermark(&pool, agent_id, Some(channel_id), None, 10).await?;
            advance_agent_target_watermark(&pool, agent_id, Some(channel_id), None, 4).await?;

            let seen: i64 = sqlx::query_scalar(
                r#"
                select last_seen_seq
                from agent_target_watermarks
                where agent_id = $1 and channel_id = $2 and thread_root_id is null
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(seen, 10);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn work_item_watermark_uses_rendered_context_max_seq() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "work-item-watermark-agent").await?;
            let channel_id = insert_test_channel(&pool, "work-item-watermark-channel").await?;
            let thread_root_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'thread root', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let work_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_work_items (
                    agent_id, channel_id, thread_root_id, source_kind, title, context,
                    context_max_seq, status
                )
                values ($1, $2, $3, 'mention', 'test work item', 'context', 7, 'queued')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .bind(thread_root_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            advance_agent_target_watermark_for_work_item(&pool, agent_id, work_item_id, 3).await?;
            advance_agent_target_watermark_for_work_item(&pool, agent_id, work_item_id, 12).await?;

            let seen: i64 = sqlx::query_scalar(
                r#"
                select last_seen_seq
                from agent_target_watermarks
                where agent_id = $1 and channel_id = $2 and thread_root_id = $3
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .bind(thread_root_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(seen, 12);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn stale_output_detects_new_non_self_thread_messages() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "fresh-agent").await?;
            let other_agent_id = insert_test_agent(&pool, "other-fresh-agent").await?;
            let channel_id = insert_test_channel(&pool, "fresh-channel").await?;
            let thread_root_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'count from zero', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let root_seq: i64 = sqlx::query_scalar("select seq from messages where id = $1")
                .bind(thread_root_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            let work_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_work_items (
                    agent_id, channel_id, thread_root_id, source_message_id,
                    source_kind, title, context, context_max_seq, status
                )
                values ($1, $2, $3, $3, 'inbox_wake', 'count', 'context', $4, 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .bind(thread_root_id)
            .bind(root_seq)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            sqlx::query(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_agent_id, sender_name, sender_role, body
                )
                values ($1, $2, $3, 'fresh-agent', 'agent', 'self update')
                "#,
            )
            .bind(channel_id)
            .bind(thread_root_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert!(stale_output_for_work_item(&pool, agent_id, work_item_id)
                .await?
                .is_none());

            let other_message_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_agent_id, sender_name, sender_role, body
                )
                values ($1, $2, $3, 'other-fresh-agent', 'agent', '1')
                returning id
                "#,
            )
            .bind(channel_id)
            .bind(thread_root_id)
            .bind(other_agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let other_seq: i64 = sqlx::query_scalar("select seq from messages where id = $1")
                .bind(other_message_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            let stale = stale_output_for_work_item(&pool, agent_id, work_item_id)
                .await?
                .expect("other completed message should make output stale");
            assert_eq!(stale.latest_message_id, other_message_id);
            assert_eq!(stale.latest_seq, other_seq);

            advance_agent_target_watermark(
                &pool,
                agent_id,
                Some(channel_id),
                Some(thread_root_id),
                other_seq,
            )
            .await?;
            assert!(stale_output_for_work_item(&pool, agent_id, work_item_id)
                .await?
                .is_none());
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn holding_stale_output_records_draft_and_queues_retry() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let agent_id = insert_test_agent(&pool, "hold-agent").await?;
            let other_agent_id = insert_test_agent(&pool, "hold-other-agent").await?;
            let channel_id = insert_test_channel(&pool, "hold-channel").await?;
            let thread_root_id: Uuid = sqlx::query_scalar(
                r#"
                insert into messages (channel_id, sender_name, sender_role, body, is_task)
                values ($1, 'Dylan', 'owner', 'count from zero', false)
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let root_seq: i64 = sqlx::query_scalar("select seq from messages where id = $1")
                .bind(thread_root_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
            let work_item_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_work_items (
                    agent_id, channel_id, thread_root_id, source_message_id,
                    source_kind, title, context, context_max_seq, status
                )
                values ($1, $2, $3, $3, 'inbox_wake', 'count', 'original context', $4, 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(channel_id)
            .bind(thread_root_id)
            .bind(root_seq)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, work_item_id, command, status)
                values ($1, $2, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_agent_id, sender_name, sender_role, body
                )
                values ($1, $2, $3, 'hold-other-agent', 'agent', '1')
                "#,
            )
            .bind(channel_id)
            .bind(thread_root_id)
            .bind(other_agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let held = hold_work_item_output_if_stale(
                &pool,
                agent_id,
                run_id,
                work_item_id,
                "visible_reply",
                "1",
            )
            .await?;
            assert!(held, "stale reply should be held");

            let duplicate_held = hold_work_item_output_if_stale(
                &pool,
                agent_id,
                run_id,
                work_item_id,
                "channel_message_create",
                "duplicate stale output",
            )
            .await?;
            assert!(duplicate_held, "duplicate stale output should be held");
            let retry_count: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from agent_work_items
                where source_kind = 'freshness_retry'
                "#,
            )
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(retry_count, 1);

            let old_status: String =
                sqlx::query_scalar("select status from agent_work_items where id = $1")
                    .bind(work_item_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(old_status, "held");
            let held_row = sqlx::query(
                r#"
                select id, state, body, retry_work_item_id, latest_seq
                from agent_held_outputs
                where work_item_id = $1
                order by created_at asc
                limit 1
                "#,
            )
            .bind(work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let retry_work_item_id = held_row
                .get::<Option<Uuid>, _>("retry_work_item_id")
                .expect("retry work item");
            assert_eq!(held_row.get::<String, _>("state"), "requeued");
            assert_eq!(held_row.get::<String, _>("body"), "1");

            let retry = sqlx::query(
                r#"
                select title, source_kind, status, context, context_max_seq, freshness_generation
                from agent_work_items
                where id = $1
                "#,
            )
            .bind(retry_work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(retry.get::<String, _>("source_kind"), "freshness_retry");
            assert_eq!(retry.get::<String, _>("status"), "queued");
            assert_eq!(retry.get::<i64, _>("freshness_generation"), 1);
            assert_eq!(retry.get::<String, _>("title"), "Refresh held reply: count");
            let retry_context: String = retry.get("context");
            assert!(retry_context.contains("Freshness retry for held agent output."));
            assert!(retry_context.contains("Held draft:"));
            assert!(retry_context.contains("hold-other-agent"));
            assert!(retry_context.contains(&format!("source_work_item_id: {work_item_id}")));
            assert!(retry_context.contains("original context"));
            assert!(retry.get::<i64, _>("context_max_seq") >= held_row.get::<i64, _>("latest_seq"));

            let retry_run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, work_item_id, command, status)
                values ($1, $2, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(retry_work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_agent_id, sender_name, sender_role, body
                )
                values ($1, $2, $3, 'hold-other-agent', 'agent', '2')
                "#,
            )
            .bind(channel_id)
            .bind(thread_root_id)
            .bind(other_agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let retry_held = hold_work_item_output_if_stale(
                &pool,
                agent_id,
                retry_run_id,
                retry_work_item_id,
                "visible_reply",
                "2",
            )
            .await?;
            assert!(retry_held, "second-generation stale output should be held");
            let gen2_retry_work_item_id: Option<Uuid> = sqlx::query_scalar(
                r#"
                select retry_work_item_id
                from agent_held_outputs
                where work_item_id = $1
                "#,
            )
            .bind(retry_work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let gen2_retry_work_item_id = gen2_retry_work_item_id
                .expect("stale retry below the generation cap should queue another retry");
            let gen2_retry = sqlx::query(
                r#"
                select title, context, freshness_generation
                from agent_work_items
                where id = $1
                "#,
            )
            .bind(gen2_retry_work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(gen2_retry.get::<i64, _>("freshness_generation"), 2);
            assert_eq!(
                gen2_retry.get::<String, _>("title"),
                "Refresh held reply: count"
            );
            let gen2_context: String = gen2_retry.get("context");
            assert_eq!(
                gen2_context
                    .matches("Freshness retry for held agent output.")
                    .count(),
                1
            );
            assert!(gen2_context.contains(&format!("source_work_item_id: {work_item_id}")));
            assert!(gen2_context.contains(&format!("previous_work_item_id: {retry_work_item_id}")));
            assert!(gen2_context.contains("original context"));
            assert!(gen2_context.contains("hold-other-agent: 2"));
            assert!(!gen2_context.contains("hold-other-agent: 1"));
            assert!(!gen2_context.contains("Held draft:\n1"));
            assert!(!gen2_context.contains("Refresh held reply: Refresh held reply"));
            let retry_count_after_gen2: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from agent_work_items
                where source_kind = 'freshness_retry'
                "#,
            )
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(retry_count_after_gen2, 2);

            // At the generation cap the chain stops: held output is suppressed
            // and no further retry work item is queued.
            sqlx::query("update agent_work_items set freshness_generation = $2 where id = $1")
                .bind(gen2_retry_work_item_id)
                .bind(super::FRESHNESS_RETRY_MAX_GENERATIONS)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            sqlx::query(
                r#"
                insert into messages (
                    channel_id, thread_root_id, sender_agent_id, sender_name, sender_role, body
                )
                values ($1, $2, $3, 'hold-other-agent', 'agent', '3')
                "#,
            )
            .bind(channel_id)
            .bind(thread_root_id)
            .bind(other_agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let capped_held = hold_work_item_output_if_stale(
                &pool,
                agent_id,
                retry_run_id,
                gen2_retry_work_item_id,
                "visible_reply",
                "3",
            )
            .await?;
            assert!(capped_held, "capped stale output should still be held");
            let retry_count_after_cap: i64 = sqlx::query_scalar(
                r#"
                select count(*)
                from agent_work_items
                where source_kind = 'freshness_retry'
                "#,
            )
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(retry_count_after_cap, 2);
            let capped_state: String = sqlx::query_scalar(
                r#"
                select state
                from agent_held_outputs
                where work_item_id = $1
                "#,
            )
            .bind(gen2_retry_work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            assert_eq!(capped_state, "suppressed");
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }
}
