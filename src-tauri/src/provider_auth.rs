//! Login health of the provider CLIs (`claude`, `codex`).
//!
//! Every agent of a runtime shares one CLI login. When a turn fails because
//! that login expired, Lantor puts the work item back in the queue, stops
//! starting new turns for the runtime (`needs_login`) and raises one system
//! decision card, which also reaches Needs you and web push. Answering the
//! card, or the periodic retry, moves the runtime to `probing`: one turn may
//! start, and its success reopens dispatch for every agent of the runtime.

use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::app::{to_string, CommandResult};
use crate::decision_store::{close_system_decision, create_system_decision_in_tx};
use crate::models::DecisionOption;
use crate::ui_notifications::insert_system_message;

const STATUS_OK: &str = "ok";
const STATUS_NEEDS_LOGIN: &str = "needs_login";
const STATUS_PROBING: &str = "probing";
/// A paused runtime tries one turn again on its own this often, so agents
/// resume after a login even when nobody answers the card.
pub(crate) const AUTO_PROBE_INTERVAL_SECS: i64 = 15 * 60;

fn normalized_runtime(runtime: &str) -> Option<&'static str> {
    if runtime.eq_ignore_ascii_case("claude") {
        Some("claude")
    } else if runtime.eq_ignore_ascii_case("codex") {
        Some("codex")
    } else {
        None
    }
}

fn provider_label(runtime: &str) -> &'static str {
    match runtime {
        "claude" => "Claude",
        _ => "Codex",
    }
}

fn login_command(runtime: &str) -> &'static str {
    match runtime {
        "claude" => "claude auth login",
        _ => "codex login",
    }
}

/// Whether a new turn may start for an agent on `runtime`. A paused runtime
/// starts nothing; a probing one starts a single turn at a time.
pub(crate) async fn provider_allows_start(
    pool: &SqlitePool,
    runtime: Option<&str>,
) -> CommandResult<bool> {
    let Some(runtime) = runtime.and_then(normalized_runtime) else {
        return Ok(true);
    };
    let status: Option<String> =
        sqlx::query_scalar("select status from provider_auth_state where runtime = $1")
            .bind(runtime)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?;
    match status.as_deref() {
        None | Some(STATUS_OK) => Ok(true),
        Some(STATUS_PROBING) => Ok(!runtime_has_turn_in_flight(pool, runtime).await?),
        Some(_) => Ok(false),
    }
}

/// Whether the runtime's login is known to be broken right now. Start
/// commands that were queued before the failure skip their turn.
pub(crate) async fn provider_needs_login(
    pool: &SqlitePool,
    runtime: Option<&str>,
) -> CommandResult<bool> {
    let Some(runtime) = runtime.and_then(normalized_runtime) else {
        return Ok(false);
    };
    let status: Option<String> =
        sqlx::query_scalar("select status from provider_auth_state where runtime = $1")
            .bind(runtime)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?;
    Ok(status.as_deref() == Some(STATUS_NEEDS_LOGIN))
}

async fn runtime_has_turn_in_flight(pool: &SqlitePool, runtime: &str) -> CommandResult<bool> {
    sqlx::query_scalar(
        r#"
        select exists(
            select 1
            from agent_runs r
            join agents a on a.id = r.agent_id
            where lower(a.runtime) = $1
              and r.stopped_at is null
              and r.status in ('starting', 'running', 'stopping')
        ) or exists(
            select 1
            from supervisor_commands c
            join agents a on a.id = c.agent_id
            where lower(a.runtime) = $1
              and c.command_type = 'start_agent'
              and c.status in ('pending', 'running')
        )
        "#,
    )
    .bind(runtime)
    .fetch_one(pool)
    .await
    .map_err(to_string)
}

