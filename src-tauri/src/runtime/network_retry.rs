//! Retrying a turn that ended because the connection to the provider failed.
//!
//! The request goes back to the queue with a delay and the next turn continues
//! it in the same session. Only failures on a Mac that is really awake count
//! toward the limit. With the lid closed it only has dark wakes, a few seconds
//! about every 17 minutes, and a request that runs in one is cut off by the
//! next sleep. So a retry waits for a real wake, and a failure caused by sleep
//! costs nothing. When the limit is reached the request fails and the owner
//! gets a card. See docs/agent-runtime.md#run-lifecycle.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};
use tokio::process::Command;
use uuid::Uuid;

use crate::agent_work_dispatch::retry_agent_work_in_pool;
use crate::app::{to_string, CommandResult};
use crate::decision_store::{create_system_decision_in_tx, notify_decision_changed};
use crate::models::DecisionOption;
use crate::provider_auth::card_channel_in_tx;

/// Delay after each counted failure. The request is retried once per entry;
/// the failure after the last one reaches the owner.
const BACKOFF_SECS: [i64; 5] = [60, 180, 300, 600, 900];

/// A failure that sleep caused says nothing about the network, so it costs
/// nothing and waits the shortest delay.
const UNCOUNTED_DELAY_SECS: i64 = BACKOFF_SECS[0];

/// A turn's monotonic clock stops while the Mac sleeps and its wall clock does
/// not. On recorded runs that did not sleep the two differ by well under a
/// second, so this is far above noise.
const SLEEP_GAP: Duration = Duration::from_secs(30);

const PMSET_TIMEOUT: Duration = Duration::from_secs(2);

const NETWORK_RETRY_NOTE: &str = "Lantor note: the connection to the model dropped during your previous attempt at this request, and this turn continues it. Carry on from where this conversation stopped. Commands you ran, files you changed and LANTOR_EVENT lines you wrote in that attempt already took effect, so do not repeat them. Its visible text was discarded, so write your whole reply again.";

