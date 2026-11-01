use std::time::Duration;

use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use super::{
    acquire_check_lease, release_check_lease, run_reminder_check, ReminderCheckJob,
    ReminderCheckOutcome,
};
use crate::domain::reminders::{
    cancel_reminder_in_pool, create_reminder_in_pool, process_due_reminders, ReminderLoopOptions,
};
use crate::test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool};

struct LoopFixture {
    agent_id: Uuid,
    channel_id: Uuid,
    thread_root_id: Uuid,
}

async fn loop_fixture(pool: &SqlitePool, handle: &str) -> Result<LoopFixture, String> {
    let agent_id = insert_test_agent(pool, handle).await?;
    let channel_id = insert_test_channel(pool, &format!("{handle}-loops")).await?;
    let workspace = std::env::temp_dir().join(format!("lantor-loop-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&workspace).map_err(|err| err.to_string())?;
    sqlx::query("update agents set working_directory = $2 where id = $1")
        .bind(agent_id)
        .bind(workspace.to_string_lossy().into_owned())
        .execute(pool)
        .await
        .map_err(|err| err.to_string())?;
    sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2)")
        .bind(channel_id)
        .bind(agent_id)
        .execute(pool)
        .await
        .map_err(|err| err.to_string())?;
    let thread_root_id: Uuid = sqlx::query_scalar(
        r#"
        insert into messages (channel_id, sender_name, sender_role, body)
        values ($1, 'Dylan', 'owner', 'Forward new messages here')
        returning id
        "#,
    )
    .bind(channel_id)
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())?;
    Ok(LoopFixture {
        agent_id,
        channel_id,
        thread_root_id,
    })
}

async fn create_loop(
    pool: &SqlitePool,
    fixture: &LoopFixture,
    command: &str,
    action: &str,
) -> Result<Uuid, String> {
    create_reminder_in_pool(
        pool,
        Some(fixture.agent_id),
        Some(fixture.channel_id),
        Some(fixture.thread_root_id),
        None,
        "Forward #github-pulls",
        "",
        chrono::Utc::now(),
        "interval",
        ReminderLoopOptions {
            interval_seconds: Some(300),
            check_command: command.to_owned(),
            check_action: action.to_owned(),
        },
    )
    .await
}

fn job(fixture: &LoopFixture, reminder_id: Uuid, command: &str, action: &str) -> ReminderCheckJob {
    ReminderCheckJob {
        reminder_id,
        creator_agent_id: Some(fixture.agent_id),
        channel_id: Some(fixture.channel_id),
        thread_root_id: Some(fixture.thread_root_id),
        title: "Forward #github-pulls".to_owned(),
        check_command: command.to_owned(),
        check_action: action.to_owned(),
        interval_seconds: 300,
        timeout: Duration::from_secs(20),
    }
}

async fn thread_messages(
    pool: &SqlitePool,
    fixture: &LoopFixture,
) -> Result<Vec<(String, String, Option<Uuid>)>, String> {
    let rows = sqlx::query(
        r#"
        select sender_role, body, sender_agent_id
        from messages
        where thread_root_id = $1
        order by seq
        "#,
    )
    .bind(fixture.thread_root_id)
    .fetch_all(pool)
    .await
    .map_err(|err| err.to_string())?;
    Ok(rows
        .into_iter()
        .map(|row| {
            (
                row.get("sender_role"),
                row.get("body"),
                row.get("sender_agent_id"),
            )
        })
        .collect())
}

async fn loop_state(pool: &SqlitePool, reminder_id: Uuid) -> Result<(String, String, i64), String> {
    sqlx::query_as(
        "select last_check_status, last_check_detail, check_failures from reminders where id = $1",
    )
    .bind(reminder_id)
    .fetch_one(pool)
    .await
    .map_err(|err| err.to_string())
}

