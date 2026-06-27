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
