//! Control lines Lantor could not apply, told back to the agent that wrote them.
//!
//! An agent writes a `LANTOR_EVENT` line and moves on; a rejected line used to
//! reach only the run log and the activity feed, so the agent believed it had
//! taken effect. Each rejection is kept here and shown at the start of the
//! agent's next turn, until a turn that showed it has finished.

use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::app::{to_string, CommandResult};

/// Rejections older than this are no longer shown; the supervisor's cleanup
/// deletes them after 30 days.
const SHOWN_FOR_DAYS: i64 = 7;
const SHOWN_AT_MOST: i64 = 5;
const ERROR_LIMIT: usize = 300;
const EXCERPT_LIMIT: usize = 300;

/// Keeps a rejected line for the agent's next turn. The same line rejected
/// twice in one run, as a streamed line and again from the final text, is kept
/// once.
pub(crate) async fn record_control_rejection(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    json: &str,
    error: &str,
) -> CommandResult<()> {
    let event_type = serde_json::from_str::<Value>(json)
        .ok()
        .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_default();
    sqlx::query(
        r#"
        insert into agent_control_rejections (agent_id, run_id, event_hash, event_type, error, excerpt)
        values ($1, $2, $3, $4, $5, $6)
        on conflict (run_id, event_hash) do nothing
        "#,
    )
    .bind(agent_id)
    .bind(run_id)
    .bind(format!("{:x}", Sha256::digest(json.as_bytes())))
    .bind(event_type)
    .bind(shorten(without_expected_list(error), ERROR_LIMIT))
    .bind(shorten(json, EXCERPT_LIMIT))
    .execute(pool)
    .await
    .map_err(to_string)?;
    Ok(())
}

/// The notice for the start of `work_item_id`'s prompt, listing the agent's
/// recent rejections that no finished turn has shown yet, and records that
/// this work item shows them. A work item that ends without finishing (failed,
/// cancelled or still queued) leaves them for the next one.
pub(crate) async fn take_rejection_notice(
    pool: &SqlitePool,
    agent_id: Uuid,
    work_item_id: Uuid,
) -> CommandResult<Option<String>> {
    let mut transaction = pool.begin().await.map_err(to_string)?;
    let rows = sqlx::query(
        r#"
        select r.id, r.event_type, r.error, r.excerpt, r.created_at
        from agent_control_rejections r
        left join agent_work_items shown on shown.id = r.shown_work_item_id
        where r.agent_id = $1
          and r.created_at >= strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now', $2)
          and (shown.id is null
               or shown.id = $3
               or shown.status not in ('done', 'silent', 'held'))
        order by r.id desc
        limit $4
        "#,
    )
    .bind(agent_id)
    .bind(format!("-{SHOWN_FOR_DAYS} days"))
    .bind(work_item_id)
    .bind(SHOWN_AT_MOST)
    .fetch_all(&mut *transaction)
    .await
    .map_err(to_string)?;
    if rows.is_empty() {
        return Ok(None);
    }
    for row in &rows {
        sqlx::query("update agent_control_rejections set shown_work_item_id = $2 where id = $1")
            .bind(row.get::<i64, _>("id"))
            .bind(work_item_id)
            .execute(&mut *transaction)
            .await
            .map_err(to_string)?;
    }
    transaction.commit().await.map_err(to_string)?;

    let mut lines = vec![
        "Lantor rejected control lines from your earlier replies, so nothing was applied for them:"
            .to_owned(),
    ];
    for row in rows.iter().rev() {
        let event_type: String = row.get("event_type");
        let error: String = row.get("error");
        let excerpt: String = row.get("excerpt");
        let created_at: String = row.get("created_at");
        let label = if event_type.is_empty() {
            "a LANTOR_EVENT line".to_owned()
        } else {
            format!("`{event_type}`")
        };
        lines.push(format!(
            "- {} UTC, {label}: {error}\n  The line began: LANTOR_EVENT {excerpt}\n  {}",
            created_at
                .get(..16)
                .unwrap_or(&created_at)
                .replace('T', " "),
            rejection_hint(&event_type, &error),
        ));
    }
    Ok(Some(lines.join("\n")))
}

fn rejection_hint(event_type: &str, error: &str) -> &'static str {
    let removed_memory_event = ["memory_append", "memory_compact"]
        .iter()
        .any(|name| event_type == *name || error.contains(&format!("`{name}`")));
    if removed_memory_event {
        "Lantor no longer applies memory_append or memory_compact. MEMORY.md and notes/ are files in your working directory: make that change now with your file tools, and remove any instruction to use these events from your MEMORY.md."
    } else {
        "If the change is still needed, make it again with a corrected line or a context tool command."
    }
}

/// An unknown type names every supported type after it; the agent's prompt
/// already lists them.
fn without_expected_list(error: &str) -> &str {
    match error.find(", expected one of") {
        Some(end) if error.starts_with("unknown variant") => &error[..end],
        _ => error,
    }
}

fn shorten(text: &str, limit: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(limit) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
#[path = "../tests/events_rejections.rs"]
mod tests;
