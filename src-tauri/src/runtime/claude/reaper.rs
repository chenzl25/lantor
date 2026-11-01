use std::{sync::Arc, time::Duration};

use sqlx::SqlitePool;
use tokio::time::sleep;
use uuid::Uuid;

use super::WarmClaudeRuntime;
use crate::runtime::warm::stop_idle_process;

const CLAUDE_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const CLAUDE_IDLE_REAPER_INTERVAL: Duration = Duration::from_secs(30);

pub(super) async fn claude_warm_idle_reaper(
    pool: SqlitePool,
    agent_id: Uuid,
    runtime: Arc<WarmClaudeRuntime>,
) {
    loop {
        sleep(CLAUDE_IDLE_REAPER_INTERVAL).await;
        let should_stop = {
            let mut state = runtime.state.lock().await;
            let should_stop = state.alive
                && !state.is_busy()
                && state.last_activity.elapsed() >= CLAUDE_IDLE_TIMEOUT;
            if should_stop {
                state.alive = false;
            }
            should_stop
        };
        if !should_stop {
            if !runtime.state.lock().await.alive {
                return;
            }
            continue;
        }

        stop_idle_process(&pool, agent_id, &runtime).await;
        return;
    }
}
