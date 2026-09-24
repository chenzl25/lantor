//! Structured owner decisions ("decision cards").
//!
//! An agent emits `decision_request` when it needs the owner to choose between
//! concrete options. Lantor posts a card message (with a plain-text rendering
//! for history/search/context tools) and records the decision. The owner answers
//! from the card or the Needs-you view; the answer is posted as an owner message
//! in the same thread that @mentions the requester, so the normal mention path
//! wakes the agent with full thread context.

use std::collections::HashSet;

use chrono::Utc;
use serde::Deserialize;
use sqlx::{sqlite::SqliteRow, Row, SqlitePool};
use uuid::Uuid;

use crate::agent_routing::upsert_agent_thread_subscription;
use crate::app::{to_string, CommandResult};
use crate::message_store::{load_message_patch_in_tx, send_owner_message_in_pool};
use crate::models::{Decision, DecisionOption};
use crate::text::compact_chars_middle;
use crate::ui_notifications::{enqueue_ui_event, enqueue_ui_event_in_tx, UiEvent};

pub(crate) const DECISION_MAX_OPTIONS: usize = 6;
const DECISION_TITLE_LIMIT: usize = 160;
const DECISION_CONTEXT_LIMIT: usize = 4000;
const DECISION_LABEL_LIMIT: usize = 80;
const DECISION_DETAIL_LIMIT: usize = 400;
const DECISION_OPTION_ID_LIMIT: usize = 32;
const DECISION_NOTE_LIMIT: usize = 4000;
/// Resolved decisions stay in the UI collection this long so their cards keep
/// rendering in recent history; older cards fall back to the message body.
const RESOLVED_DECISION_RETENTION_DAYS: i64 = 30;
const DECISION_LOAD_LIMIT: i64 = 500;

/// Statuses: `open` → `answered` (owner chose/replied), `dismissed` (owner
/// closed it without choosing) or `withdrawn` (the requesting agent retracted it).
pub(crate) const DECISION_STATUS_OPEN: &str = "open";

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DecisionOptionInput {
    #[serde(default)]
    pub(crate) id: Option<String>,
    pub(crate) label: String,
    #[serde(default, alias = "description")]
    pub(crate) detail: Option<String>,
    #[serde(default)]
    pub(crate) recommended: Option<bool>,
}

pub(crate) struct NewDecision<'a> {
    pub(crate) agent_id: Uuid,
    pub(crate) channel_id: Uuid,
    pub(crate) thread_root_id: Option<Uuid>,
    pub(crate) task_id: Option<Uuid>,
    pub(crate) title: &'a str,
    pub(crate) context: Option<&'a str>,
    pub(crate) options: Option<Vec<DecisionOptionInput>>,
}

fn clip(value: &str, limit: usize) -> String {
    compact_chars_middle(value.trim(), limit)
}

fn clip_single_line(value: &str, limit: usize) -> String {
    clip(
        &value.split_whitespace().collect::<Vec<_>>().join(" "),
        limit,
    )
}

fn normalize_option_id(raw: &str) -> String {
    let mut id = String::new();
    let mut last_dash = false;
    for ch in raw.trim().chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            id.push(ch);
            last_dash = false;
        } else if !id.is_empty() && !last_dash {
            id.push('-');
            last_dash = true;
        }
        if id.chars().count() >= DECISION_OPTION_ID_LIMIT {
            break;
        }
    }
    id.trim_end_matches('-').to_owned()
}

fn letter_id(index: usize) -> String {
    let letter = (b'a' + (index % 26) as u8) as char;
    if index < 26 {
        letter.to_string()
    } else {
        format!("{letter}{}", index / 26)
    }
}

