use std::{
    env, fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{sqlite::SqliteRow, Row, SqlitePool};
use uuid::Uuid;

use crate::agent_routing::resolve_agent_by_handle;
use crate::db::db_connect;
use crate::message_store::load_artifact;
use crate::{
    app::{to_string, CommandResult},
    attachments::{attachment_summary_sql, format_attachment_size},
    text::compact_chars_middle,
};

const AGENT_CONTEXT_TOOL_MESSAGE_LIMIT: usize = 2_000;

struct AgentContextTarget {
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    label: String,
}

struct AgentWorkspaceTarget {
    id: Uuid,
    handle: String,
    working_directory: String,
}

struct AgentInboxTarget {
    id: Uuid,
    handle: String,
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find_map(|window| (window[0] == name).then(|| window[1].clone()))
}

fn has_arg(args: &[String], name: &str) -> bool {
    args.iter().any(|arg| arg == name)
}

fn parse_context_tool_limit(args: &[String], default: i64, max: i64) -> CommandResult<i64> {
    let Some(raw) = arg_value(args, "--limit") else {
        return Ok(default);
    };
    let parsed = raw
        .parse::<i64>()
        .map_err(|_| format!("invalid --limit value: {raw}"))?;
    Ok(parsed.clamp(1, max))
}

fn parse_context_tool_usize_limit(
    args: &[String],
    default: usize,
    max: usize,
) -> CommandResult<usize> {
    let Some(raw) = arg_value(args, "--limit") else {
        return Ok(default);
    };
    let parsed = raw
        .parse::<usize>()
        .map_err(|_| format!("invalid --limit value: {raw}"))?;
    Ok(parsed.clamp(1, max))
}

fn parse_context_tool_named_usize(
    args: &[String],
    name: &str,
    default: usize,
    max: usize,
) -> CommandResult<usize> {
    let Some(raw) = arg_value(args, name) else {
        return Ok(default);
    };
    let parsed = raw
        .parse::<usize>()
        .map_err(|_| format!("invalid {name} value: {raw}"))?;
    Ok(parsed.clamp(1, max))
}

pub(crate) fn short_id(id: Uuid) -> String {
    id.to_string().chars().take(8).collect()
}

fn tail_chars(value: &str, limit: usize) -> String {
    let trimmed = value.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= limit {
        return trimmed.to_owned();
    }
    let omitted = chars.len().saturating_sub(limit);
    let tail = chars
        .iter()
        .skip(chars.len().saturating_sub(limit))
        .collect::<String>();
    format!("[... Lantor omitted {omitted} earlier chars from run log ...]\n{tail}")
}

fn split_context_target(raw_target: &str) -> (String, Option<String>) {
    let target = raw_target.trim();
    if let Some(rest) = target.strip_prefix("dm:@") {
        if let Some((handle, thread)) = rest.split_once(':') {
            return (format!("dm:@{handle}"), Some(thread.to_owned()));
        }
        return (target.to_owned(), None);
    }
    if let Some(rest) = target.strip_prefix('#') {
        if let Some((channel, thread)) = rest.split_once(':') {
            return (format!("#{channel}"), Some(thread.to_owned()));
        }
    }
    if let Some((channel, thread)) = target.split_once(':') {
        return (channel.to_owned(), Some(thread.to_owned()));
    }
    (target.to_owned(), None)
}

async fn resolve_agent_context_channel(
    pool: &SqlitePool,
    channel_ref: &str,
) -> CommandResult<(Uuid, String)> {
    let channel_ref = channel_ref.trim();
    if channel_ref.is_empty() {
        return Err("target channel is empty".to_owned());
    }

    if let Some(handle) = channel_ref.strip_prefix("dm:@") {
        let row = sqlx::query(
            r#"
            select c.id, a.handle
            from channels c
            join agents a on a.id = c.dm_agent_id
            where c.kind = 'dm' and lower(a.handle) = lower($1)
            "#,
        )
        .bind(handle)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?;
        let Some(row) = row else {
            return Err(format!("unknown DM target: {channel_ref}"));
        };
        let channel_id: Uuid = row.get("id");
        let handle: String = row.get("handle");
        return Ok((channel_id, format!("dm:@{handle}")));
    }

    if let Ok(channel_id) = Uuid::parse_str(channel_ref.trim_start_matches("channel:")) {
        let row = sqlx::query("select name, kind from channels where id = $1")
            .bind(channel_id)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?;
        let Some(row) = row else {
            return Err(format!("unknown channel id: {channel_id}"));
        };
        let name: String = row.get("name");
        let kind: String = row.get("kind");
        return Ok((
            channel_id,
            if kind == "dm" {
                format!("dm:{name}")
            } else {
                format!("#{name}")
            },
        ));
    }

    let name = channel_ref.trim_start_matches('#');
    let row = sqlx::query("select id, name, kind from channels where lower(name) = lower($1)")
        .bind(name)
        .fetch_optional(pool)
        .await
        .map_err(to_string)?;
    let Some(row) = row else {
        return Err(format!("unknown channel: {channel_ref}"));
    };
    let channel_id: Uuid = row.get("id");
    let name: String = row.get("name");
    let kind: String = row.get("kind");
    Ok((
        channel_id,
        if kind == "dm" {
            format!("dm:{name}")
        } else {
            format!("#{name}")
        },
    ))
}

async fn resolve_agent_context_thread(
    pool: &SqlitePool,
    channel_id: Uuid,
    raw_thread: &str,
) -> CommandResult<Uuid> {
    let raw_thread = raw_thread.trim();
    if raw_thread.is_empty() {
        return Err("thread reference is empty".to_owned());
    }
    if let Ok(thread_id) = Uuid::parse_str(raw_thread) {
        return Ok(thread_id);
    }
    let pattern = format!("{raw_thread}%");
    let thread_id: Option<Uuid> = sqlx::query_scalar(
        r#"
        select id
        from messages
        where channel_id = $1 and lower(hex(id)) like replace(lower($2), '-', '')
          and lifecycle in ('streaming_final', 'committed')
        order by created_at asc
        limit 1
        "#,
    )
    .bind(channel_id)
    .bind(pattern)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    thread_id.ok_or_else(|| format!("unknown thread/message id in target: {raw_thread}"))
}

async fn resolve_agent_context_target(
    pool: &SqlitePool,
    raw_target: &str,
    thread_override: Option<&str>,
) -> CommandResult<AgentContextTarget> {
    let (channel_ref, thread_from_target) = split_context_target(raw_target);
    let (channel_id, channel_label) = resolve_agent_context_channel(pool, &channel_ref).await?;
    let thread_ref = thread_override
        .map(str::to_owned)
        .or(thread_from_target)
        .filter(|thread| !thread.trim().is_empty());
    let thread_root_id = match thread_ref {
        Some(thread_ref) => {
            Some(resolve_agent_context_thread(pool, channel_id, &thread_ref).await?)
        }
        None => None,
    };
    let label = match thread_root_id {
        Some(thread_root_id) => format!("{channel_label}:{}", short_id(thread_root_id)),
        None => channel_label,
    };
    Ok(AgentContextTarget {
        channel_id,
        thread_root_id,
        label,
    })
}

fn format_context_message_target(row: &SqliteRow, target_label: Option<&str>) -> String {
    if let Some(target_label) = target_label {
        return target_label.to_owned();
    }
    let channel_name = row
        .try_get::<String, _>("channel_name")
        .unwrap_or_else(|_| "unknown".to_owned());
    let channel_kind = row
        .try_get::<String, _>("channel_kind")
        .unwrap_or_else(|_| "channel".to_owned());
    let thread_root_id = row
        .try_get::<Option<Uuid>, _>("thread_root_id")
        .ok()
        .flatten();
    let mut target = if channel_kind == "dm" {
        format!("dm:{channel_name}")
    } else {
        format!("#{channel_name}")
    };
    if let Some(thread_root_id) = thread_root_id {
        target.push(':');
        target.push_str(&short_id(thread_root_id));
    }
    target
}

fn format_context_message_row(row: &SqliteRow, target_label: Option<&str>) -> String {
    let id: Uuid = row.get("id");
    let sender_name: String = row.get("sender_name");
    let sender_role: String = row.get("sender_role");
    let body: String = row.get("body");
    let created_at: DateTime<Utc> = row.get("created_at");
    let task_number: Option<i64> = row.get("task_number");
    let task_status: Option<String> = row.get("task_status");
    let target = format_context_message_target(row, target_label);
    let body = compact_chars_middle(&body, AGENT_CONTEXT_TOOL_MESSAGE_LIMIT).replace('\n', " ");
    let mut output = format!(
        "[target={} msg={} time={} type={}] {}: {}",
        target,
        short_id(id),
        created_at.to_rfc3339(),
        sender_role,
        sender_name,
        body
    );
    if let Some(task_number) = task_number {
        output.push_str(&format!(
            " [task #{task_number} status={}]",
            task_status.unwrap_or_else(|| "unknown".to_owned())
        ));
    }
    if let Ok(attachment_summary) = row.try_get::<String, _>("attachment_summary") {
        if !attachment_summary.trim().is_empty() {
            output.push_str("\n  attachments:");
            for line in attachment_summary.lines() {
                output.push_str("\n  - ");
                output.push_str(line);
            }
            output.push_str(
                "\n  To inspect an attachment, run attachment-info with its attachment_id.",
            );
        }
    }
    output
}

pub(crate) async fn agent_context_history_read(
    pool: &SqlitePool,
    args: &[String],
) -> CommandResult<String> {
    let target = arg_value(args, "--target")
        .or_else(|| arg_value(args, "--channel"))
        .ok_or_else(|| "history-read requires --target \"#channel[:thread]\"".to_owned())?;
    let limit = parse_context_tool_limit(args, 30, 100)?;
    let thread_override = arg_value(args, "--thread");
    let target = resolve_agent_context_target(pool, &target, thread_override.as_deref()).await?;

    let rows = if let Some(thread_root_id) = target.thread_root_id {
        sqlx::query(&format!(
            r#"
            select
                m.id, m.sender_name, m.sender_role, m.body, m.thread_root_id, m.created_at,
                t.number as task_number, t.status as task_status,
                {}
            from messages m
            left join tasks t on t.message_id = m.id
            where m.channel_id = $1
              and (m.id = $2 or m.thread_root_id = $2)
              and m.lifecycle in ('streaming_final', 'committed')
            order by m.created_at desc
            limit $3
            "#,
            attachment_summary_sql()
        ))
        .bind(target.channel_id)
        .bind(thread_root_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(to_string)?
    } else {
        sqlx::query(&format!(
            r#"
            select
                m.id, m.sender_name, m.sender_role, m.body, m.thread_root_id, m.created_at,
                t.number as task_number, t.status as task_status,
                {}
            from messages m
            left join tasks t on t.message_id = m.id
            where m.channel_id = $1
              and m.thread_root_id is null
              and m.lifecycle in ('streaming_final', 'committed')
            order by m.created_at desc
            limit $2
            "#,
            attachment_summary_sql()
        ))
        .bind(target.channel_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(to_string)?
    };

    let mut output = vec![format!(
        "Lantor history for {} ({} message{})",
        target.label,
        rows.len(),
        if rows.len() == 1 { "" } else { "s" }
    )];
    for row in rows.into_iter().rev() {
        output.push(format_context_message_row(&row, Some(&target.label)));
    }
    Ok(output.join("\n\n"))
}

pub(crate) async fn agent_context_message_search(
    pool: &SqlitePool,
    args: &[String],
) -> CommandResult<String> {
    let query = arg_value(args, "--query")
        .or_else(|| arg_value(args, "-q"))
        .ok_or_else(|| "message-search requires --query <text>".to_owned())?;
    let query = query.trim();
    if query.is_empty() {
        return Err("message-search query is empty".to_owned());
    }
    let limit = parse_context_tool_limit(args, 30, 100)?;
    let target = match arg_value(args, "--target").or_else(|| arg_value(args, "--channel")) {
        Some(target) => Some(resolve_agent_context_target(pool, &target, None).await?),
        None => None,
    };
    let pattern = format!("%{query}%");

    let rows = if let Some(target) = target {
        sqlx::query(&format!(
            r#"
            select
                m.id, m.sender_name, m.sender_role, m.body, m.thread_root_id, m.created_at,
                c.name as channel_name, c.kind as channel_kind,
                t.number as task_number, t.status as task_status,
                {}
            from messages m
            join channels c on c.id = m.channel_id
            left join tasks t on t.message_id = m.id
            where m.channel_id = $1
              and lower(m.body) like lower($2)
              and m.lifecycle in ('streaming_final', 'committed')
            order by m.created_at desc
            limit $3
            "#,
            attachment_summary_sql()
        ))
        .bind(target.channel_id)
        .bind(pattern)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(to_string)?
    } else {
        sqlx::query(&format!(
            r#"
            select
                m.id, m.sender_name, m.sender_role, m.body, m.thread_root_id, m.created_at,
                c.name as channel_name, c.kind as channel_kind,
                t.number as task_number, t.status as task_status,
                {}
            from messages m
            join channels c on c.id = m.channel_id
            left join tasks t on t.message_id = m.id
            where lower(m.body) like lower($1)
              and m.lifecycle in ('streaming_final', 'committed')
            order by m.created_at desc
            limit $2
            "#,
            attachment_summary_sql()
        ))
        .bind(pattern)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(to_string)?
    };

    let mut output = vec![format!(
        "Lantor message search for {:?} ({} result{})",
        query,
        rows.len(),
        if rows.len() == 1 { "" } else { "s" }
    )];
    for row in rows {
        output.push(format_context_message_row(&row, None));
    }
    Ok(output.join("\n\n"))
}

pub(crate) async fn agent_context_attachment_info(
    pool: &SqlitePool,
    args: &[String],
) -> CommandResult<String> {
    let raw_id = arg_value(args, "--attachment-id")
        .or_else(|| arg_value(args, "--id"))
        .ok_or_else(|| "attachment-info requires --attachment-id <uuid>".to_owned())?;
    let attachment_id =
        Uuid::parse_str(raw_id.trim()).map_err(|err| format!("invalid attachment id: {err}"))?;
    let row = sqlx::query(
        r#"
        select
            ma.id,
            ma.message_id,
            ma.original_name,
            ma.mime_type,
            ma.size_bytes,
            ma.storage_path,
            ma.created_at,
            m.channel_id,
            m.thread_root_id,
            c.name as channel_name,
            c.kind as channel_kind
        from message_attachments ma
        join messages m on m.id = ma.message_id
        join channels c on c.id = m.channel_id
        where ma.id = $1
        "#,
    )
    .bind(attachment_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?
    .ok_or_else(|| format!("attachment {attachment_id} does not exist"))?;

    let mime_type: String = row.get("mime_type");
    let storage_path: String = row.get("storage_path");
    let exists = PathBuf::from(&storage_path).exists();
    let channel_name: String = row.get("channel_name");
    let channel_kind: String = row.get("channel_kind");
    let surface = if channel_kind == "dm" {
        format!("dm:{channel_name}")
    } else {
        format!("#{channel_name}")
    };
    let mut output = vec![
        format!("Lantor attachment {}", row.get::<Uuid, _>("id")),
        format!("message_id={}", row.get::<Uuid, _>("message_id")),
        format!("surface={surface}"),
        format!("name=\"{}\"", row.get::<String, _>("original_name")),
        format!("mime={mime_type}"),
        format!("size={}", format_attachment_size(row.get("size_bytes"))),
        format!("local_path=\"{storage_path}\""),
        format!("file_exists={exists}"),
    ];
    if mime_type.starts_with("image/") {
        output.push(
            "vision_hint=This is an image attachment. Inspect local_path directly with your runtime's file/vision support before answering visual UI questions."
                .to_owned(),
        );
    }
    Ok(output.join("\n"))
}

pub(crate) async fn agent_context_agent_inspect(
    pool: &SqlitePool,
    args: &[String],
) -> CommandResult<String> {
    let target = arg_value(args, "--target")
        .or_else(|| arg_value(args, "--agent"))
        .ok_or_else(|| "agent-inspect requires --target @handle".to_owned())?;
    let agent_id = resolve_agent_by_handle(pool, &target).await?;
    let agent = sqlx::query(
        r#"
        select handle, display_name, role, status, runtime, model, reasoning_effort, service_tier,
               avatar, description, working_directory, daily_budget_micros
        from agents
        where id = $1
        "#,
    )
    .bind(agent_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;
    let handle: String = agent.get("handle");
    let mut output = vec![
        format!("Agent @{handle}"),
        format!("display_name={}", agent.get::<String, _>("display_name")),
        format!("role={}", agent.get::<String, _>("role")),
        format!("status={}", agent.get::<String, _>("status")),
        format!(
            "runtime={}/{}",
            agent.get::<String, _>("runtime"),
            agent.get::<String, _>("model")
        ),
        {
            let runtime = agent.get::<String, _>("runtime");
            let reasoning_effort = agent.get::<String, _>("reasoning_effort");
            if runtime.eq_ignore_ascii_case("claude") {
                let effort = if reasoning_effort.trim().is_empty() {
                    "default".to_owned()
                } else {
                    reasoning_effort
                };
                format!("claude_options=effort:{effort}")
            } else {
                format!(
                    "codex_options=reasoning_effort:{} service_tier:{}",
                    reasoning_effort,
                    agent.get::<String, _>("service_tier")
                )
            }
        },
        format!("description={}", agent.get::<String, _>("description")),
        format!(
            "working_directory={}",
            agent.get::<String, _>("working_directory")
        ),
        format!(
            "daily_budget=${:.4}",
            agent.get::<i64, _>("daily_budget_micros") as f64 / 1_000_000.0
        ),
    ];

    let runs = sqlx::query(
        r#"
        select status, command, input_tokens, output_tokens, cost_micros, started_at, stopped_at
        from agent_runs
        where agent_id = $1
        order by started_at desc
        limit 5
        "#,
    )
    .bind(agent_id)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;
    if !runs.is_empty() {
        output.push("recent_runs:".to_owned());
        for row in runs {
            let started_at: DateTime<Utc> = row.get("started_at");
            let stopped_at: Option<DateTime<Utc>> = row.get("stopped_at");
            output.push(format!(
                "- {} status={} tokens={}/{} cost=${:.4} command=\"{}\" stopped={}",
                started_at.to_rfc3339(),
                row.get::<String, _>("status"),
                row.get::<i64, _>("input_tokens"),
                row.get::<i64, _>("output_tokens"),
                row.get::<i64, _>("cost_micros") as f64 / 1_000_000.0,
                compact_chars_middle(&row.get::<String, _>("command"), 120).replace('"', "\\\""),
                stopped_at
                    .map(|value| value.to_rfc3339())
                    .unwrap_or_else(|| "active".to_owned())
            ));
        }
    }

    let work_items = sqlx::query(
        r#"
        select source_kind, title, status, created_at, updated_at
        from agent_work_items
        where agent_id = $1
        order by created_at desc
        limit 5
        "#,
    )
    .bind(agent_id)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;
    if !work_items.is_empty() {
        output.push("recent_requests:".to_owned());
        for row in work_items {
            let created_at: DateTime<Utc> = row.get("created_at");
            output.push(format!(
                "- {} [{}] {} status={}",
                created_at.to_rfc3339(),
                row.get::<String, _>("source_kind"),
                compact_chars_middle(&row.get::<String, _>("title"), 120).replace('\n', " "),
                row.get::<String, _>("status")
            ));
        }
    }

    let activities = sqlx::query(
        r#"
        select phase, status, summary, created_at
        from agent_activities
        where agent_id = $1 or agent_handle = $2
        order by created_at desc
        limit 5
        "#,
    )
    .bind(agent_id)
    .bind(&handle)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;
    if !activities.is_empty() {
        output.push("recent_activity:".to_owned());
        for row in activities {
            let created_at: DateTime<Utc> = row.get("created_at");
            output.push(format!(
                "- {} {}:{} {}",
                created_at.to_rfc3339(),
                row.get::<String, _>("phase"),
                row.get::<String, _>("status"),
                compact_chars_middle(&row.get::<String, _>("summary"), 120).replace('\n', " ")
            ));
        }
    }

    Ok(output.join("\n"))
}

async fn resolve_agent_run_id(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_ref: &str,
) -> CommandResult<Uuid> {
    let run_ref = run_ref.trim();
    if run_ref.is_empty() {
        return Err("run-read requires --run-id <uuid-or-prefix>".to_owned());
    }
    if let Ok(run_id) = Uuid::parse_str(run_ref) {
        let exists: bool = sqlx::query_scalar(
            "select exists(select 1 from agent_runs where id = $1 and agent_id = $2)",
        )
        .bind(run_id)
        .bind(agent_id)
        .fetch_one(pool)
        .await
        .map_err(to_string)?;
        if exists {
            return Ok(run_id);
        }
        return Err(format!("unknown run id for this agent: {run_ref}"));
    }

    let rows = sqlx::query(
        r#"
        select id
        from agent_runs
        where agent_id = $1
        order by started_at desc
        limit 200
        "#,
    )
    .bind(agent_id)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;
    let matches = rows
        .into_iter()
        .filter_map(|row| {
            let id: Uuid = row.get("id");
            id.to_string().starts_with(run_ref).then_some(id)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [run_id] => Ok(*run_id),
        [] => Err(format!("unknown run id prefix for this agent: {run_ref}")),
        _ => Err(format!("ambiguous run id prefix for this agent: {run_ref}")),
    }
}

pub(crate) async fn agent_context_run_read(
    pool: &SqlitePool,
    args: &[String],
) -> CommandResult<String> {
    let target = resolve_agent_inbox_target(pool, args)
        .await
        .map_err(|err| err.replace("inbox commands", "run-read"))?;
    let run_ref = arg_value(args, "--run-id")
        .or_else(|| arg_value(args, "--id"))
        .ok_or_else(|| "run-read requires --run-id <uuid-or-prefix>".to_owned())?;
    let run_id = resolve_agent_run_id(pool, target.id, &run_ref).await?;
    let limit = parse_context_tool_limit(args, 8, 30)?;
    let log_limit = parse_context_tool_named_usize(args, "--log-limit", 8_000, 64_000)?;

    let row = sqlx::query(
        r#"
        select
            r.status,
            r.command,
            r.working_directory,
            r.log,
            r.input_tokens,
            r.output_tokens,
            r.cost_micros,
            r.started_at,
            r.stopped_at,
            w.id as work_item_id,
            w.source_kind,
            w.title as work_title,
            w.context as work_context,
            w.status as work_status,
            w.channel_id,
            w.thread_root_id,
            c.name as channel_name,
            c.kind as channel_kind
        from agent_runs r
        left join agent_work_items w on w.id = r.work_item_id
        left join channels c on c.id = w.channel_id
        where r.id = $1 and r.agent_id = $2
        "#,
    )
    .bind(run_id)
    .bind(target.id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;

    let started_at: DateTime<Utc> = row.get("started_at");
    let stopped_at: Option<DateTime<Utc>> = row.get("stopped_at");
    let mut output = vec![
        format!("Lantor run {} for @{}", short_id(run_id), target.handle),
        format!("run_id={run_id}"),
        format!(
            "status={} tokens={}/{} cost=${:.4}",
            row.get::<String, _>("status"),
            row.get::<i64, _>("input_tokens"),
            row.get::<i64, _>("output_tokens"),
            row.get::<i64, _>("cost_micros") as f64 / 1_000_000.0
        ),
        format!(
            "started_at={} stopped_at={}",
            started_at.to_rfc3339(),
            stopped_at
                .map(|value| value.to_rfc3339())
                .unwrap_or_else(|| "active".to_owned())
        ),
        format!(
            "command=\"{}\"",
            compact_chars_middle(&row.get::<String, _>("command"), 240).replace('"', "\\\"")
        ),
        format!(
            "working_directory={}",
            row.get::<String, _>("working_directory")
        ),
    ];

    let work_item_id: Option<Uuid> = row.get("work_item_id");
    if let Some(work_item_id) = work_item_id {
        output.push("work_item:".to_owned());
        output.push(format!(
            "- id={} source_kind={} status={} title={}",
            work_item_id,
            row.get::<String, _>("source_kind"),
            row.get::<String, _>("work_status"),
            compact_chars_middle(&row.get::<String, _>("work_title"), 240).replace('\n', " ")
        ));
        let channel_name: Option<String> = row.get("channel_name");
        let channel_kind: Option<String> = row.get("channel_kind");
        let thread_root_id: Option<Uuid> = row.get("thread_root_id");
        if let Some(channel_name) = channel_name {
            let target_label = if channel_kind.as_deref() == Some("dm") {
                format!("dm:{channel_name}")
            } else {
                format!("#{channel_name}")
            };
            let target_with_thread = if let Some(thread_root_id) = thread_root_id {
                format!("{target_label}:{}", short_id(thread_root_id))
            } else {
                target_label
            };
            output.push(format!("- surface={target_with_thread}"));
            output.push(format!(
                "history_hint=\"$LANTOR_CONTEXT_TOOL\" --agent-context-tool history-read --target {:?} --limit 30",
                target_with_thread
            ));
        }
        let context: String = row.get("work_context");
        if !context.trim().is_empty() {
            output.push("work_item_context:".to_owned());
            output.push(compact_chars_middle(&context, 4_000));
        }
    }

    let run_prefix = format!("{run_id}:%");
    let messages = sqlx::query(
        r#"
        select
            m.id,
            m.sender_name,
            m.sender_role,
            m.body,
            m.created_at,
            m.thread_root_id,
            c.name as channel_name,
            c.kind as channel_kind,
            null as task_number,
            null as task_status
        from messages m
        join channels c on c.id = m.channel_id
        where m.sender_agent_id = $1
          and m.stream_key like $2
          and trim(m.body) <> ''
          and m.lifecycle in ('streaming_final', 'committed')
        order by m.created_at asc
        limit $3
        "#,
    )
    .bind(target.id)
    .bind(&run_prefix)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;
    if !messages.is_empty() {
        output.push("visible_messages:".to_owned());
        for row in messages {
            output.push(format_context_message_row(&row, None));
        }
    }

    let activities = sqlx::query(
        r#"
        select phase, status, title, detail, created_at
        from agent_activities
        where run_id = $1
        order by created_at asc
        limit $2
        "#,
    )
    .bind(run_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;
    if !activities.is_empty() {
        output.push("activities:".to_owned());
        for row in activities {
            let created_at: DateTime<Utc> = row.get("created_at");
            let title: String = row.get("title");
            let detail: String = row.get("detail");
            let detail = if detail.trim().is_empty() {
                String::new()
            } else {
                format!(
                    ": {}",
                    compact_chars_middle(&detail, AGENT_CONTEXT_TOOL_MESSAGE_LIMIT)
                        .replace('\n', " ")
                )
            };
            output.push(format!(
                "- {} {}:{} {}{}",
                created_at.to_rfc3339(),
                row.get::<String, _>("phase"),
                row.get::<String, _>("status"),
                compact_chars_middle(&title, 160).replace('\n', " "),
                detail
            ));
        }
    }

    let log: String = row.get("log");
    if !log.trim().is_empty() && log_limit > 0 {
        output.push(format!("log_tail_chars={log_limit}:"));
        output.push(tail_chars(&log, log_limit));
    }

    Ok(output.join("\n"))
}

pub(crate) async fn agent_context_artifact_read_in_pool(
    pool: &SqlitePool,
    args: &[String],
) -> CommandResult<String> {
    let raw_id = arg_value(args, "--artifact-id")
        .or_else(|| arg_value(args, "--id"))
        .ok_or_else(|| "artifact-read requires --artifact-id <uuid>".to_owned())?;
    let artifact_id =
        Uuid::parse_str(raw_id.trim()).map_err(|err| format!("invalid artifact id: {err}"))?;
    let artifact = load_artifact(pool, artifact_id).await?;
    Ok(format!(
        "Lantor artifact {}\nkind={}\ntitle={}\nsummary={}\nmessage_id={}\nchannel_id={}\nthread_root_id={}\ncreator=@{}\nmetadata={}\n\n{}",
        artifact.id,
        artifact.kind,
        artifact.title,
        artifact.summary,
        artifact.message_id,
        artifact.channel_id,
        artifact
            .thread_root_id
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_owned()),
        artifact.creator_agent_handle.unwrap_or_else(|| "unknown".to_owned()),
        artifact.metadata,
        artifact.content
    ))
}

async fn resolve_agent_workspace_target(
    pool: &SqlitePool,
    args: &[String],
) -> CommandResult<AgentWorkspaceTarget> {
    let explicit_target = arg_value(args, "--target").or_else(|| arg_value(args, "--agent"));
    let row = if let Some(target) = explicit_target {
        let agent_id = resolve_agent_by_handle(pool, &target).await?;
        sqlx::query("select id, handle, working_directory from agents where id = $1")
            .bind(agent_id)
            .fetch_one(pool)
            .await
            .map_err(to_string)?
    } else if let Ok(agent_id) = env::var("LANTOR_AGENT_ID") {
        let agent_id = Uuid::parse_str(agent_id.trim())
            .map_err(|err| format!("invalid LANTOR_AGENT_ID: {err}"))?;
        sqlx::query("select id, handle, working_directory from agents where id = $1")
            .bind(agent_id)
            .fetch_one(pool)
            .await
            .map_err(to_string)?
    } else if let Ok(handle) = env::var("LANTOR_AGENT_HANDLE") {
        let agent_id = resolve_agent_by_handle(pool, &handle).await?;
        sqlx::query("select id, handle, working_directory from agents where id = $1")
            .bind(agent_id)
            .fetch_one(pool)
            .await
            .map_err(to_string)?
    } else {
        return Err("workspace commands require --target @handle or LANTOR_AGENT_ID".to_owned());
    };

    Ok(AgentWorkspaceTarget {
        id: row.get("id"),
        handle: row.get("handle"),
        working_directory: row.get("working_directory"),
    })
}

fn workspace_path(target: &AgentWorkspaceTarget) -> CommandResult<PathBuf> {
    let working_directory = target.working_directory.trim();
    if working_directory.is_empty() {
        return Err(format!(
            "@{} has no working_directory configured",
            target.handle
        ));
    }
    Ok(PathBuf::from(working_directory))
}

async fn resolve_agent_inbox_target(
    pool: &SqlitePool,
    args: &[String],
) -> CommandResult<AgentInboxTarget> {
    let explicit_target = arg_value(args, "--target").or_else(|| arg_value(args, "--agent"));
    let row = if let Some(target) = explicit_target {
        let agent_id = resolve_agent_by_handle(pool, &target).await?;
        sqlx::query("select id, handle from agents where id = $1")
            .bind(agent_id)
            .fetch_one(pool)
            .await
            .map_err(to_string)?
    } else if let Ok(agent_id) = env::var("LANTOR_AGENT_ID") {
        let agent_id = Uuid::parse_str(agent_id.trim())
            .map_err(|err| format!("invalid LANTOR_AGENT_ID: {err}"))?;
        sqlx::query("select id, handle from agents where id = $1")
            .bind(agent_id)
            .fetch_one(pool)
            .await
            .map_err(to_string)?
    } else if let Ok(handle) = env::var("LANTOR_AGENT_HANDLE") {
        let agent_id = resolve_agent_by_handle(pool, &handle).await?;
        sqlx::query("select id, handle from agents where id = $1")
            .bind(agent_id)
            .fetch_one(pool)
            .await
            .map_err(to_string)?
    } else {
        return Err("inbox commands require --target @handle or LANTOR_AGENT_ID".to_owned());
    };

    Ok(AgentInboxTarget {
        id: row.get("id"),
        handle: row.get("handle"),
    })
}

fn parse_inbox_states(args: &[String]) -> CommandResult<(String, Vec<String>)> {
    let state = arg_value(args, "--state").unwrap_or_else(|| "active".to_owned());
    let normalized = state.trim().to_ascii_lowercase();
    let states = match normalized.as_str() {
        "active" => vec!["unread".to_owned(), "processing".to_owned()],
        "unread" => vec!["unread".to_owned()],
        "processing" => vec!["processing".to_owned()],
        "archived" | "done" => vec!["archived".to_owned()],
        "all" => vec![
            "unread".to_owned(),
            "processing".to_owned(),
            "archived".to_owned(),
        ],
        other => {
            return Err(format!(
                "invalid inbox --state {other:?}; expected active, unread, processing, archived, or all"
            ));
        }
    };
    Ok((normalized, states))
}

fn inbox_surface_label(
    channel_name: Option<&str>,
    channel_kind: Option<&str>,
    thread_root_id: Option<Uuid>,
) -> String {
    match (channel_kind, channel_name, thread_root_id) {
        (Some("dm"), Some(name), Some(thread_root_id)) => {
            format!("dm:{name}:{}", short_id(thread_root_id))
        }
        (Some("dm"), Some(name), None) => format!("dm:{name}"),
        (_, Some(name), Some(thread_root_id)) => format!("#{name}:{}", short_id(thread_root_id)),
        (_, Some(name), None) => format!("#{name}"),
        _ => "unknown".to_owned(),
    }
}

fn inbox_history_target(channel_id: Uuid, thread_root_id: Option<Uuid>) -> String {
    if let Some(thread_root_id) = thread_root_id {
        format!("{channel_id}:{}", short_id(thread_root_id))
    } else {
        channel_id.to_string()
    }
}

async fn resolve_inbox_item_id(
    pool: &SqlitePool,
    agent_id: Uuid,
    raw_id: &str,
) -> CommandResult<Uuid> {
    let raw_id = raw_id.trim().trim_start_matches("inbox:");
    if raw_id.is_empty() {
        return Err("inbox id is empty".to_owned());
    }
    if let Ok(inbox_id) = Uuid::parse_str(raw_id) {
        let exists: Option<Uuid> =
            sqlx::query_scalar("select id from agent_inbox_items where id = $1 and agent_id = $2")
                .bind(inbox_id)
                .bind(agent_id)
                .fetch_optional(pool)
                .await
                .map_err(to_string)?;
        return exists.ok_or_else(|| format!("inbox item {inbox_id} is not visible to this agent"));
    }

    let rows = sqlx::query_scalar::<_, Uuid>(
        r#"
        select id
        from agent_inbox_items
        where agent_id = $1 and lower(hex(id)) like replace(lower($2), '-', '')
        order by created_at desc
        limit 2
        "#,
    )
    .bind(agent_id)
    .bind(format!("{raw_id}%"))
    .fetch_all(pool)
    .await
    .map_err(to_string)?;
    match rows.as_slice() {
        [id] => Ok(*id),
        [] => Err(format!("unknown inbox item id prefix: {raw_id}")),
        _ => Err(format!("ambiguous inbox item id prefix: {raw_id}")),
    }
}

async fn notify_context_tool_refresh(pool: &SqlitePool, reason: &str) {
    let _ = sqlx::query("insert into ui_events (event_json) values ($1)")
        .bind(serde_json::json!({ "type": "refresh", "reason": reason }).to_string())
        .execute(pool)
        .await;
}

fn memory_path(workspace: &Path) -> PathBuf {
    workspace.join("MEMORY.md")
}

fn file_summary(path: &Path) -> String {
    match fs::metadata(path) {
        Ok(metadata) => {
            if metadata.is_dir() {
                "dir".to_owned()
            } else if metadata.is_file() {
                format!("file bytes={}", metadata.len())
            } else {
                "other".to_owned()
            }
        }
        Err(err) => format!("missing error={}", err),
    }
}

fn should_skip_workspace_entry(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    matches!(
        name,
        ".git" | "node_modules" | "target" | "dist" | ".next" | ".turbo"
    )
}

fn collect_workspace_entries(
    root: &Path,
    current: &Path,
    depth: usize,
    max_depth: usize,
    limit: usize,
    entries: &mut Vec<String>,
) {
    if entries.len() >= limit || depth > max_depth {
        return;
    }
    let Ok(read_dir) = fs::read_dir(current) else {
        return;
    };
    let mut children = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    children.sort_by(|left, right| {
        left.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .cmp(
                right
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default(),
            )
    });
    for child in children {
        if entries.len() >= limit {
            return;
        }
        if should_skip_workspace_entry(&child) {
            continue;
        }
        let relative = child.strip_prefix(root).unwrap_or(&child);
        let is_dir = child.is_dir();
        entries.push(format!(
            "- {}{} ({})",
            relative.display(),
            if is_dir { "/" } else { "" },
            file_summary(&child)
        ));
        if is_dir && depth < max_depth {
            collect_workspace_entries(root, &child, depth + 1, max_depth, limit, entries);
        }
    }
}

pub(crate) async fn agent_context_workspace_info(
    pool: &SqlitePool,
    args: &[String],
) -> CommandResult<String> {
    let target = resolve_agent_workspace_target(pool, args).await?;
    let workspace = workspace_path(&target)?;
    let memory = memory_path(&workspace);
    let cwd = env::current_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|err| format!("unavailable: {err}"));
    let mut output = vec![
        format!("Lantor workspace for @{}", target.handle),
        format!("agent_id={}", target.id),
        format!("working_directory=\"{}\"", workspace.display()),
        format!("context_tool_cwd=\"{}\"", cwd.replace('"', "\\\"")),
        format!("workspace_exists={}", workspace.exists()),
        format!("workspace_kind={}", file_summary(&workspace)),
        format!("memory_path=\"{}\"", memory.display()),
        format!("memory_exists={}", memory.exists()),
        format!("memory_kind={}", file_summary(&memory)),
    ];

    if workspace.exists() && workspace.is_dir() {
        let mut entries = Vec::new();
        collect_workspace_entries(&workspace, &workspace, 1, 1, 12, &mut entries);
        if !entries.is_empty() {
            output.push("top_level_entries:".to_owned());
            output.extend(entries);
        }
    }
    Ok(output.join("\n"))
}

pub(crate) async fn agent_context_memory_read(
    pool: &SqlitePool,
    args: &[String],
) -> CommandResult<String> {
    let target = resolve_agent_workspace_target(pool, args).await?;
    let workspace = workspace_path(&target)?;
    let memory = memory_path(&workspace);
    if !memory.exists() {
        return Ok(format!(
            "Lantor MEMORY.md for @{}\nmemory_path=\"{}\"\nmemory_exists=false",
            target.handle,
            memory.display()
        ));
    }
    let metadata = fs::metadata(&memory).map_err(to_string)?;
    if !metadata.is_file() {
        return Err(format!(
            "MEMORY.md is not a regular file: {}",
            memory.display()
        ));
    }
    let limit = parse_context_tool_usize_limit(args, 16 * 1024, 64 * 1024)?;
    let body = fs::read_to_string(&memory).map_err(to_string)?;
    let compacted = compact_chars_middle(body.trim(), limit);
    Ok(format!(
        "Lantor MEMORY.md for @{}\nmemory_path=\"{}\"\nbytes={}\nchars_returned={}\n\n{}",
        target.handle,
        memory.display(),
        metadata.len(),
        compacted.chars().count(),
        compacted
    ))
}

pub(crate) async fn agent_context_workspace_list(
    pool: &SqlitePool,
    args: &[String],
) -> CommandResult<String> {
    let target = resolve_agent_workspace_target(pool, args).await?;
    let workspace = workspace_path(&target)?;
    if !workspace.exists() {
        return Ok(format!(
            "Lantor workspace list for @{}\nworking_directory=\"{}\"\nworkspace_exists=false",
            target.handle,
            workspace.display()
        ));
    }
    if !workspace.is_dir() {
        return Err(format!(
            "working_directory is not a directory: {}",
            workspace.display()
        ));
    }
    let limit = parse_context_tool_usize_limit(args, 80, 500)?;
    let max_depth = arg_value(args, "--max-depth")
        .map(|value| {
            value
                .parse::<usize>()
                .map(|parsed| parsed.clamp(1, 5))
                .map_err(|_| format!("invalid --max-depth value: {value}"))
        })
        .transpose()?
        .unwrap_or(2);
    let mut entries = Vec::new();
    collect_workspace_entries(&workspace, &workspace, 1, max_depth, limit, &mut entries);
    let mut output = vec![format!(
        "Lantor workspace list for @{}\nworking_directory=\"{}\"\nmax_depth={} limit={} returned={}",
        target.handle,
        workspace.display(),
        max_depth,
        limit,
        entries.len()
    )];
    output.extend(entries);
    Ok(output.join("\n"))
}

pub(crate) async fn agent_context_inbox_list(
    pool: &SqlitePool,
    args: &[String],
) -> CommandResult<String> {
    let target = resolve_agent_inbox_target(pool, args).await?;
    let (state_label, states) = parse_inbox_states(args)?;
    let limit = parse_context_tool_limit(args, 20, 100)?;
    let state_placeholders = (0..states.len())
        .map(|index| format!("${}", index + 2))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        r#"
        select
            i.id,
            i.kind,
            i.priority,
            i.state,
            i.title,
            i.body_preview,
            i.channel_id,
            c.name as channel_name,
            c.kind as channel_kind,
            i.thread_root_id,
            i.source_message_id,
            i.task_id,
            i.work_item_id,
            i.created_at,
            i.updated_at
        from agent_inbox_items i
        left join channels c on c.id = i.channel_id
        where i.agent_id = $1
          and i.state in ({state_placeholders})
        order by
            case i.state when 'processing' then 0 when 'unread' then 1 else 2 end,
            i.priority desc,
            i.created_at asc
        limit ${limit_param}
        "#,
        limit_param = states.len() + 2
    );
    let mut query = sqlx::query(&sql).bind(target.id);
    for state in &states {
        query = query.bind(state);
    }
    let rows = query.bind(limit).fetch_all(pool).await.map_err(to_string)?;

    let mut output = vec![format!(
        "Lantor inbox for @{}\nstate={} limit={} returned={}",
        target.handle,
        state_label,
        limit,
        rows.len()
    )];
    if rows.is_empty() {
        output.push("No matching inbox items.".to_owned());
        return Ok(output.join("\n"));
    }

    for row in rows {
        let id: Uuid = row.get("id");
        let created_at: DateTime<Utc> = row.get("created_at");
        let channel_name: Option<String> = row.get("channel_name");
        let channel_kind: Option<String> = row.get("channel_kind");
        let thread_root_id: Option<Uuid> = row.get("thread_root_id");
        let surface = inbox_surface_label(
            channel_name.as_deref(),
            channel_kind.as_deref(),
            thread_root_id,
        );
        let preview = compact_chars_middle(row.get::<String, _>("body_preview").trim(), 240)
            .replace('\n', "\n  ");
        let mut line = format!(
            "- {} kind={} state={} priority={} target={} created={} title={:?}",
            short_id(id),
            row.get::<String, _>("kind"),
            row.get::<String, _>("state"),
            row.get::<i32, _>("priority"),
            surface,
            created_at.to_rfc3339(),
            row.get::<String, _>("title")
        );
        if let Some(source_message_id) = row.get::<Option<Uuid>, _>("source_message_id") {
            line.push_str(&format!(" source_message={}", short_id(source_message_id)));
        }
        if let Some(task_id) = row.get::<Option<Uuid>, _>("task_id") {
            line.push_str(&format!(" task_id={}", short_id(task_id)));
        }
        if let Some(work_item_id) = row.get::<Option<Uuid>, _>("work_item_id") {
            line.push_str(&format!(" work_item={}", short_id(work_item_id)));
        }
        if !preview.is_empty() {
            line.push_str(&format!("\n  preview: {preview}"));
        }
        line.push_str(&format!(
            "\n  read: \"$LANTOR_CONTEXT_TOOL\" --agent-context-tool inbox-read --inbox-id {}",
            short_id(id)
        ));
        output.push(line);
    }

    Ok(output.join("\n"))
}

pub(crate) async fn agent_context_inbox_read(
    pool: &SqlitePool,
    args: &[String],
) -> CommandResult<String> {
    let target = resolve_agent_inbox_target(pool, args).await?;
    let raw_id = arg_value(args, "--inbox-id")
        .or_else(|| arg_value(args, "--id"))
        .ok_or_else(|| "inbox-read requires --inbox-id <uuid-or-prefix>".to_owned())?;
    let inbox_id = resolve_inbox_item_id(pool, target.id, &raw_id).await?;
    sqlx::query(
        r#"
        update agent_inbox_items
        set state = case when state = 'archived' then state else 'processing' end,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1 and agent_id = $2
        "#,
    )
    .bind(inbox_id)
    .bind(target.id)
    .execute(pool)
    .await
    .map_err(to_string)?;
    notify_context_tool_refresh(pool, "inbox_read").await;

    let row = sqlx::query(
        r#"
        select
            i.id,
            i.kind,
            i.priority,
            i.state,
            i.title,
            i.body_preview,
            i.payload,
            i.channel_id,
            c.name as channel_name,
            c.kind as channel_kind,
            i.thread_root_id,
            i.source_message_id,
            i.task_id,
            i.work_item_id,
            i.created_at,
            i.updated_at,
            m.sender_name as source_sender_name,
            m.sender_role as source_sender_role,
            m.body as source_body,
            m.created_at as source_created_at
        from agent_inbox_items i
        left join channels c on c.id = i.channel_id
        left join messages m on m.id = i.source_message_id
        where i.id = $1 and i.agent_id = $2
        "#,
    )
    .bind(inbox_id)
    .bind(target.id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;

    let channel_name: Option<String> = row.get("channel_name");
    let channel_kind: Option<String> = row.get("channel_kind");
    let thread_root_id: Option<Uuid> = row.get("thread_root_id");
    let surface = inbox_surface_label(
        channel_name.as_deref(),
        channel_kind.as_deref(),
        thread_root_id,
    );
    let payload: Value = row.get("payload");
    let payload = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_owned());
    let body_preview = compact_chars_middle(row.get::<String, _>("body_preview").trim(), 1200);
    let mut output = vec![
        format!("Lantor inbox item {}", row.get::<Uuid, _>("id")),
        format!("agent=@{}", target.handle),
        format!("short_id={}", short_id(inbox_id)),
        format!("kind={}", row.get::<String, _>("kind")),
        format!("state={}", row.get::<String, _>("state")),
        format!("priority={}", row.get::<i32, _>("priority")),
        format!("target={surface}"),
        format!("title={:?}", row.get::<String, _>("title")),
        format!(
            "created_at={}",
            row.get::<DateTime<Utc>, _>("created_at").to_rfc3339()
        ),
    ];
    if let Some(channel_id) = row.get::<Option<Uuid>, _>("channel_id") {
        output.push(format!("channel_id={channel_id}"));
    }
    if let Some(thread_root_id) = thread_root_id {
        output.push(format!("thread_root_id={thread_root_id}"));
    }
    if let Some(source_message_id) = row.get::<Option<Uuid>, _>("source_message_id") {
        output.push(format!("source_message_id={source_message_id}"));
    }
    if let Some(task_id) = row.get::<Option<Uuid>, _>("task_id") {
        output.push(format!("task_id={task_id}"));
    }
    if let Some(work_item_id) = row.get::<Option<Uuid>, _>("work_item_id") {
        output.push(format!("work_item_id={work_item_id}"));
    }
    if !body_preview.is_empty() {
        output.push(format!("preview:\n{body_preview}"));
    }
    if let Some(source_body) = row.get::<Option<String>, _>("source_body") {
        let source_sender = row
            .get::<Option<String>, _>("source_sender_name")
            .unwrap_or_else(|| "unknown".to_owned());
        let source_role = row
            .get::<Option<String>, _>("source_sender_role")
            .unwrap_or_else(|| "unknown".to_owned());
        let source_created = row
            .get::<Option<DateTime<Utc>>, _>("source_created_at")
            .map(|created| created.to_rfc3339())
            .unwrap_or_else(|| "unknown".to_owned());
        output.push(format!(
            "source_message:\n  sender={}({})\n  created_at={}\n  body:\n{}",
            source_sender,
            source_role,
            source_created,
            compact_chars_middle(source_body.trim(), AGENT_CONTEXT_TOOL_MESSAGE_LIMIT)
        ));
    }
    output.push(format!("payload:\n{payload}"));
    if let Some(channel_id) = row.get::<Option<Uuid>, _>("channel_id") {
        let history_target = inbox_history_target(channel_id, thread_root_id);
        output.push(format!(
            "history_hint=\"$LANTOR_CONTEXT_TOOL\" --agent-context-tool history-read --target {:?} --limit 30",
            history_target
        ));
    } else if surface != "unknown" {
        output.push(format!(
            "history_hint=\"$LANTOR_CONTEXT_TOOL\" --agent-context-tool history-read --target {:?} --limit 30",
            surface
        ));
    }
    if row.get::<Option<Uuid>, _>("work_item_id").is_some() {
        output.push(
            "archive_note=linked work-item inbox items are archived automatically when the work item finishes; do not call inbox-archive for normal completion".to_owned(),
        );
    } else {
        output.push(format!(
            "archive_if_cleared=\"$LANTOR_CONTEXT_TOOL\" --agent-context-tool inbox-archive --inbox-id {}",
            short_id(inbox_id)
        ));
    }

    Ok(output.join("\n\n"))
}

pub(crate) async fn agent_context_inbox_archive(
    pool: &SqlitePool,
    args: &[String],
) -> CommandResult<String> {
    let target = resolve_agent_inbox_target(pool, args).await?;
    let raw_id = arg_value(args, "--inbox-id")
        .or_else(|| arg_value(args, "--id"))
        .ok_or_else(|| "inbox-archive requires --inbox-id <uuid-or-prefix>".to_owned())?;
    let inbox_id = resolve_inbox_item_id(pool, target.id, &raw_id).await?;
    let row = sqlx::query(
        r#"
        update agent_inbox_items
        set state = 'archived',
            archived_at = coalesce(archived_at, strftime('%Y-%m-%dT%H:%M:%f+00:00','now')),
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where id = $1 and agent_id = $2
        returning id, kind, title
        "#,
    )
    .bind(inbox_id)
    .bind(target.id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;
    notify_context_tool_refresh(pool, "inbox_archived").await;
    Ok(format!(
        "Archived Lantor inbox item {} for @{} kind={} title={:?}",
        short_id(row.get("id")),
        target.handle,
        row.get::<String, _>("kind"),
        row.get::<String, _>("title")
    ))
}

pub(crate) async fn run_agent_context_tool(args: &[String]) -> CommandResult<String> {
    if args.is_empty() || has_arg(args, "--help") || has_arg(args, "-h") {
        return Ok(
            "Lantor agent context tool\n\nCommands:\n  inbox-list [--state active|unread|processing|archived|all] [--limit 20]\n  inbox-read --inbox-id <uuid-or-prefix>\n  inbox-archive --inbox-id <uuid-or-prefix>\n  workspace-info [--target @handle]\n  workspace-list [--target @handle] [--max-depth 2] [--limit 80]\n  memory-read [--target @handle] [--limit 16000]\n  run-read --run-id <uuid-or-prefix> [--target @handle] [--limit 8] [--log-limit 8000]\n  history-read --target \"#channel[:thread]\" [--limit 30]\n  message-search --query <text> [--target \"#channel\"] [--limit 30]\n  attachment-info --attachment-id <uuid>\n  artifact-read --artifact-id <uuid>\n  agent-inspect --target @handle\n\nTargets may be #channel, #channel:<message-id-prefix>, dm:@agent, channel UUID, or channel UUID:<message-id-prefix>. Inbox, run, workspace, and memory commands default to the current LANTOR_AGENT_ID when invoked by an agent."
                .to_owned(),
        );
    }

    let pool = db_connect(2).await.map_err(to_string)?;
    match args[0].as_str() {
        "inbox-list" | "inbox" | "inbox-check" => agent_context_inbox_list(&pool, args).await,
        "inbox-read" | "read-inbox" => agent_context_inbox_read(&pool, args).await,
        "inbox-archive" | "archive-inbox" | "inbox-done" => {
            agent_context_inbox_archive(&pool, args).await
        }
        "workspace-info" | "workspace" | "self-workspace" => {
            agent_context_workspace_info(&pool, args).await
        }
        "workspace-list" | "list-workspace" | "workspace-ls" => {
            agent_context_workspace_list(&pool, args).await
        }
        "memory-read" | "read-memory" | "memory" => agent_context_memory_read(&pool, args).await,
        "run-read" | "read-run" | "run" => agent_context_run_read(&pool, args).await,
        "history-read" | "read-history" | "read" => agent_context_history_read(&pool, args).await,
        "message-search" | "search-messages" | "search" => {
            agent_context_message_search(&pool, args).await
        }
        "attachment-info" | "attachment" | "attachment-view" => {
            agent_context_attachment_info(&pool, args).await
        }
        "agent-inspect" | "inspect-agent" | "agent-query" => {
            agent_context_agent_inspect(&pool, args).await
        }
        "artifact-read" | "artifact" | "artifact-view" => {
            agent_context_artifact_read_in_pool(&pool, args).await
        }
        other => Err(format!("unknown agent context tool command: {other}")),
    }
}

#[cfg(test)]
#[path = "tests/context_tool.rs"]
mod relocated_tests;