/// A turn on `runtime` failed because the CLI login is no longer valid.
/// Pauses the runtime and raises a card unless this outage already has one
/// open, or the owner dismissed it.
pub(crate) async fn record_provider_auth_failure(
    pool: &SqlitePool,
    runtime: &str,
    channel_id: Option<Uuid>,
    detail: &str,
) -> CommandResult<()> {
    let Some(runtime) = normalized_runtime(runtime) else {
        return Ok(());
    };
    let detail = detail.trim();
    // One transaction decides whether this failure raises the card, so agents
    // failing in the same second share a single card.
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let decision_status: Option<String> = sqlx::query_scalar(
        r#"
        select d.status
        from provider_auth_state p
        join decisions d on d.id = p.decision_id
        where p.runtime = $1
        "#,
    )
    .bind(runtime)
    .fetch_optional(&mut *tx)
    .await
    .map_err(to_string)?;
    sqlx::query(
        r#"
        insert into provider_auth_state (runtime, status, detail, paused_at, updated_at)
        values (
            $1, $2, $3,
            strftime('%Y-%m-%dT%H:%M:%f+00:00','now'),
            strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        )
        on conflict (runtime) do update set
            status = excluded.status,
            detail = excluded.detail,
            paused_at = excluded.paused_at,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(runtime)
    .bind(STATUS_NEEDS_LOGIN)
    .bind(detail)
    .execute(&mut *tx)
    .await
    .map_err(to_string)?;
    // Open: already asked. Dismissed: the owner chose to handle it alone.
    // Answered: the owner said they logged in, yet it still fails, so ask again.
    let raise_card = !matches!(decision_status.as_deref(), Some("open" | "dismissed"));
    let card_channel = if raise_card {
        card_channel_in_tx(&mut tx, channel_id).await?
    } else {
        None
    };
    if let Some(card_channel) = card_channel {
        let agents = paused_agent_handles_in_tx(&mut tx, runtime).await?;
        let (title, context, options) = login_card(
            runtime,
            &agents,
            detail,
            decision_status.as_deref() == Some("answered"),
        );
        let (decision_id, _) =
            create_system_decision_in_tx(&mut tx, card_channel, &title, &context, options).await?;
        sqlx::query("update provider_auth_state set decision_id = $2 where runtime = $1")
            .bind(runtime)
            .bind(decision_id)
            .execute(&mut *tx)
            .await
            .map_err(to_string)?;
    }
    tx.commit().await.map_err(to_string)
}

fn login_card(
    runtime: &str,
    agents: &[String],
    detail: &str,
    asked_before: bool,
) -> (String, String, Vec<DecisionOption>) {
    let label = provider_label(runtime);
    let still = if asked_before { " still" } else { "" };
    let title = format!("{label} login{still} needs attention — its agents are paused");
    let mut context = format!(
        "Lantor stopped starting {label} turns because the {label} CLI login is no longer valid."
    );
    if !agents.is_empty() {
        context.push_str(&format!(" Paused agents: {}.", agents.join(", ")));
    }
    context.push_str(&format!(
        " Their queued requests stay queued; none is marked failed.\n\nLog in again on this Mac with `{}`, then choose the option below. Lantor also retries one request on its own every {} minutes.",
        login_command(runtime),
        AUTO_PROBE_INTERVAL_SECS / 60
    ));
    if !detail.is_empty() {
        context.push_str(&format!("\n\nLast error: {detail}"));
    }
    let options = vec![DecisionOption {
        id: "a".to_owned(),
        label: "I logged in again — resume the agents".to_owned(),
        detail: "Lantor first runs one queued request. If it succeeds every paused agent resumes; if the login still fails they stay paused and a new card arrives.".to_owned(),
        recommended: true,
    }];
    (title, context, options)
}

/// The card goes to `#all` when it exists, else where the failure happened,
/// else the oldest channel.
async fn card_channel_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    failed_channel_id: Option<Uuid>,
) -> CommandResult<Option<Uuid>> {
    let all: Option<Uuid> =
        sqlx::query_scalar("select id from channels where name = 'all' and kind = 'channel'")
            .fetch_optional(&mut **tx)
            .await
            .map_err(to_string)?;
    if all.is_some() {
        return Ok(all);
    }
    if failed_channel_id.is_some() {
        return Ok(failed_channel_id);
    }
    sqlx::query_scalar("select id from channels where kind = 'channel' order by created_at limit 1")
        .fetch_optional(&mut **tx)
        .await
        .map_err(to_string)
}