pub(crate) fn with_network_retry_note(prompt: String) -> String {
    format!("{NETWORK_RETRY_NOTE}\n\n{prompt}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NetworkRetry {
    /// Counted failures of the request so far, this one included if it counts.
    pub(crate) failures: i64,
    pub(crate) delay_secs: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NetworkRetryPlan {
    Retry(NetworkRetry),
    GiveUp { failures: i64 },
}

impl NetworkRetryPlan {
    pub(crate) fn retry(&self) -> Option<&NetworkRetry> {
        match self {
            Self::Retry(retry) => Some(retry),
            Self::GiveUp { .. } => None,
        }
    }
}

fn next_network_retry(failures_so_far: i64, counted: bool) -> NetworkRetryPlan {
    if !counted {
        return NetworkRetryPlan::Retry(NetworkRetry {
            failures: failures_so_far,
            delay_secs: UNCOUNTED_DELAY_SECS,
        });
    }
    match usize::try_from(failures_so_far)
        .ok()
        .and_then(|index| BACKOFF_SECS.get(index))
    {
        Some(delay_secs) => NetworkRetryPlan::Retry(NetworkRetry {
            failures: failures_so_far + 1,
            delay_secs: *delay_secs,
        }),
        None => NetworkRetryPlan::GiveUp {
            failures: failures_so_far + 1,
        },
    }
}

fn overlapped_sleep(wall_elapsed: Duration, awake_elapsed: Duration) -> bool {
    wall_elapsed.saturating_sub(awake_elapsed) > SLEEP_GAP
}

/// `pmset -g systemstate` lists the current capabilities; a dark wake has no
/// graphics (`pmset -g log` shows `[CDNP]` there and `[CDNVA]` on a full wake).
fn pmset_reports_dark_wake(output: &str) -> bool {
    output
        .lines()
        .find(|line| line.starts_with("Current System Capabilities"))
        .is_some_and(|line| !line.contains("Graphics"))
}

/// False whenever the state cannot be read, as on other platforms: a retry
/// must not wait for a wake nobody can see.
pub(crate) async fn system_in_dark_wake() -> bool {
    let mut command = Command::new("pmset");
    command.args(["-g", "systemstate"]).kill_on_drop(true);
    match tokio::time::timeout(PMSET_TIMEOUT, command.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            pmset_reports_dark_wake(&String::from_utf8_lossy(&output.stdout))
        }
        _ => false,
    }
}

/// What to do with a turn that lost its connection. `None` when the request
/// cannot be retried: it has no work item, or it is no longer simply running.
pub(crate) async fn plan_network_retry(
    pool: &SqlitePool,
    work_item_id: Option<Uuid>,
    current_status: Option<&str>,
    run_id: Uuid,
    awake_elapsed: Duration,
) -> CommandResult<Option<NetworkRetryPlan>> {
    let Some(work_item_id) = work_item_id else {
        return Ok(None);
    };
    if current_status != Some("running") {
        return Ok(None);
    }
    let row = sqlx::query(
        r#"
        select w.network_failures, r.started_at
        from agent_work_items w
        join agent_runs r on r.id = $2
        where w.id = $1
        "#,
    )
    .bind(work_item_id)
    .bind(run_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;
    let wall_elapsed = DateTime::parse_from_rfc3339(&row.get::<String, _>("started_at"))
        .ok()
        .and_then(|started_at| (Utc::now() - started_at.to_utc()).to_std().ok())
        .unwrap_or_default();
    let counted = !overlapped_sleep(wall_elapsed, awake_elapsed) && !system_in_dark_wake().await;
    Ok(Some(next_network_retry(
        row.get("network_failures"),
        counted,
    )))
}

/// The retries ran out: tell the owner, who can run the request again.
pub(crate) async fn raise_network_failure_card(
    pool: &SqlitePool,
    work_item_id: Uuid,
    failures: i64,
    detail: &str,
) -> CommandResult<()> {
    let row = sqlx::query(
        r#"
        select w.channel_id, w.title, a.handle
        from agent_work_items w
        join agents a on a.id = w.agent_id
        where w.id = $1
        "#,
    )
    .bind(work_item_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;
    let handle: String = row.get("handle");
    let request: String = row.get("title");
    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let Some(channel_id) = card_channel_in_tx(&mut tx, row.get("channel_id")).await? else {
        return Ok(());
    };
    let options = vec![DecisionOption {
        id: "a".to_owned(),
        label: "Run the request again".to_owned(),
        detail: "It continues in the same session, so work already done is not repeated. Dismiss this card to drop the request.".to_owned(),
        recommended: true,
    }];
    let (decision_id, _) = create_system_decision_in_tx(
        &mut tx,
        channel_id,
        // No `@`: the owner's answer repeats the title, and a mention would wake the agent.
        &format!("{handle} could not finish a request: the connection kept failing"),
        &format!(
            "Lantor stopped retrying after {failures} failed attempts on an awake Mac.\n\nRequest: {request}\n\nLast error: {}",
            detail.trim()
        ),
        options,
    )
    .await?;
    sqlx::query("update decisions set retry_work_item_id = $2 where id = $1")
        .bind(decision_id)
        .bind(work_item_id)
        .execute(&mut *tx)
        .await
        .map_err(to_string)?;
    tx.commit().await.map_err(to_string)?;
    notify_decision_changed(pool).await
}

/// The owner answered a card from [`raise_network_failure_card`].
pub(crate) async fn retry_after_owner_answer(
    pool: &SqlitePool,
    decision_id: Uuid,
) -> CommandResult<()> {
    let work_item_id: Option<Uuid> =
        sqlx::query_scalar("select retry_work_item_id from decisions where id = $1")
            .bind(decision_id)
            .fetch_one(pool)
            .await
            .map_err(to_string)?;
    let Some(work_item_id) = work_item_id else {
        return Ok(());
    };
    retry_agent_work_in_pool(pool, work_item_id).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counted_failures_back_off_and_then_give_up() {
        let mut failures = 0;
        for expected_delay in BACKOFF_SECS {
            let NetworkRetryPlan::Retry(retry) = next_network_retry(failures, true) else {
                panic!("retries end early, at {failures} failures");
            };
            assert_eq!(retry.delay_secs, expected_delay);
            assert_eq!(retry.failures, failures + 1);
            failures = retry.failures;
        }
        assert_eq!(failures, 5);
        assert_eq!(
            next_network_retry(failures, true),
            NetworkRetryPlan::GiveUp { failures: 6 }
        );
    }

    #[test]
    fn failures_from_sleep_cost_nothing_even_after_the_limit() {
        for failures in [0, 3, 5] {
            assert_eq!(
                next_network_retry(failures, false),
                NetworkRetryPlan::Retry(NetworkRetry {
                    failures,
                    delay_secs: UNCOUNTED_DELAY_SECS,
                })
            );
        }
    }

    #[test]
    fn a_turn_slept_when_its_wall_clock_outran_its_monotonic_clock() {
        let hour = Duration::from_secs(3600);
        assert!(!overlapped_sleep(hour, hour));
        assert!(!overlapped_sleep(hour + Duration::from_millis(40), hour));
        assert!(!overlapped_sleep(Duration::from_secs(1), hour));
        assert!(overlapped_sleep(hour * 4, Duration::from_secs(60)));
    }

    #[test]
    fn pmset_output_tells_a_dark_wake_from_a_full_wake() {
        assert!(!pmset_reports_dark_wake(
            "Current System Capabilities are: CPU Graphics Audio Network \nCurrent Power State: 4\n"
        ));
        assert!(pmset_reports_dark_wake(
            "Current System Capabilities are: CPU Network Disk \nCurrent Power State: 4\n"
        ));
        assert!(!pmset_reports_dark_wake(""));
    }

    #[test]
    fn the_retry_note_comes_before_the_request() {
        let prompt = with_network_retry_note("the request".to_owned());
        assert!(prompt.starts_with("Lantor note: the connection"));
        assert!(prompt.ends_with("\n\nthe request"));
    }
}