#[tokio::test]
async fn due_loop_runs_its_check_without_posting_a_reminder_message() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let fixture = loop_fixture(&pool, "loop-quiet").await?;
        let reminder_id = create_loop(&pool, &fixture, "printf ''", "post").await?;
        // Anchor 12 minutes ago: the next tick lands on the 5-minute cadence.
        sqlx::query(
            r#"
            update reminders
            set due_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-12 minutes'),
                recurrence_anchor_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-12 minutes')
            where id = $1
            "#,
        )
        .bind(reminder_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;

        process_due_reminders(&pool).await?;

        let (status, seconds_until_due): (String, f64) = sqlx::query_as(
            "select status, (julianday(due_at) - julianday('now')) * 86400.0 from reminders where id = $1",
        )
        .bind(reminder_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(status, "scheduled");
        assert!(
            (150.0..=181.0).contains(&seconds_until_due),
            "next tick should be 15 minutes after the anchor, got {seconds_until_due}s from now"
        );
        let mut checked = String::new();
        for _ in 0..100 {
            checked = loop_state(&pool, reminder_id).await?.0;
            if !checked.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(checked, "quiet");
        let mut lease_released = false;
        for _ in 0..100 {
            let lease: Option<String> =
                sqlx::query_scalar("select check_lease_until from reminders where id = $1")
                    .bind(reminder_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            if lease.is_none() {
                lease_released = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(lease_released, "a finished check should release its lease");
        let system_messages: i64 = sqlx::query_scalar(
            "select count(*) from messages where channel_id = $1 and sender_role = 'system'",
        )
        .bind(fixture.channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(system_messages, 0);
        let inbox_items: i64 =
            sqlx::query_scalar("select count(*) from agent_inbox_items where agent_id = $1")
                .bind(fixture.agent_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(inbox_items, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn post_loop_posts_output_as_the_agent_without_dispatching_mentions() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let fixture = loop_fixture(&pool, "loop-poster").await?;
        let other_agent = insert_test_agent(&pool, "bystander").await?;
        sqlx::query("insert into channel_members (channel_id, agent_id) values ($1, $2)")
            .bind(fixture.channel_id)
            .bind(other_agent)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        let command = "printf 'PR #1 opened\\n@bystander please review\\n'; printf \"$LANTOR_AGENT_HANDLE\" > handle.txt";
        let reminder_id = create_loop(&pool, &fixture, command, "post").await?;

        let outcome = run_reminder_check(&pool, &job(&fixture, reminder_id, command, "post")).await?;
        let ReminderCheckOutcome::Posted { message_id } = outcome else {
            return Err(format!("expected a post, got {outcome:?}"));
        };
        let messages = thread_messages(&pool, &fixture).await?;
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].0, "agent");
        assert_eq!(messages[0].1, "PR #1 opened\n@bystander please review");
        assert_eq!(messages[0].2, Some(fixture.agent_id));
        let bystander_inbox: i64 =
            sqlx::query_scalar("select count(*) from agent_inbox_items where agent_id = $1")
                .bind(other_agent)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(bystander_inbox, 0, "forwarded output must not dispatch mentions");
        let (status, detail, failures) = loop_state(&pool, reminder_id).await?;
        assert_eq!(status, "posted");
        assert_eq!(detail, message_id.to_string());
        assert_eq!(failures, 0);
        // The command runs in the agent workspace with the agent identity.
        let workspace: String =
            sqlx::query_scalar("select working_directory from agents where id = $1")
                .bind(fixture.agent_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        let handle = std::fs::read_to_string(std::path::Path::new(&workspace).join("handle.txt"))
            .map_err(|err| err.to_string())?;
        assert_eq!(handle, "loop-poster");
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn wake_loop_hands_output_to_the_agent_inbox() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let fixture = loop_fixture(&pool, "loop-waker").await?;
        let command = "printf 'iceberg PR #2 needs a look\\n'";
        let reminder_id = create_loop(&pool, &fixture, command, "wake").await?;

        let outcome =
            run_reminder_check(&pool, &job(&fixture, reminder_id, command, "wake")).await?;
        let ReminderCheckOutcome::Woke { inbox_item_id } = outcome else {
            return Err(format!("expected a wake, got {outcome:?}"));
        };
        let row = sqlx::query(
            r#"
            select kind, channel_id, thread_root_id, source_message_id, body_preview,
                   json_extract(payload, '$.check_output') as check_output,
                   work_item_id
            from agent_inbox_items
            where id = $1 and agent_id = $2
            "#,
        )
        .bind(inbox_item_id)
        .bind(fixture.agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(row.get::<String, _>("kind"), "loop_check");
        assert_eq!(
            row.get::<Option<Uuid>, _>("channel_id"),
            Some(fixture.channel_id)
        );
        assert_eq!(
            row.get::<Option<Uuid>, _>("thread_root_id"),
            Some(fixture.thread_root_id)
        );
        assert_eq!(row.get::<Option<Uuid>, _>("source_message_id"), None);
        assert_eq!(
            row.get::<String, _>("check_output"),
            "iceberg PR #2 needs a look"
        );
        assert!(
            row.get::<Option<Uuid>, _>("work_item_id").is_some(),
            "the agent should be woken"
        );
        assert!(thread_messages(&pool, &fixture).await?.is_empty());
        assert_eq!(loop_state(&pool, reminder_id).await?.0, "woke");
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn failing_loop_notifies_once_backs_off_and_reports_recovery() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let fixture = loop_fixture(&pool, "loop-failing").await?;
        let failing = "echo 'token expired' >&2; exit 3";
        let reminder_id = create_loop(&pool, &fixture, failing, "post").await?;

        for expected in 1..=4 {
            let outcome =
                run_reminder_check(&pool, &job(&fixture, reminder_id, failing, "post")).await?;
            assert_eq!(
                outcome,
                ReminderCheckOutcome::Failed {
                    error: "exit status 3: token expired".to_owned(),
                    failures: expected,
                }
            );
        }
        let messages = thread_messages(&pool, &fixture).await?;
        assert_eq!(
            messages.len(),
            1,
            "only the third failure is announced: {messages:?}"
        );
        assert_eq!(messages[0].0, "system");
        assert!(messages[0]
            .1
            .contains("failed 3 checks in a row: exit status 3: token expired"));
        let seconds_until_due: f64 = sqlx::query_scalar(
            "select (julianday(due_at) - julianday('now')) * 86400.0 from reminders where id = $1",
        )
        .bind(reminder_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        // Fourth failure: 300s * 2^2.
        assert!(
            (1_150.0..=1_201.0).contains(&seconds_until_due),
            "backoff should push the next tick out, got {seconds_until_due}s"
        );

        let outcome =
            run_reminder_check(&pool, &job(&fixture, reminder_id, "printf ''", "post")).await?;
        assert_eq!(outcome, ReminderCheckOutcome::Quiet);
        assert_eq!(
            loop_state(&pool, reminder_id).await?,
            ("quiet".to_owned(), String::new(), 0)
        );
        let messages = thread_messages(&pool, &fixture).await?;
        assert_eq!(messages.len(), 2);
        assert!(messages[1].1.contains("is working again"));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn stopped_loop_never_delivers_output_from_a_running_check() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let fixture = loop_fixture(&pool, "loop-stopped").await?;
        let command = "sleep 0.3; printf 'late output\\n'";
        let reminder_id = create_loop(&pool, &fixture, command, "post").await?;
        let check_job = job(&fixture, reminder_id, command, "post");
        let check_pool = pool.clone();
        let running =
            tokio::spawn(async move { run_reminder_check(&check_pool, &check_job).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_reminder_in_pool(&pool, reminder_id).await?;
        let outcome = running.await.map_err(|err| err.to_string())??;
        assert_eq!(outcome, ReminderCheckOutcome::Inactive);
        assert!(thread_messages(&pool, &fixture).await?.is_empty());
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn long_output_is_truncated_and_slow_checks_time_out() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let fixture = loop_fixture(&pool, "loop-limits").await?;
        let chatty = "yes 'new item' | head -c 200000";
        let reminder_id = create_loop(&pool, &fixture, chatty, "post").await?;
        let outcome =
            run_reminder_check(&pool, &job(&fixture, reminder_id, chatty, "post")).await?;
        assert!(
            matches!(outcome, ReminderCheckOutcome::Posted { .. }),
            "{outcome:?}"
        );
        let messages = thread_messages(&pool, &fixture).await?;
        let body = &messages[0].1;
        let tail: String = body
            .chars()
            .rev()
            .take(40)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        assert!(body.ends_with("…(output truncated)"), "{tail}");
        assert!(body.chars().count() < 20_100);

        let mut slow = job(&fixture, reminder_id, "sleep 5; printf 'too late'", "post");
        slow.timeout = Duration::from_millis(300);
        let started = std::time::Instant::now();
        let outcome = run_reminder_check(&pool, &slow).await?;
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(
            outcome,
            ReminderCheckOutcome::Failed {
                error: "timed out after 1s".to_owned(),
                failures: 1,
            }
        );
        assert_eq!(thread_messages(&pool, &fixture).await?.len(), 1);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn loop_options_are_validated_on_create() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let fixture = loop_fixture(&pool, "loop-validate").await?;
        let outsider = insert_test_agent(&pool, "outsider").await?;
        // (creator, recurrence, interval, command, action, expected error)
        type Case<'a> = (
            Option<Uuid>,
            &'a str,
            Option<i64>,
            &'a str,
            &'a str,
            &'a str,
        );
        let cases: [Case; 6] = [
            (
                Some(fixture.agent_id),
                "interval",
                Some(30),
                "true",
                "post",
                "between 1 minute and 7 days",
            ),
            (
                Some(fixture.agent_id),
                "interval",
                None,
                "true",
                "post",
                "require interval_minutes",
            ),
            (
                Some(fixture.agent_id),
                "none",
                None,
                "true",
                "post",
                "requires a repeating reminder",
            ),
            (
                Some(fixture.agent_id),
                "interval",
                Some(300),
                "true",
                "",
                "check_action post or wake",
            ),
            (
                None,
                "interval",
                Some(300),
                "true",
                "post",
                "created by an agent",
            ),
            (
                Some(outsider),
                "interval",
                Some(300),
                "true",
                "post",
                "channel membership",
            ),
        ];
        for (agent, recurrence, interval, command, action, expected) in cases {
            let err = create_reminder_in_pool(
                &pool,
                agent,
                Some(fixture.channel_id),
                None,
                None,
                "Loop",
                "",
                chrono::Utc::now(),
                recurrence,
                ReminderLoopOptions {
                    interval_seconds: interval,
                    check_command: command.to_owned(),
                    check_action: action.to_owned(),
                },
            )
            .await
            .expect_err("invalid loop should be rejected");
            assert!(err.contains(expected), "{err} should mention {expected}");
        }
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn check_lease_keeps_one_check_per_loop_across_processes() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let fixture = loop_fixture(&pool, "loop-lease").await?;
        let reminder_id = create_loop(&pool, &fixture, "printf ''", "post").await?;
        let timeout = Duration::from_secs(60);

        // The desktop app and the supervisor share the database: the second
        // claimant must skip while the first check holds the lease.
        assert!(acquire_check_lease(&pool, reminder_id, timeout).await?);
        assert!(!acquire_check_lease(&pool, reminder_id, timeout).await?);
        let lease_seconds: f64 = sqlx::query_scalar(
            "select (julianday(check_lease_until) - julianday('now')) * 86400.0 from reminders where id = $1",
        )
        .bind(reminder_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert!(
            (85.0..=91.0).contains(&lease_seconds),
            "lease should cover the timeout plus grace, got {lease_seconds}s"
        );

        release_check_lease(&pool, reminder_id).await?;
        assert!(acquire_check_lease(&pool, reminder_id, timeout).await?);

        // A lease left by a crashed process expires instead of stopping the loop.
        sqlx::query(
            "update reminders set check_lease_until = strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-1 seconds') where id = $1",
        )
        .bind(reminder_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert!(acquire_check_lease(&pool, reminder_id, timeout).await?);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}