async fn paused_agent_handles_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    runtime: &str,
) -> CommandResult<Vec<String>> {
    sqlx::query_scalar("select handle from agents where lower(runtime) = $1 order by handle")
        .bind(runtime)
        .fetch_all(&mut **tx)
        .await
        .map_err(to_string)
}

/// A turn on `runtime` completed. If it started after the latest login
/// failure, the login works again: reopen dispatch and close the card.
pub(crate) async fn record_provider_turn_success(
    pool: &SqlitePool,
    runtime: &str,
    run_id: Uuid,
) -> CommandResult<()> {
    let Some(runtime) = normalized_runtime(runtime) else {
        return Ok(());
    };
    let Some(row) = sqlx::query(
        "select status, decision_id, paused_at from provider_auth_state where runtime = $1",
    )
    .bind(runtime)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?
    else {
        return Ok(());
    };
    let status: String = row.get("status");
    if status == STATUS_OK {
        return Ok(());
    }
    let decision_id: Option<Uuid> = row.get("decision_id");
    let paused_at: Option<String> = row.get("paused_at");
    // Only a turn that started after the failure proves the login works; one
    // that was already running may have finished on an older token.
    let recovered = sqlx::query(
        r#"
        update provider_auth_state
        set status = 'ok',
            detail = '',
            paused_at = null,
            decision_id = null,
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where runtime = $1
          and status <> 'ok'
          and paused_at is $3
          and exists (
              select 1 from agent_runs r
              where r.id = $2 and ($3 is null or r.started_at > $3)
          )
        "#,
    )
    .bind(runtime)
    .bind(run_id)
    .bind(&paused_at)
    .execute(pool)
    .await
    .map_err(to_string)?
    .rows_affected()
        > 0;
    if !recovered {
        return Ok(());
    }
    let Some(decision_id) = decision_id else {
        return Ok(());
    };
    close_system_decision(pool, decision_id).await?;
    let card: Option<(Uuid, Uuid)> =
        sqlx::query("select channel_id, message_id from decisions where id = $1")
            .bind(decision_id)
            .fetch_optional(pool)
            .await
            .map_err(to_string)?
            .map(|row| (row.get("channel_id"), row.get("message_id")));
    if let Some((channel_id, message_id)) = card {
        insert_system_message(
            pool,
            channel_id,
            Some(message_id),
            format!(
                "{} login works again; paused agents resumed their queued requests.",
                provider_label(runtime)
            ),
        )
        .await?;
    }
    Ok(())
}

/// The owner answered a Lantor card. If it is the login card of a paused
/// runtime, let one turn try the login again.
pub(crate) async fn resume_after_login_answer(
    pool: &SqlitePool,
    decision_id: Uuid,
) -> CommandResult<()> {
    sqlx::query(
        r#"
        update provider_auth_state
        set status = 'probing',
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where decision_id = $1 and status = 'needs_login'
        "#,
    )
    .bind(decision_id)
    .execute(pool)
    .await
    .map_err(to_string)?;
    Ok(())
}

/// Paused runtimes that waited long enough try one turn again.
pub(crate) async fn start_due_provider_probes(pool: &SqlitePool) -> CommandResult<()> {
    sqlx::query(
        r#"
        update provider_auth_state
        set status = 'probing',
            updated_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
        where status = 'needs_login'
          and updated_at < strftime('%Y-%m-%dT%H:%M:%f+00:00','now', $1)
        "#,
    )
    .bind(format!("-{AUTO_PROBE_INTERVAL_SECS} seconds"))
    .execute(pool)
    .await
    .map_err(to_string)?;
    Ok(())
}

#[cfg(test)]
#[path = "tests/provider_auth.rs"]
mod tests;