/// Validates agent-provided options. Options are single-choice and mutually
/// exclusive. Missing ids become `a`, `b`, `c`...; duplicate ids get a suffix;
/// only the first `recommended` option keeps the flag. With no options the card
/// is a plain approval request.
pub(crate) fn normalize_decision_options(
    options: Option<Vec<DecisionOptionInput>>,
) -> CommandResult<Vec<DecisionOption>> {
    let options = options.unwrap_or_default();
    if options.is_empty() {
        return Ok(vec![
            DecisionOption {
                id: "approve".to_owned(),
                label: "Approve".to_owned(),
                detail: String::new(),
                recommended: false,
            },
            DecisionOption {
                id: "decline".to_owned(),
                label: "Decline".to_owned(),
                detail: String::new(),
                recommended: false,
            },
        ]);
    }
    if options.len() == 1 {
        return Err(
            "decision_request needs at least 2 options (or none for approve/decline)".to_owned(),
        );
    }
    if options.len() > DECISION_MAX_OPTIONS {
        return Err(format!(
            "decision_request supports at most {DECISION_MAX_OPTIONS} options"
        ));
    }
    let mut seen = HashSet::new();
    let mut has_recommendation = false;
    let mut normalized = Vec::with_capacity(options.len());
    for (index, option) in options.into_iter().enumerate() {
        let label = clip_single_line(&option.label, DECISION_LABEL_LIMIT);
        if label.is_empty() {
            return Err(format!("decision option {} label is required", index + 1));
        }
        let mut id = option
            .id
            .as_deref()
            .map(normalize_option_id)
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| letter_id(index));
        if seen.contains(&id) {
            let base = id.clone();
            let mut suffix = 2;
            while seen.contains(&id) {
                id = format!("{base}-{suffix}");
                suffix += 1;
            }
        }
        seen.insert(id.clone());
        let recommended = option.recommended.unwrap_or(false) && !has_recommendation;
        has_recommendation |= recommended;
        normalized.push(DecisionOption {
            id,
            label,
            detail: option
                .detail
                .as_deref()
                .map(|detail| clip(detail, DECISION_DETAIL_LIMIT))
                .unwrap_or_default(),
            recommended,
        });
    }
    Ok(normalized)
}

/// Plain-text rendering stored as the card message body. Agents read this via
/// history/context tools, and it is what search and activity previews show.
pub(crate) fn render_decision_body(
    title: &str,
    context: &str,
    options: &[DecisionOption],
) -> String {
    let mut body = format!("**Decision needed:** {title}");
    if !context.is_empty() {
        body.push_str("\n\n");
        body.push_str(context);
    }
    body.push('\n');
    for option in options {
        body.push_str(&format!("\n- **[{}] {}**", option.id, option.label));
        if option.recommended {
            body.push_str(" (recommended)");
        }
        if !option.detail.is_empty() {
            body.push_str(" — ");
            body.push_str(&option.detail.replace('\n', " "));
        }
    }
    body
}

