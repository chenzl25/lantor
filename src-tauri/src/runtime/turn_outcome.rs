use std::{future::Future, time::Duration};

use crate::app::CommandResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WarmTurnOutcome {
    pub(crate) agent_status: &'static str,
    pub(crate) run_status: &'static str,
    pub(crate) should_reset_runtime: bool,
    pub(crate) runtime_session_status: &'static str,
    pub(crate) activity_kind: &'static str,
    pub(crate) activity_title: &'static str,
}

pub(crate) fn resolve_warm_turn_outcome(success: bool, was_cancelled: bool) -> WarmTurnOutcome {
    if was_cancelled {
        return WarmTurnOutcome {
            agent_status: "idle",
            run_status: "cancelled",
            should_reset_runtime: false,
            runtime_session_status: "idle",
            activity_kind: "run",
            activity_title: "Stopped",
        };
    }

    if success {
        WarmTurnOutcome {
            agent_status: "idle",
            run_status: "exited",
            should_reset_runtime: false,
            runtime_session_status: "idle",
            activity_kind: "run",
            activity_title: "Completed",
        }
    } else {
        WarmTurnOutcome {
            agent_status: "idle",
            run_status: "failed",
            should_reset_runtime: true,
            runtime_session_status: "stopped",
            activity_kind: "run_error",
            activity_title: "Failed",
        }
    }
}

/// SQLite answers some write conflicts with `database is locked` at once
/// instead of waiting out `busy_timeout` (a deferred transaction that read
/// first, for instance). Turn finalization retries those so a burst of agents
/// finishing together cannot leave a run open.
pub(crate) fn is_transient_sqlite_error(error: &str) -> bool {
    error.contains("database is locked")
        || error.contains("database table is locked")
        || error.contains("(code: 5)")
        || error.contains("(code: 6)")
}

const TRANSIENT_RETRY_ATTEMPTS: u32 = 8;
const TRANSIENT_RETRY_FIRST_DELAY: Duration = Duration::from_millis(50);
const TRANSIENT_RETRY_MAX_DELAY: Duration = Duration::from_secs(2);

pub(crate) async fn retry_transient_sqlite<T, F, Fut>(mut operation: F) -> CommandResult<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = CommandResult<T>>,
{
    let mut delay = TRANSIENT_RETRY_FIRST_DELAY;
    let mut attempt = 1;
    loop {
        match operation().await {
            Err(error)
                if attempt < TRANSIENT_RETRY_ATTEMPTS && is_transient_sqlite_error(&error) =>
            {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(TRANSIENT_RETRY_MAX_DELAY);
                attempt += 1;
            }
            result => return result,
        }
    }
}

/// Keeps the first error of a best-effort step and lets finalization go on.
pub(crate) fn keep_first_error<T>(
    first: &mut Option<String>,
    result: CommandResult<T>,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            if first.is_none() {
                *first = Some(error);
            }
            None
        }
    }
}

/// Work-item status after a turn ends. A request whose turn failed for a
/// reason that passes (an expired login, a lost connection) goes back to the
/// queue instead of failing.
pub(crate) fn finished_work_status(
    success: bool,
    was_cancelled: bool,
    requeue: bool,
    current_status: Option<&str>,
) -> &'static str {
    if was_cancelled {
        "cancelled"
    } else if requeue {
        "queued"
    } else if success && current_status == Some("silent") {
        "silent"
    } else if success && current_status == Some("held") {
        "held"
    } else if success {
        "done"
    } else {
        "failed"
    }
}

#[cfg(test)]
mod tests {
    use super::{resolve_warm_turn_outcome, WarmTurnOutcome};

    #[test]
    fn warm_turn_outcome_matrix_keeps_agents_routable_and_resets_only_failures() {
        let cases = [
            (
                true,
                false,
                WarmTurnOutcome {
                    agent_status: "idle",
                    run_status: "exited",
                    should_reset_runtime: false,
                    runtime_session_status: "idle",
                    activity_kind: "run",
                    activity_title: "Completed",
                },
            ),
            (
                false,
                false,
                WarmTurnOutcome {
                    agent_status: "idle",
                    run_status: "failed",
                    should_reset_runtime: true,
                    runtime_session_status: "stopped",
                    activity_kind: "run_error",
                    activity_title: "Failed",
                },
            ),
            (
                true,
                true,
                WarmTurnOutcome {
                    agent_status: "idle",
                    run_status: "cancelled",
                    should_reset_runtime: false,
                    runtime_session_status: "idle",
                    activity_kind: "run",
                    activity_title: "Stopped",
                },
            ),
            (
                false,
                true,
                WarmTurnOutcome {
                    agent_status: "idle",
                    run_status: "cancelled",
                    should_reset_runtime: false,
                    runtime_session_status: "idle",
                    activity_kind: "run",
                    activity_title: "Stopped",
                },
            ),
        ];

        for (success, was_cancelled, expected) in cases {
            assert_eq!(
                resolve_warm_turn_outcome(success, was_cancelled),
                expected,
                "success={success} was_cancelled={was_cancelled}"
            );
        }
    }
}
