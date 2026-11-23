//! What an agent, and the provider account it runs on, has spent recently.
//!
//! Accounts without usage windows (API keys, enterprise or credit-based plans)
//! show this instead of "percent left". The figures add up each run's
//! `cost_micros`, which is a list-price estimate (`usage.rs`), not the bill.

use serde::Serialize;
use sqlx::{sqlite::SqliteRow, Row, SqlitePool};
use uuid::Uuid;

use crate::app::{to_string, CommandResult};

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct SpendTotals {
    pub(crate) last_day_micros: i64,
    pub(crate) last_week_micros: i64,
    pub(crate) last_month_micros: i64,
    pub(crate) runs_last_month: i64,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct AccountSpend {
    /// Agents whose latest usage snapshot names the same provider account.
    pub(crate) agent_count: i64,
    pub(crate) totals: SpendTotals,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct AgentSpend {
    pub(crate) agent: SpendTotals,
    pub(crate) account: Option<AccountSpend>,
}

/// Runs in the last 30 days of the agents the caller's `where` clause picks.
const SPEND_SINCE_LAST_MONTH: &str = r#"
    select
        coalesce(sum(case when started_at >= strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now', '-1 day')
                          then cost_micros end), 0) as last_day,
        coalesce(sum(case when started_at >= strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now', '-7 days')
                          then cost_micros end), 0) as last_week,
        coalesce(sum(cost_micros), 0) as last_month,
        count(*) as runs
    from agent_runs
    where started_at >= strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now', '-30 days')
"#;

pub(crate) async fn load_agent_spend(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<AgentSpend> {
    let row = sqlx::query(&format!("{SPEND_SINCE_LAST_MONTH} and agent_id = $1"))
        .bind(agent_id)
        .fetch_one(pool)
        .await
        .map_err(to_string)?;
    let agent = spend_totals(&row);

    let account = sqlx::query(
        "select provider, account_id from agent_subscription_status where agent_id = $1 and account_id is not null",
    )
    .bind(agent_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    let Some(account) = account else {
        return Ok(AgentSpend {
            agent,
            account: None,
        });
    };
    let provider: String = account.get("provider");
    let account_id: String = account.get("account_id");
    let agent_count: i64 = sqlx::query_scalar(
        "select count(*) from agent_subscription_status where provider = $1 and account_id = $2",
    )
    .bind(&provider)
    .bind(&account_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;
    let row = sqlx::query(&format!(
        "{SPEND_SINCE_LAST_MONTH} and agent_id in \
         (select agent_id from agent_subscription_status where provider = $1 and account_id = $2)"
    ))
    .bind(&provider)
    .bind(&account_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;
    Ok(AgentSpend {
        agent,
        account: Some(AccountSpend {
            agent_count,
            totals: spend_totals(&row),
        }),
    })
}

fn spend_totals(row: &SqliteRow) -> SpendTotals {
    SpendTotals {
        last_day_micros: row.get("last_day"),
        last_week_micros: row.get("last_week"),
        last_month_micros: row.get("last_month"),
        runs_last_month: row.get("runs"),
    }
}

#[cfg(test)]
#[path = "../tests/application_spend.rs"]
mod tests;
