use super::{
    dispatch_agent_restart_backlog, dispatch_task_followup_to_agent, try_claim_unassigned_task,
};
use crate::agent_inbox_wake::{create_agent_inbox_item, AgentInboxItemInput};
use crate::events::control::{handle_agent_event, AgentEvent};
use crate::message_store::send_owner_message_in_pool;
use crate::test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool};
use crate::ui_notifications::reconcile_work_item_change;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

#[tokio::test]
async fn task_followup_dispatch_uses_the_followup_as_fresh_context() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "followup-agent").await?;
        let channel_id = insert_test_channel(&pool, "task-followup").await?;
        let thread_root_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'Review the original head', true)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let task_id: Uuid = sqlx::query_scalar(
            r#"
            insert into tasks (
                message_id, channel_id, title, status, assignee_agent_id
            )
            values ($1, $2, 'Review the original head', 'in_progress', $3)
            returning id
            "#,
        )
        .bind(thread_root_id)
        .bind(channel_id)
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let followup_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (
                channel_id, thread_root_id, sender_name, sender_role, body
            )
            values ($1, $2, 'Dylan', 'owner', 'Re-review only the new commit')
            returning id
            "#,
        )
        .bind(channel_id)
        .bind(thread_root_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        dispatch_task_followup_to_agent(
            &pool,
            task_id,
            agent_id,
            followup_id,
            "Re-review the new head",
            "Re-review only the new commit",
            "test_followup",
        )
        .await?;

        let inbox = sqlx::query(
            r#"
            select source_message_id, thread_root_id, task_id, title, body_preview
            from agent_inbox_items
            where agent_id = $1 and kind = 'task_assigned'
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(
            inbox.get::<Option<Uuid>, _>("source_message_id"),
            Some(followup_id)
        );
        assert_eq!(
            inbox.get::<Option<Uuid>, _>("thread_root_id"),
            Some(thread_root_id)
        );
        assert_eq!(inbox.get::<Option<Uuid>, _>("task_id"), Some(task_id));
        assert_eq!(inbox.get::<String, _>("title"), "Re-review the new head");
        assert_eq!(
            inbox.get::<String, _>("body_preview"),
            "Re-review only the new commit"
        );
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn owner_task_without_mentions_auto_assigns_single_channel_agent() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "solo-task-agent").await?;
        let channel_id = insert_test_channel(&pool, "solo-task").await?;
        sqlx::query(
            r#"
            insert into channel_members (channel_id, agent_id)
            values ($1, $2)
            "#,
        )
        .bind(channel_id)
        .bind(agent_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;

        send_owner_message_in_pool(
            &pool,
            None,
            channel_id,
            None,
            "Implement the compact task flow",
            true,
            vec![],
        )
        .await?;

        let task = sqlx::query("select status, assignee_agent_id from tasks limit 1")
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(task.get::<String, _>("status"), "in_progress");
        assert_eq!(
            task.get::<Option<Uuid>, _>("assignee_agent_id"),
            Some(agent_id)
        );
        let inbox_kind: String = sqlx::query_scalar(
            "select kind from agent_inbox_items where agent_id = $1 order by created_at desc limit 1",
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(inbox_kind, "task_assigned");
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn owner_task_without_mentions_stays_unassigned_with_multiple_channel_agents() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let first_agent_id = insert_test_agent(&pool, "multi-task-a").await?;
        let second_agent_id = insert_test_agent(&pool, "multi-task-b").await?;
        let channel_id = insert_test_channel(&pool, "multi-task").await?;
        for agent_id in [first_agent_id, second_agent_id] {
            sqlx::query(
                r#"
                insert into channel_members (channel_id, agent_id)
                values ($1, $2)
                "#,
            )
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        }

        send_owner_message_in_pool(
            &pool,
            None,
            channel_id,
            None,
            "Implement the unassigned queue",
            true,
            vec![],
        )
        .await?;

        let task = sqlx::query("select status, assignee_agent_id from tasks limit 1")
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(task.get::<String, _>("status"), "todo");
        assert_eq!(task.get::<Option<Uuid>, _>("assignee_agent_id"), None);
        let inbox_count: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from agent_inbox_items
            where task_id = (select id from tasks limit 1)
              and kind = 'task_available'
            "#,
        )
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(inbox_count, 2);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn unassigned_task_claim_is_atomic_across_agents() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let first_agent_id = insert_test_agent(&pool, "claim-race-a").await?;
        let second_agent_id = insert_test_agent(&pool, "claim-race-b").await?;
        let channel_id = insert_test_channel(&pool, "claim-race").await?;
        for agent_id in [first_agent_id, second_agent_id] {
            sqlx::query(
                r#"
                insert into channel_members (channel_id, agent_id)
                values ($1, $2)
                "#,
            )
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        }
        let message_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'Race to claim', true)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let task_id: Uuid = sqlx::query_scalar(
            r#"
            insert into tasks (message_id, channel_id, title, status)
            values ($1, $2, 'Race to claim', 'todo')
            returning id
            "#,
        )
        .bind(message_id)
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        let first = try_claim_unassigned_task(&pool, task_id, first_agent_id, Some(0), "test");
        let second = try_claim_unassigned_task(&pool, task_id, second_agent_id, Some(0), "test");
        let (first, second) = tokio::join!(first, second);
        let wins = [first?, second?]
            .into_iter()
            .filter(|claim| claim.is_some())
            .count();
        assert_eq!(wins, 1);

        let task =
            sqlx::query("select status, assignee_agent_id, version from tasks where id = $1")
                .bind(task_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())?;
        assert_eq!(task.get::<String, _>("status"), "in_progress");
        assert!(task.get::<Option<Uuid>, _>("assignee_agent_id").is_some());
        assert_eq!(task.get::<i64, _>("version"), 1);

        let assigned_inboxes: i64 = sqlx::query_scalar(
            "select count(*) from agent_inbox_items where task_id = $1 and kind = 'task_assigned'",
        )
        .bind(task_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(assigned_inboxes, 1);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn stale_task_claim_event_is_ignored_without_dispatch_noise() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let winner_agent_id = insert_test_agent(&pool, "claim-winner").await?;
        let stale_agent_id = insert_test_agent(&pool, "claim-stale").await?;
        let channel_id = insert_test_channel(&pool, "claim-stale").await?;
        for agent_id in [winner_agent_id, stale_agent_id] {
            sqlx::query(
                r#"
                insert into channel_members (channel_id, agent_id)
                values ($1, $2)
                "#,
            )
            .bind(channel_id)
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        }
        let message_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'Claim once', true)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let task_row = sqlx::query(
            r#"
            insert into tasks (message_id, channel_id, title, status)
            values ($1, $2, 'Claim once', 'todo')
            returning id, number
            "#,
        )
        .bind(message_id)
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let task_id: Uuid = task_row.get("id");
        let task_number: i64 = task_row.get("number");
        assert!(
            try_claim_unassigned_task(&pool, task_id, winner_agent_id, None, "test")
                .await?
                .is_some()
        );

        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, command, status)
            values ($1, 'codex app-server', 'running')
            returning id
            "#,
        )
        .bind(stale_agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let result = handle_agent_event(
            &pool,
            stale_agent_id,
            run_id,
            AgentEvent::TaskClaim {
                task_number,
                assignee_handle: None,
            },
        )
        .await?;
        assert_eq!(result, format!("task #{task_number} claim ignored"));

        let task = sqlx::query("select assignee_agent_id from tasks where id = $1")
            .bind(task_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(
            task.get::<Option<Uuid>, _>("assignee_agent_id"),
            Some(winner_agent_id)
        );
        let stale_inboxes: i64 = sqlx::query_scalar(
            "select count(*) from agent_inbox_items where task_id = $1 and agent_id = $2 and kind = 'task_assigned'",
        )
        .bind(task_id)
        .bind(stale_agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(stale_inboxes, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn task_work_item_queue_and_start_do_not_insert_system_messages() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "task-lifecycle-agent").await?;
        let channel_id = insert_test_channel(&pool, "task-lifecycle").await?;
        let message_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'Handle this task', true)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let task_id: Uuid = sqlx::query_scalar(
            r#"
            insert into tasks (message_id, channel_id, title, status, assignee_agent_id)
            values ($1, $2, 'Handle this task', 'in_progress', $3)
            returning id
            "#,
        )
        .bind(message_id)
        .bind(channel_id)
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let work_item_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_work_items (
                agent_id, channel_id, thread_root_id, source_message_id,
                task_id, source_kind, title, context, status
            )
            values ($1, $2, $3, $3, $4, 'task_assigned', 'Handle this task', 'context', 'queued')
            returning id
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .bind(message_id)
        .bind(task_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        reconcile_work_item_change(&pool, work_item_id, "work_item_created").await?;
        reconcile_work_item_change(&pool, work_item_id, "work_item_queued").await?;
        sqlx::query("update agent_work_items set status = 'running' where id = $1")
            .bind(work_item_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        reconcile_work_item_change(&pool, work_item_id, "work_item_running").await?;

        let system_messages: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from messages
            where channel_id = $1
              and thread_root_id = $2
              and sender_role = 'system'
              and (body like '%queued task run%' or body like '%started task run%')
            "#,
        )
        .bind(channel_id)
        .bind(message_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(system_messages, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn busy_warm_codex_start_requeues_work_item_without_leaving_running_state() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "busy-start-agent").await?;
        let channel_id = insert_test_channel(&pool, "busy-start").await?;
        let work_item_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_work_items (
                agent_id, channel_id, source_kind, title, context, status
            )
            values ($1, $2, 'thread_followup', 'busy start', 'context', 'running')
            returning id
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (agent_id, work_item_id, command, working_directory, status, pid, log)
            values ($1, $2, 'codex app-server --listen stdio://', '', 'running', 51932, '$ codex app-server --listen stdio://')
            returning id
            "#,
        )
        .bind(agent_id)
        .bind(work_item_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        sqlx::query(
            r#"
            update agent_work_items
            set run_id = $2
            where id = $1
            "#,
        )
        .bind(work_item_id)
        .bind(run_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;
        sqlx::query("update agents set status = 'running' where id = $1")
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

        crate::runtime::codex::cleanup_failed_warm_codex_start(
            &pool,
            agent_id,
            run_id,
            Some(work_item_id),
            "codex warm runtime became busy before turn start",
            crate::runtime::process::WarmStartFailure::RuntimeBusy,
        )
        .await?;

        let run_row = sqlx::query(
            r#"
            select status, stopped_at, log
            from agent_runs
            where id = $1
            "#,
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let run_status: String = run_row.get("status");
        let run_stopped_at: Option<String> = run_row.get("stopped_at");
        let run_log: String = run_row.get("log");
        assert_eq!(run_status, "failed");
        assert!(run_stopped_at.is_some());
        assert!(run_log.contains("codex warm runtime became busy before turn start"));

        let work_row = sqlx::query(
            r#"
            select status, run_id, completed_at
            from agent_work_items
            where id = $1
            "#,
        )
        .bind(work_item_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let work_status: String = work_row.get("status");
        let work_run_id: Option<Uuid> = work_row.get("run_id");
        let work_completed_at: Option<String> = work_row.get("completed_at");
        assert_eq!(work_status, "queued");
        assert!(work_run_id.is_none());
        assert!(work_completed_at.is_none());

        let agent_status: String = sqlx::query_scalar("select status from agents where id = $1")
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(agent_status, "running");

        let activity_title: String = sqlx::query_scalar(
            r#"
            select title
            from agent_activities
            where run_id = $1
            order by created_at desc
            limit 1
            "#,
        )
        .bind(run_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(activity_title, "Request requeued");
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn restart_backlog_redispatches_assigned_in_progress_tasks() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "restart-backlog-agent").await?;
        let channel_id = insert_test_channel(&pool, "restart-backlog").await?;
        let message_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Martin', 'owner', 'Finish restart backlog task', true)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let task_id: Uuid = sqlx::query_scalar(
            r#"
            insert into tasks (message_id, channel_id, title, status, assignee_agent_id)
            values ($1, $2, 'Finish restart backlog task', 'in_progress', $3)
            returning id
            "#,
        )
        .bind(message_id)
        .bind(channel_id)
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        sqlx::query(
            r#"
            insert into agent_work_items (
                agent_id, channel_id, thread_root_id, source_message_id,
                task_id, source_kind, title, context, status, completed_at
            )
            values (
                $1, $2, $3, $3, $4, 'task', 'Old failed attempt', 'old context',
                'failed', strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            )
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .bind(message_id)
        .bind(task_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;

        let (redispatched_tasks, _) = dispatch_agent_restart_backlog(&pool, agent_id).await?;
        assert_eq!(redispatched_tasks, 1);

        let assigned_inbox_items: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from agent_inbox_items
            where agent_id = $1
              and task_id = $2
              and kind = 'task_assigned'
            "#,
        )
        .bind(agent_id)
        .bind(task_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(assigned_inbox_items, 1);

        let pending_task_starts: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from supervisor_commands c
            join agent_work_items w on w.id = c.work_item_id
            where c.agent_id = $1
              and c.command_type = 'start_agent'
              and c.status = 'pending'
              and w.task_id = $2
            "#,
        )
        .bind(agent_id)
        .bind(task_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(pending_task_starts, 1);

        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn task_claim_opportunity_finish_does_not_insert_system_message() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let loser_agent_id = insert_test_agent(&pool, "claim-loser").await?;
        let winner_agent_id = insert_test_agent(&pool, "claim-winner-finish").await?;
        let channel_id = insert_test_channel(&pool, "claim-finish").await?;
        let available_message_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'Race this task', true)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let available_task_id: Uuid = sqlx::query_scalar(
            r#"
            insert into tasks (message_id, channel_id, title, status)
            values ($1, $2, 'Race this task', 'todo')
            returning id
            "#,
        )
        .bind(available_message_id)
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let available_inbox_id = create_agent_inbox_item(
            &pool,
            AgentInboxItemInput {
                agent_id: loser_agent_id,
                channel_id: Some(channel_id),
                thread_root_id: Some(available_message_id),
                source_message_id: Some(available_message_id),
                task_id: Some(available_task_id),
                kind: "task_available",
                priority: 70,
                title: "Race this task",
                body_preview: "Race this task",
                payload: json!({}),
            },
        )
        .await?;
        let available_work_item_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_work_items (
                agent_id, channel_id, thread_root_id, source_message_id, inbox_item_id,
                task_id, source_kind, title, context, status, completed_at
            )
            values ($1, $2, $3, $3, $4, $5, 'inbox_wake', 'Race this task', 'context', 'done', strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
            returning id
            "#,
        )
        .bind(loser_agent_id)
        .bind(channel_id)
        .bind(available_message_id)
        .bind(available_inbox_id)
        .bind(available_task_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        sqlx::query("update agent_inbox_items set work_item_id = $2 where id = $1")
            .bind(available_inbox_id)
            .bind(available_work_item_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        reconcile_work_item_change(&pool, available_work_item_id, "work_item_finished").await?;

        let assigned_message_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'Run this task', true)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let assigned_task_id: Uuid = sqlx::query_scalar(
            r#"
            insert into tasks (message_id, channel_id, title, status, assignee_agent_id)
            values ($1, $2, 'Run this task', 'in_progress', $3)
            returning id
            "#,
        )
        .bind(assigned_message_id)
        .bind(channel_id)
        .bind(winner_agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let assigned_inbox_id = create_agent_inbox_item(
            &pool,
            AgentInboxItemInput {
                agent_id: winner_agent_id,
                channel_id: Some(channel_id),
                thread_root_id: Some(assigned_message_id),
                source_message_id: Some(assigned_message_id),
                task_id: Some(assigned_task_id),
                kind: "task_assigned",
                priority: 95,
                title: "Run this task",
                body_preview: "Run this task",
                payload: json!({}),
            },
        )
        .await?;
        let assigned_work_item_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_work_items (
                agent_id, channel_id, thread_root_id, source_message_id, inbox_item_id,
                task_id, source_kind, title, context, status, completed_at
            )
            values ($1, $2, $3, $3, $4, $5, 'inbox_wake', 'Run this task', 'context', 'done', strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
            returning id
            "#,
        )
        .bind(winner_agent_id)
        .bind(channel_id)
        .bind(assigned_message_id)
        .bind(assigned_inbox_id)
        .bind(assigned_task_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        sqlx::query("update agent_inbox_items set work_item_id = $2 where id = $1")
            .bind(assigned_inbox_id)
            .bind(assigned_work_item_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        reconcile_work_item_change(&pool, assigned_work_item_id, "work_item_finished").await?;

        let claim_opportunity_messages: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from messages
            where channel_id = $1
              and thread_root_id = $2
              and sender_role = 'system'
              and body like '@claim-loser completed task run%'
            "#,
        )
        .bind(channel_id)
        .bind(available_message_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(claim_opportunity_messages, 0);

        let assigned_messages: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from messages
            where channel_id = $1
              and thread_root_id = $2
              and sender_role = 'system'
              and body like '@claim-winner-finish completed task run%'
            "#,
        )
        .bind(channel_id)
        .bind(assigned_message_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(assigned_messages, 0);

        sqlx::query("update tasks set status = 'in_review' where id = $1")
            .bind(assigned_task_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        reconcile_work_item_change(&pool, assigned_work_item_id, "work_item_finished").await?;

        let assigned_messages: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from messages
            where channel_id = $1
              and thread_root_id = $2
              and sender_role = 'system'
              and body like '@claim-winner-finish completed task run%'
            "#,
        )
        .bind(channel_id)
        .bind(assigned_message_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(assigned_messages, 1);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn conversational_work_item_finish_does_not_insert_system_message() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "conversation-agent").await?;
        let channel_id = insert_test_channel(&pool, "conversation-finish").await?;
        let source_message_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'Please answer in thread', false)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let work_item_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_work_items (
                agent_id, channel_id, thread_root_id, source_message_id,
                source_kind, title, context, status, completed_at
            )
            values ($1, $2, $3, $3, 'mention', 'Please answer in thread', 'context', 'done', strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
            returning id
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .bind(source_message_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        reconcile_work_item_change(&pool, work_item_id, "work_item_finished").await?;

        let system_messages: i64 = sqlx::query_scalar(
            r#"
            select count(*)
            from messages
            where channel_id = $1
              and thread_root_id = $2
              and sender_role = 'system'
              and body like '@conversation-agent completed agent request%'
            "#,
        )
        .bind(channel_id)
        .bind(source_message_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        assert_eq!(system_messages, 0);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn transient_warm_codex_start_retries_then_fails_without_flagging_agent() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "transient-start-agent").await?;
        let channel_id = insert_test_channel(&pool, "transient-start").await?;
        let work_item_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_work_items (
                agent_id, channel_id, source_kind, title, context, status
            )
            values ($1, $2, 'thread_followup', 'transient start', 'context', 'queued')
            returning id
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        let max_attempts = crate::runtime::process::MAX_WARM_START_ATTEMPTS;
        for attempt in 1..=max_attempts {
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, work_item_id, command, working_directory, status, pid, log)
                values ($1, $2, 'codex app-server --listen stdio://', '', 'running', 51932, '')
                returning id
                "#,
            )
            .bind(agent_id)
            .bind(work_item_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query(
                "update agent_work_items set status = 'running', run_id = $2 where id = $1",
            )
            .bind(work_item_id)
            .bind(run_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
            sqlx::query("update agents set status = 'running' where id = $1")
                .bind(agent_id)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;

            crate::runtime::codex::cleanup_failed_warm_codex_start(
                &pool,
                agent_id,
                run_id,
                Some(work_item_id),
                "error returned from database: (code: 5) database is locked",
                crate::runtime::process::WarmStartFailure::Transient,
            )
            .await?;

            let run_status: String =
                sqlx::query_scalar("select status from agent_runs where id = $1")
                    .bind(run_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            assert_eq!(run_status, "failed", "attempt {attempt}");
            let work_row =
                sqlx::query("select status, run_id from agent_work_items where id = $1")
                    .bind(work_item_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            let work_status: String = work_row.get("status");
            let work_run_id: Option<Uuid> = work_row.get("run_id");
            let agent_status: String =
                sqlx::query_scalar("select status from agents where id = $1")
                    .bind(agent_id)
                    .fetch_one(&pool)
                    .await
                    .map_err(|err| err.to_string())?;
            let activity_title: String = sqlx::query_scalar(
                "select title from agent_activities where run_id = $1 order by created_at desc limit 1",
            )
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            // A healthy runtime never flags the agent as broken.
            assert_eq!(agent_status, "idle", "attempt {attempt}");
            if attempt < max_attempts {
                assert_eq!(work_status, "queued", "attempt {attempt}");
                assert!(work_run_id.is_none(), "attempt {attempt}");
                assert_eq!(activity_title, "Request start retry scheduled");
            } else {
                assert_eq!(work_status, "failed");
                assert_eq!(activity_title, "Run failed to start");
            }
        }
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn error_agent_launch_waits_for_cooldown_then_relaunches() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "cooldown-agent").await?;
        let channel_id = insert_test_channel(&pool, "cooldown").await?;
        sqlx::query("update agents set status = 'error' where id = $1")
            .bind(agent_id)
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;
        crate::events::activity::record_agent_activity(
            &pool,
            Some(agent_id),
            None,
            "run_error",
            "Codex warm app-server failed to start",
            "spawn failed",
        )
        .await?;
        let work_item_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_work_items (
                agent_id, channel_id, source_kind, title, context, status
            )
            values ($1, $2, 'thread_followup', 'cooldown start', 'context', 'queued')
            returning id
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        let pending_starts = |pool: &sqlx::SqlitePool| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, i64>(
                    r#"
                    select count(*)
                    from supervisor_commands
                    where command_type = 'start_agent'
                      and work_item_id = $1
                      and status in ('pending', 'running')
                    "#,
                )
                .bind(work_item_id)
                .fetch_one(&pool)
                .await
                .map_err(|err| err.to_string())
            }
        };

        // A launch failure seconds ago keeps the request queued for now.
        let scheduled =
            crate::agent_inbox_wake::enqueue_agent_work_if_available(&pool, agent_id, work_item_id)
                .await?;
        assert!(!scheduled);
        assert_eq!(pending_starts(&pool).await?, 0);

        // Once the failure ages past the cooldown the next scheduler pass relaunches.
        sqlx::query(
            r#"
            update agent_activities
            set created_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now','-10 minutes')
            where agent_id = $1
            "#,
        )
        .bind(agent_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let scheduled =
            crate::agent_inbox_wake::enqueue_agent_work_if_available(&pool, agent_id, work_item_id)
                .await?;
        assert!(scheduled);
        assert_eq!(pending_starts(&pool).await?, 1);
        let agent_status: String = sqlx::query_scalar("select status from agents where id = $1")
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(agent_status, "queued");
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn retry_is_idempotent_across_clients_and_preserves_the_original_surface() {
    let (pool, schema) = crate::test_support::test_pool_with_connections(4)
        .await
        .expect("test database");
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "retry-agent").await?;
        let channel_id = insert_test_channel(&pool, "retry-channel").await?;
        let root: Uuid = sqlx::query_scalar("insert into messages (channel_id, sender_name, sender_role, body) values ($1, 'Owner', 'owner', 'Original request') returning id")
            .bind(channel_id).fetch_one(&pool).await.map_err(|e| e.to_string())?;
        let old: Uuid = sqlx::query_scalar("insert into agent_work_items (agent_id, channel_id, thread_root_id, source_message_id, source_kind, title, context, status) values ($1, $2, $3, $3, 'thread_followup', 'Original request', 'Original context', 'failed') returning id")
            .bind(agent_id).bind(channel_id).bind(root).fetch_one(&pool).await.map_err(|e| e.to_string())?;
        sqlx::query("update agents set status = 'error' where id = $1").bind(agent_id).execute(&pool).await.map_err(|e| e.to_string())?;
        let (left, right) = tokio::join!(super::retry_agent_work_in_pool(&pool, old), super::retry_agent_work_in_pool(&pool, old));
        let new = left?;
        assert_eq!(right?, new);
        assert_eq!(super::retry_agent_work_in_pool(&pool, old).await?, new, "lost-response retry reuses its attempt");
        let row = sqlx::query("select * from agent_work_items where id = $1").bind(new).fetch_one(&pool).await.map_err(|e| e.to_string())?;
        assert_eq!(row.get::<Uuid, _>("channel_id"), channel_id);
        assert_eq!(row.get::<Uuid, _>("thread_root_id"), root);
        assert_eq!(row.get::<Uuid, _>("source_message_id"), root);
        assert_eq!(row.get::<String, _>("context"), "Original context");
        let commands: i64 = sqlx::query_scalar("select count(*) from supervisor_commands where work_item_id = $1").bind(new).fetch_one(&pool).await.map_err(|e| e.to_string())?;
        assert_eq!(commands, 1, "only one execution is scheduled even from error state");
        assert!(super::retry_agent_work_in_pool(&pool, new).await.is_err(), "cannot retry queued work");
        sqlx::query("update agent_work_items set status = 'failed' where id = $1").bind(new).execute(&pool).await.map_err(|e| e.to_string())?;
        let next = super::retry_agent_work_in_pool(&pool, new).await?;
        assert_ne!(next, new);
        assert_eq!(super::retry_agent_work_in_pool(&pool, old).await?, new);
        // A subsequent startup/migration preserves the idempotency link.
        crate::db::migrate(&pool).await.map_err(|e| e.to_string())?;
        assert_eq!(super::retry_agent_work_in_pool(&pool, old).await?, new);
        let items = crate::activity_store::load_agent_work_items(&pool).await?;
        assert_eq!(items.iter().find(|item| item.id == old).unwrap().retry_work_item_id, Some(new));
        Ok(())
    }.await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{result:?}");
}

#[tokio::test]
async fn cancellation_is_atomic_idempotent_and_targets_only_the_selected_run() {
    let (pool, schema) = crate::test_support::test_pool_with_connections(4)
        .await
        .expect("test database");
    let result: Result<(), String> = async {
        let agent = insert_test_agent(&pool, "stop-agent").await?;
        let other_agent = insert_test_agent(&pool, "other-agent").await?;
        let mut pairs = Vec::new();
        for agent_id in [agent, other_agent] {
            let run: Uuid = sqlx::query_scalar("insert into agent_runs (agent_id, command, working_directory, status) values ($1, 'test', '/tmp', 'running') returning id")
                .bind(agent_id).fetch_one(&pool).await.map_err(|e| e.to_string())?;
            let work: Uuid = sqlx::query_scalar("insert into agent_work_items (agent_id, title, status, run_id) values ($1, 'Stop test', 'running', $2) returning id")
                .bind(agent_id).bind(run).fetch_one(&pool).await.map_err(|e| e.to_string())?;
            pairs.push((run, work));
        }
        let (run, work) = pairs[0];
        let (left, right) = tokio::join!(super::cancel_agent_work_in_pool(&pool, work), super::cancel_agent_work_in_pool(&pool, work));
        left?; right?;
        let status: String = sqlx::query_scalar("select status from agent_work_items where id = $1").bind(work).fetch_one(&pool).await.map_err(|e| e.to_string())?;
        assert_eq!(status, "cancelling");
        let commands: Vec<Uuid> = sqlx::query_scalar("select run_id from supervisor_commands where command_type = 'stop_run'").fetch_all(&pool).await.map_err(|e| e.to_string())?;
        assert_eq!(commands, vec![run]);
        let other_status: String = sqlx::query_scalar("select status from agent_work_items where id = $1").bind(pairs[1].1).fetch_one(&pool).await.map_err(|e| e.to_string())?;
        assert_eq!(other_status, "running");
        sqlx::query("update agent_runs set status = 'cancelled', stopped_at = strftime('%Y-%m-%dT%H:%M:%f+00:00','now') where id = $1").bind(run).execute(&pool).await.map_err(|e| e.to_string())?;
        super::cancel_agent_work_in_pool(&pool, work).await?;
        super::cancel_agent_work_in_pool(&pool, work).await?;
        let status: String = sqlx::query_scalar("select status from agent_work_items where id = $1").bind(work).fetch_one(&pool).await.map_err(|e| e.to_string())?;
        assert_eq!(status, "cancelled");
        sqlx::query("update agent_work_items set status = 'done' where id = $1").bind(work).execute(&pool).await.map_err(|e| e.to_string())?;
        super::cancel_agent_work_in_pool(&pool, work).await?;
        let status: String = sqlx::query_scalar("select status from agent_work_items where id = $1").bind(work).fetch_one(&pool).await.map_err(|e| e.to_string())?;
        assert_eq!(status, "done", "a late stop must not overwrite completion");
        Ok(())
    }.await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{result:?}");
}

#[tokio::test]
async fn old_failures_keep_their_reason_and_live_work_survives_busy_history() {
    let (pool, schema) = test_pool().await.expect("test database");
    let result: Result<(), String> = async {
        let agent = insert_test_agent(&pool, "failure-history-agent").await?;
        let run: Uuid = sqlx::query_scalar("insert into agent_runs (agent_id, command, working_directory, status) values ($1, 'test', '/tmp', 'failed') returning id")
            .bind(agent).fetch_one(&pool).await.map_err(|e| e.to_string())?;
        let failure: Uuid = sqlx::query_scalar("insert into agent_work_items (agent_id, title, status, run_id, created_at) values ($1, 'Old failure', 'failed', $2, '2026-01-01T00:00:00Z') returning id")
            .bind(agent).bind(run).fetch_one(&pool).await.map_err(|e| e.to_string())?;
        let queued: Uuid = sqlx::query_scalar("insert into agent_work_items (agent_id, title, status, created_at) values ($1, 'Old queued work', 'queued', '2026-01-01T00:00:00Z') returning id")
            .bind(agent).fetch_one(&pool).await.map_err(|e| e.to_string())?;
        let detail = r#"{"error":{"message":"Provider connection closed"}}"#;
        crate::events::activity::record_agent_activity(&pool, Some(agent), Some(run), "run_error", "Failed", detail.to_owned()).await?;
        for _ in 0..90 {
            sqlx::query("insert into agent_work_items (agent_id, title, status) values ($1, 'New completed work', 'done')")
                .bind(agent).execute(&pool).await.map_err(|e| e.to_string())?;
        }
        let items = crate::activity_store::load_agent_work_item_summaries(&pool).await?;
        assert_eq!(items.len(), 82);
        let failed = items.iter().find(|item| item.id == failure).expect("old failure remains in bootstrap");
        assert_eq!(failed.failure_detail, detail);
        assert!(items.iter().any(|item| item.id == queued), "live work remains in bootstrap");
        Ok(())
    }.await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{result:?}");
}
