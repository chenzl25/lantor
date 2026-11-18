use std::time::Duration;

use tokio::sync::Notify;

use super::{SupervisorWake, Wake};
use crate::test_support::{drop_test_schema, insert_test_agent, test_pool};

/// A signal of the test's own, so other tests' wakes cannot reach it.
fn own_signal() -> &'static Notify {
    Box::leak(Box::new(Notify::new()))
}

#[tokio::test]
async fn a_signal_from_this_process_ends_the_wait() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let signal = own_signal();
    let mut wake = SupervisorWake::connect_with_signal(&pool, signal).await;
    // Sent while the supervisor was busy with its pass: it is not lost.
    signal.notify_one();
    assert_eq!(wake.wait(Duration::from_secs(60)).await, Wake::Signalled);
    drop(wake);
    drop_test_schema(pool, schema).await;
}

#[tokio::test]
async fn work_queued_through_another_connection_ends_the_wait() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "wake-agent").await?;
        let queue_request = || async {
            sqlx::query_scalar::<_, uuid::Uuid>(
                "insert into agent_work_items (agent_id, title, status) values ($1, 'request', 'queued') returning id",
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())
        };
        // Queued before the supervisor started watching: its next pass
        // starts it, so it is no reason to wake.
        queue_request().await?;
        let mut wake = SupervisorWake::connect_with_signal(&pool, own_signal()).await;
        let no_wait = Duration::ZERO;
        assert_eq!(wake.wait(no_wait).await, Wake::TimedOut);

        // The app's process writes through its own connections.
        let request = queue_request().await?;
        assert_eq!(wake.wait(Duration::from_secs(60)).await, Wake::Queued);
        assert_eq!(wake.wait(no_wait).await, Wake::TimedOut);

        // Writes that queue nothing, such as the heartbeat or an agent's
        // streamed reply, do not end the wait.
        crate::runtime::supervisor::write_supervisor_heartbeat(&pool).await?;
        assert_eq!(wake.wait(no_wait).await, Wake::TimedOut);

        // A request that starts leaves the queue, which is worth one look.
        sqlx::query("update agent_work_items set status = 'running' where id = $1")
            .bind(request)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(wake.wait(no_wait).await, Wake::Queued);

        sqlx::query(
            "insert into supervisor_commands (command_type, agent_id, status) values ('stop_agent', $1, 'pending')",
        )
        .bind(agent_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(wake.wait(Duration::from_secs(60)).await, Wake::Queued);
        drop(wake);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    result.unwrap();
}