pub(crate) async fn create_agent_decision(
    pool: &SqlitePool,
    input: NewDecision<'_>,
) -> CommandResult<(Uuid, Uuid)> {
    let title = clip_single_line(input.title, DECISION_TITLE_LIMIT);
    if title.is_empty() {
        return Err("decision_request title is required".to_owned());
    }
    let context = input
        .context
        .map(|context| clip(context, DECISION_CONTEXT_LIMIT))
        .unwrap_or_default();
    let options = normalize_decision_options(input.options)?;
    let body = render_decision_body(&title, &context, &options);
    let options_json = serde_json::to_string(&options).map_err(to_string)?;

    if let Some(thread_root_id) = input.thread_root_id {
        let root_channel: Option<Uuid> = sqlx::query_scalar(
            "select channel_id from messages where id = $1 and thread_root_id is null",
        )
        .bind(thread_root_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?;
        if root_channel != Some(input.channel_id) {
            return Err("thread_root_id does not belong to target channel".to_owned());
        }
    }
    let sender = sqlx::query("select display_name, role from agents where id = $1")
        .bind(input.agent_id)
        .fetch_one(pool)
        .await
        .map_err(to_string)?;
    let sender_name: String = sender.get("display_name");
    let sender_role: String = sender.get("role");

    // Card message and decision row commit together, so no client ever sees
    // the card message without its decision.
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let message_id: Uuid = sqlx::query_scalar(
        r#"
        insert into messages (
            channel_id, thread_root_id, sender_agent_id, sender_name, sender_role, body, is_task
        )
        values ($1, $2, $3, $4, $5, $6, 0)
        returning id
        "#,
    )
    .bind(input.channel_id)
    .bind(input.thread_root_id)
    .bind(input.agent_id)
    .bind(sender_name)
    .bind(sender_role)
    .bind(&body)
    .fetch_one(&mut *tx)
    .await
    .map_err(to_string)?;
    let decision_id: Uuid = sqlx::query_scalar(
        r#"
        insert into decisions (
            message_id, channel_id, thread_root_id, requester_agent_id, task_id,
            title, context, options
        )
        values ($1, $2, $3, $4, $5, $6, $7, $8)
        returning id
        "#,
    )
    .bind(message_id)
    .bind(input.channel_id)
    .bind(input.thread_root_id)
    .bind(input.agent_id)
    .bind(input.task_id)
    .bind(&title)
    .bind(&context)
    .bind(options_json)
    .fetch_one(&mut *tx)
    .await
    .map_err(to_string)?;
    enqueue_ui_event_in_tx(
        &mut tx,
        &UiEvent::Refresh {
            reason: "decision_created",
        },
    )
    .await?;
    let message = load_message_patch_in_tx(&mut tx, message_id).await?;
    enqueue_ui_event_in_tx(
        &mut tx,
        &UiEvent::MessageUpsert {
            reason: "message",
            message: &message,
        },
    )
    .await?;
    tx.commit().await.map_err(to_string)?;
    upsert_agent_thread_subscription(
        pool,
        input.agent_id,
        input.channel_id,
        input.thread_root_id.unwrap_or(message_id),
        "agent_message",
        Some(message_id),
    )
    .await?;
    Ok((decision_id, message_id))
}

fn decision_from_row(row: &SqliteRow) -> Decision {
    let options: String = row.get("options");
    Decision {
        id: row.get("id"),
        message_id: row.get("message_id"),
        channel_id: row.get("channel_id"),
        channel_name: row.get("channel_name"),
        thread_root_id: row.get("thread_root_id"),
        requester_agent_id: row.get("requester_agent_id"),
        requester_handle: row.get("requester_handle"),
        task_id: row.get("task_id"),
        task_number: row.get("task_number"),
        title: row.get("title"),
        context: row.get("context"),
        options: serde_json::from_str(&options).unwrap_or_default(),
        status: row.get("status"),
        answer_option_id: row.get("answer_option_id"),
        answer_note: row.get("answer_note"),
        answer_message_id: row.get("answer_message_id"),
        resolved_at: row.get("resolved_at"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

const DECISION_SELECT: &str = r#"
    select
        d.id,
        d.message_id,
        d.channel_id,
        c.name as channel_name,
        d.thread_root_id,
        d.requester_agent_id,
        a.handle as requester_handle,
        d.task_id,
        t.number as task_number,
        d.title,
        d.context,
        d.options,
        d.status,
        d.answer_option_id,
        d.answer_note,
        d.answer_message_id,
        d.resolved_at,
        d.created_at,
        d.updated_at
    from decisions d
    join channels c on c.id = d.channel_id
    left join agents a on a.id = d.requester_agent_id
    left join tasks t on t.id = d.task_id
"#;

/// Open decisions plus recently resolved ones, newest first.
pub(crate) async fn load_decisions(pool: &SqlitePool) -> CommandResult<Vec<Decision>> {
    let cutoff = (Utc::now() - chrono::Duration::days(RESOLVED_DECISION_RETENTION_DAYS))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, false);
    let sql = format!(
        "{DECISION_SELECT} where d.status = 'open' or d.updated_at >= $1 \
         order by d.created_at desc limit $2"
    );
    let rows = sqlx::query(&sql)
        .bind(cutoff)
        .bind(DECISION_LOAD_LIMIT)
        .fetch_all(pool)
        .await
        .map_err(to_string)?;
    Ok(rows.iter().map(decision_from_row).collect())
}

/// One agent's own decisions, newest first; `open_only` hides resolved cards.
pub(crate) async fn load_agent_decisions(
    pool: &SqlitePool,
    agent_id: Uuid,
    open_only: bool,
    limit: i64,
) -> CommandResult<Vec<Decision>> {
    let sql = format!(
        "{DECISION_SELECT} where d.requester_agent_id = $1 and ($2 = 0 or d.status = 'open') \
         order by d.created_at desc limit $3"
    );
    let rows = sqlx::query(&sql)
        .bind(agent_id)
        .bind(open_only)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(to_string)?;
    Ok(rows.iter().map(decision_from_row).collect())
}

pub(crate) async fn load_decision(pool: &SqlitePool, decision_id: Uuid) -> CommandResult<Decision> {
    let sql = format!("{DECISION_SELECT} where d.id = $1");
    sqlx::query(&sql)
        .bind(decision_id)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?
        .map(|row| decision_from_row(&row))
        .ok_or_else(|| "decision not found".to_owned())
}

async fn notify_decision_changed(pool: &SqlitePool) -> CommandResult<()> {
    enqueue_ui_event(
        pool,
        &UiEvent::Refresh {
            reason: "decision_updated",
        },
    )
    .await
}

fn render_answer_body(decision: &Decision, option: Option<&DecisionOption>, note: &str) -> String {
    let mut body = String::new();
    if let Some(handle) = decision.requester_handle.as_deref() {
        body.push('@');
        body.push_str(handle);
        body.push(' ');
    }
    body.push_str(&format!("Decision: {}\n→ ", decision.title));
    match option {
        Some(option) => body.push_str(&format!("**[{}] {}**", option.id, option.label)),
        None => body.push_str("**Custom answer**"),
    }
    if !note.is_empty() {
        body.push_str("\n\n");
        body.push_str(note);
    }
    body
}

/// Records the owner's answer and posts it into the decision's thread as an
/// owner message that @mentions the requester, which queues the requester's
/// follow-up through the normal mention path.
pub(crate) async fn answer_decision_in_pool(
    pool: &SqlitePool,
    decision_id: Uuid,
    option_id: Option<&str>,
    note: Option<&str>,
) -> CommandResult<Decision> {
    let decision = load_decision(pool, decision_id).await?;
    if decision.status != DECISION_STATUS_OPEN {
        return Err("This decision was already resolved.".to_owned());
    }
    let option_id = option_id.map(str::trim).filter(|id| !id.is_empty());
    let option = match option_id {
        Some(option_id) => Some(
            decision
                .options
                .iter()
                .find(|option| option.id == option_id)
                .cloned()
                .ok_or_else(|| format!("unknown decision option: {option_id}"))?,
        ),
        None => None,
    };
    let note = note
        .map(|note| clip(note, DECISION_NOTE_LIMIT))
        .unwrap_or_default();
    if option.is_none() && note.is_empty() {
        return Err("Choose an option or write an answer.".to_owned());
    }

    // Claim the decision first so a double tap cannot post two answers.
    let claimed = sqlx::query(
        r#"
        update decisions
        set status = 'answered',
            answer_option_id = $2,
            answer_note = $3,
            resolved_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1 and status = 'open'
        "#,
    )
    .bind(decision_id)
    .bind(option.as_ref().map(|option| option.id.clone()))
    .bind(&note)
    .execute(pool)
    .await
    .map_err(to_string)?
    .rows_affected();
    if claimed == 0 {
        return Err("This decision was already resolved.".to_owned());
    }

    let body = render_answer_body(&decision, option.as_ref(), &note);
    let thread_root_id = decision.thread_root_id.unwrap_or(decision.message_id);
    let answer = send_owner_message_in_pool(
        pool,
        None,
        decision.channel_id,
        Some(thread_root_id),
        &body,
        false,
        Vec::new(),
    )
    .await;
    let answer = match answer {
        Ok(answer) => answer,
        Err(err) => {
            sqlx::query(
                r#"
                update decisions
                set status = 'open',
                    answer_option_id = null,
                    answer_note = '',
                    resolved_at = null,
                    updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
                where id = $1 and status = 'answered' and answer_message_id is null
                "#,
            )
            .bind(decision_id)
            .execute(pool)
            .await
            .map_err(to_string)?;
            return Err(err);
        }
    };
    sqlx::query(
        r#"
        update decisions
        set answer_message_id = $2,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1
        "#,
    )
    .bind(decision_id)
    .bind(answer.id)
    .execute(pool)
    .await
    .map_err(to_string)?;
    notify_decision_changed(pool).await?;
    load_decision(pool, decision_id).await
}

/// Owner closes a decision without choosing (e.g. it was settled in chat).
pub(crate) async fn dismiss_decision_in_pool(
    pool: &SqlitePool,
    decision_id: Uuid,
) -> CommandResult<()> {
    let updated = sqlx::query(
        r#"
        update decisions
        set status = 'dismissed',
            resolved_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1 and status = 'open'
        "#,
    )
    .bind(decision_id)
    .execute(pool)
    .await
    .map_err(to_string)?
    .rows_affected();
    if updated == 0 {
        load_decision(pool, decision_id).await?;
        return Err("This decision was already resolved.".to_owned());
    }
    notify_decision_changed(pool).await
}

/// Normalizes a message reference as shown by history tools (`msg=6e0853de`)
/// or a full UUID into lowercase hex without dashes.
fn message_ref_hex(message_ref: &str) -> CommandResult<String> {
    let hex: String = message_ref
        .trim()
        .trim_start_matches("msg=")
        .chars()
        .filter(|ch| *ch != '-')
        .collect::<String>()
        .to_lowercase();
    if hex.len() < 8 || hex.len() > 32 || !hex.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(
            "decision_withdraw message_id must be a message UUID or its 8+ hex prefix".to_owned(),
        );
    }
    Ok(hex)
}

/// The requesting agent withdraws its own open decision, e.g. because the owner
/// already answered in chat or the question became moot.
pub(crate) async fn withdraw_agent_decision(
    pool: &SqlitePool,
    agent_id: Uuid,
    message_ref: &str,
    reason: Option<&str>,
) -> CommandResult<Uuid> {
    let prefix = message_ref_hex(message_ref)?;
    let matches: Vec<Uuid> = sqlx::query_scalar(
        r#"
        select id
        from decisions
        where requester_agent_id = $1
          and status = 'open'
          and substr(lower(hex(message_id)), 1, length($2)) = $2
        limit 2
        "#,
    )
    .bind(agent_id)
    .bind(&prefix)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;
    let decision_id = match matches.as_slice() {
        [decision_id] => *decision_id,
        [] => {
            return Err(format!(
                "no open decision of yours matches message {message_ref}"
            ))
        }
        _ => return Err(format!("message reference {message_ref} is ambiguous")),
    };
    let reason = reason
        .map(|reason| clip(reason, DECISION_NOTE_LIMIT))
        .unwrap_or_default();
    sqlx::query(
        r#"
        update decisions
        set status = 'withdrawn',
            answer_note = $2,
            resolved_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1 and status = 'open'
        "#,
    )
    .bind(decision_id)
    .bind(reason)
    .execute(pool)
    .await
    .map_err(to_string)?;
    notify_decision_changed(pool).await?;
    Ok(decision_id)
}

/// Resolves the default placement and task for a decision raised during a run:
/// the run's work-item conversation, like the agent's normal reply.
pub(crate) async fn resolve_run_decision_anchor(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
) -> CommandResult<(Option<Uuid>, Option<Uuid>, Option<Uuid>)> {
    let row = sqlx::query(
        r#"
        select w.channel_id, w.thread_root_id, w.task_id
        from agent_runs r
        left join agent_work_items w on w.id = r.work_item_id
        where r.id = $1 and r.agent_id = $2
        "#,
    )
    .bind(run_id)
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    Ok(row
        .map(|row| {
            (
                row.get("channel_id"),
                row.get("thread_root_id"),
                row.get("task_id"),
            )
        })
        .unwrap_or((None, None, None)))
}

#[cfg(test)]
#[path = "tests/decisions.rs"]
mod tests;
