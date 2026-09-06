use super::{
    agent_context_agent_inspect, agent_context_attachment_info, agent_context_github_sync,
    agent_context_history_read, agent_context_inbox_archive, agent_context_inbox_list,
    agent_context_inbox_read, agent_context_memory_read, agent_context_message_search,
    agent_context_run_read, agent_context_wiki_log, agent_context_wiki_read,
    agent_context_wiki_write, agent_context_workspace_info, agent_context_workspace_list,
    render_agent_context_github_sync, short_id,
};
use crate::channels::open_dm_with_agent_in_pool;
use crate::events::activity::record_agent_activity;
use crate::github::GithubReviewAttentionRefreshResult;
use crate::message_store::send_owner_message_in_pool;
use crate::models::AttachmentUpload;
use crate::test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool};
use crate::text::read_compact_memory_file;
use uuid::Uuid;

#[test]
fn github_sync_result_is_machine_readable() {
    let channel_id = Uuid::new_v4();
    let output = render_agent_context_github_sync(
        "#github-tools",
        &GithubReviewAttentionRefreshResult {
            channel_id,
            repository: "acme/stream".to_owned(),
            review_login: "octocat".to_owned(),
            review_request_count: 3,
            new_unread_count: 2,
            unread_count: 2,
            baseline_established: false,
        },
    )
    .expect("render GitHub sync result");
    let value: serde_json::Value = serde_json::from_str(&output).expect("parse GitHub sync JSON");
    assert_eq!(value["channel"], "#github-tools");
    assert_eq!(value["channel_id"], channel_id.to_string());
    assert_eq!(value["repository"], "acme/stream");
    assert_eq!(value["review_requests"], 3);
    assert_eq!(value["new_unread"], 2);
    assert_eq!(value["unread"], 2);
    assert_eq!(value["baseline_established"], false);
}

#[tokio::test]
async fn github_sync_requires_a_bound_channel() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        insert_test_channel(&pool, "github-tools-unbound").await?;
        let error = agent_context_github_sync(
            &pool,
            &[
                "github-sync".to_owned(),
                "--channel".to_owned(),
                "#github-tools-unbound".to_owned(),
            ],
        )
        .await
        .expect_err("unbound channel should fail before invoking GitHub");
        assert_eq!(error, "channel has no GitHub repository binding");
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[test]
fn compact_memory_read_preserves_bounded_head_and_tail() {
    let path = std::env::temp_dir().join(format!("lantor-memory-edge-{}.md", Uuid::new_v4()));
    let body = format!("HEAD-{}\nTAIL", "middle🙂".repeat(20_000));
    std::fs::write(&path, &body).unwrap();
    let file_size = std::fs::metadata(&path).unwrap().len();

    let compacted = read_compact_memory_file(&path, file_size, 120).unwrap();
    let _ = std::fs::remove_file(path);

    assert!(compacted.starts_with("HEAD-"));
    assert!(compacted.contains("omitted the middle"));
    assert!(compacted.ends_with("TAIL"));
    assert!(compacted.chars().count() < 300);
}

#[tokio::test]
async fn agent_context_tool_reads_thread_history_and_searches_messages() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let channel_id = insert_test_channel(&pool, "context-tools").await?;
        let root_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'root message with needle', false)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        sqlx::query(
            r#"
            insert into messages (channel_id, thread_root_id, sender_name, sender_role, body, is_task)
            values ($1, $2, 'agent', 'agent', 'reply inside thread', false),
                   ($1, null, 'Dylan', 'owner', 'separate root message', false)
            "#,
        )
        .bind(channel_id)
        .bind(root_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;

        let history_args = vec![
            "history-read".to_owned(),
            "--target".to_owned(),
            format!("#context-tools:{}", short_id(root_id)),
            "--limit".to_owned(),
            "10".to_owned(),
        ];
        let history = agent_context_history_read(&pool, &history_args).await?;
        assert!(history.contains("root message with needle"));
        assert!(history.contains("reply inside thread"));
        assert!(history.contains("[target=#context-tools:"));
        assert!(history.contains(" type=owner] Dylan: root message with needle"));
        assert!(!history.contains("separate root message"));

        let search_args = vec![
            "message-search".to_owned(),
            "--query".to_owned(),
            "needle".to_owned(),
            "--target".to_owned(),
            "#context-tools".to_owned(),
        ];
        let search = agent_context_message_search(&pool, &search_args).await?;
        assert!(search.contains("root message with needle"));
        assert!(search.contains("[target=#context-tools msg="));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn agent_context_tool_reads_workspace_and_memory() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let workspace = std::env::temp_dir().join(format!("lantor-workspace-tool-{}", Uuid::new_v4()));
    let result: Result<(), String> = async {
        std::fs::create_dir_all(workspace.join("notes")).map_err(|err| err.to_string())?;
        std::fs::write(
            workspace.join("MEMORY.md"),
            "# @workspace-agent\n\n## Role\nWorkspace-aware test agent.\n",
        )
        .map_err(|err| err.to_string())?;
        std::fs::write(workspace.join("notes").join("handoff.md"), "handoff note")
            .map_err(|err| err.to_string())?;

        let agent_id = insert_test_agent(&pool, "workspace-agent").await?;
        sqlx::query("update agents set working_directory = $2 where id = $1")
            .bind(agent_id)
            .bind(workspace.to_string_lossy().to_string())
            .execute(&pool)
            .await
            .map_err(|err| err.to_string())?;

        let target_args = vec![
            "workspace-info".to_owned(),
            "--target".to_owned(),
            "@workspace-agent".to_owned(),
        ];
        let workspace_info = agent_context_workspace_info(&pool, &target_args).await?;
        assert!(workspace_info.contains("Lantor workspace for @workspace-agent"));
        assert!(workspace_info.contains("memory_exists=true"));
        assert!(workspace_info.contains("MEMORY.md"));

        let memory_args = vec![
            "memory-read".to_owned(),
            "--target".to_owned(),
            "@workspace-agent".to_owned(),
        ];
        let memory = agent_context_memory_read(&pool, &memory_args).await?;
        assert!(memory.contains("Workspace-aware test agent"));

        let list_args = vec![
            "workspace-list".to_owned(),
            "--target".to_owned(),
            "@workspace-agent".to_owned(),
            "--max-depth".to_owned(),
            "2".to_owned(),
        ];
        let listing = agent_context_workspace_list(&pool, &list_args).await?;
        assert!(listing.contains("notes/"));
        assert!(listing.contains("notes/handoff.md"));
        Ok(())
    }
    .await;
    let _ = std::fs::remove_dir_all(&workspace);
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn history_context_and_attachment_tool_expose_attachment_paths() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let channel_id = insert_test_channel(&pool, "vision-channel").await?;
        let message_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'Please inspect this screenshot', false)
            returning id
            "#,
        )
        .bind(channel_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let image_path = std::env::temp_dir().join(format!("lantor-vision-{}.png", Uuid::new_v4()));
        std::fs::write(&image_path, b"fake image bytes").map_err(|err| err.to_string())?;
        let attachment_id: Uuid = sqlx::query_scalar(
            r#"
            insert into message_attachments (
                message_id, original_name, mime_type, size_bytes, storage_path
            )
            values ($1, 'screen.png', 'image/png', 16, $2)
            returning id
            "#,
        )
        .bind(message_id)
        .bind(image_path.to_string_lossy().to_string())
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        let history = agent_context_history_read(
            &pool,
            &[
                "history-read".to_owned(),
                "--target".to_owned(),
                "#vision-channel".to_owned(),
            ],
        )
        .await?;
        assert!(history.contains(&attachment_id.to_string()));
        assert!(history.contains("local_path="));
        assert!(history.contains("attachment-info"));

        let info = agent_context_attachment_info(
            &pool,
            &[
                "attachment-info".to_owned(),
                "--attachment-id".to_owned(),
                attachment_id.to_string(),
            ],
        )
        .await?;
        assert!(info.contains("mime=image/png"));
        assert!(info.contains("file_exists=true"));
        assert!(info.contains("vision_hint="));
        let _ = std::fs::remove_file(image_path);
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn agent_inspect_tool_summarizes_profile_runs_requests_and_activity() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "inspectable").await?;
        let channel_id = insert_test_channel(&pool, "inspect-channel").await?;
        sqlx::query(
            r#"
            update agents
            set display_name = 'Inspectable Agent',
                role = 'review specialist',
                description = 'Reviews work from other agents.',
                daily_budget_micros = 2500000
            where id = $1
            "#,
        )
        .bind(agent_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (
                agent_id, command, status, input_tokens, output_tokens, cost_micros
            )
            values ($1, 'codex app-server', 'complete', 100, 20, 500)
            returning id
            "#,
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        sqlx::query(
            r#"
            insert into agent_work_items (agent_id, channel_id, title, context, status, source_kind)
            values ($1, $2, 'Review implementation', 'context', 'done', 'collaboration')
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;
        record_agent_activity(
            &pool,
            Some(agent_id),
            Some(run_id),
            "thinking",
            "Reading recent context",
            "{}".to_owned(),
        )
        .await?;

        let output = agent_context_agent_inspect(
            &pool,
            &[
                "agent-inspect".to_owned(),
                "--target".to_owned(),
                "@inspectable".to_owned(),
            ],
        )
        .await?;
        assert!(output.contains("Agent @inspectable"));
        assert!(output.contains("display_name=Inspectable Agent"));
        assert!(output.contains("role=review specialist"));
        assert!(output.contains("recent_runs:"));
        assert!(output.contains("recent_requests:"));
        assert!(output.contains("recent_activity:"));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn run_read_tool_exposes_previous_run_context_on_demand() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "run-reader").await?;
        let channel_id = insert_test_channel(&pool, "run-read-channel").await?;
        let thread_root_id: Uuid = sqlx::query_scalar(
            r#"
            insert into messages (channel_id, sender_name, sender_role, body, is_task)
            values ($1, 'Dylan', 'owner', 'thread root for run-read', false)
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
                agent_id, channel_id, thread_root_id, title, context, status, source_kind
            )
            values ($1, $2, $3, 'Continue rotated work', 'Previous work item context', 'done', 'inbox_wake')
            returning id
            "#,
        )
        .bind(agent_id)
        .bind(channel_id)
        .bind(thread_root_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;
        let run_id: Uuid = sqlx::query_scalar(
            r#"
            insert into agent_runs (
                agent_id, work_item_id, command, working_directory, status, log,
                input_tokens, output_tokens, cost_micros, stopped_at
            )
            values (
                $1, $2, 'codex app-server', '/tmp/lantor-run-read', 'done',
                '[codex] did useful work\n[thinking] next step is cargo test\n',
                190000, 1200, 12345, strftime('%Y-%m-%dT%H:%M:%f+00:00','now')
            )
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
            insert into messages (
                channel_id, thread_root_id, sender_agent_id, sender_name, sender_role,
                body, delivery_state, stream_key
            )
            values ($1, $2, $3, 'run-reader', 'agent', 'Visible reply from previous run', 'complete', $4)
            "#,
        )
        .bind(channel_id)
        .bind(thread_root_id)
        .bind(agent_id)
        .bind(format!("{run_id}:item-1"))
        .execute(&pool)
        .await
        .map_err(|err| err.to_string())?;
        record_agent_activity(
            &pool,
            Some(agent_id),
            Some(run_id),
            "command",
            "Running verification",
            "cargo test".to_owned(),
        )
        .await?;

        let output = agent_context_run_read(
            &pool,
            &[
                "run-read".to_owned(),
                "--target".to_owned(),
                "@run-reader".to_owned(),
                "--run-id".to_owned(),
                short_id(run_id),
                "--log-limit".to_owned(),
                "2000".to_owned(),
            ],
        )
        .await?;

        assert!(output.contains("Lantor run"));
        assert!(output.contains("run_id="));
        assert!(output.contains("tokens=190000/1200"));
        assert!(output.contains("Continue rotated work"));
        assert!(output.contains("Previous work item context"));
        assert!(output.contains("history-read"));
        assert!(output.contains("Visible reply from previous run"));
        assert!(output.contains("Running verification"));
        assert!(output.contains("next step is cargo test"));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn agent_context_inbox_tools_list_read_and_archive_items() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        let agent_id = insert_test_agent(&pool, "inbox-tool").await?;
        let dm_channel_id = Uuid::parse_str(&open_dm_with_agent_in_pool(&pool, agent_id).await?)
            .map_err(|err| err.to_string())?;
        let message = send_owner_message_in_pool(
            &pool,
            dm_channel_id,
            None,
            "please inspect inbox tools",
            false,
            vec![AttachmentUpload {
                original_name: "inbox.txt".to_owned(),
                mime_type: "text/plain".to_owned(),
                bytes: b"inbox context".to_vec(),
                staged: None,
            }],
        )
        .await?;
        let attachment_id = message
            .attachments
            .first()
            .ok_or_else(|| "expected inbox attachment".to_owned())?
            .id;
        let inbox_id: Uuid = sqlx::query_scalar(
            "select id from agent_inbox_items where agent_id = $1 and kind = 'dm'",
        )
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .map_err(|err| err.to_string())?;

        let list = agent_context_inbox_list(
            &pool,
            &[
                "inbox-list".to_owned(),
                "--target".to_owned(),
                "@inbox-tool".to_owned(),
                "--state".to_owned(),
                "active".to_owned(),
            ],
        )
        .await?;
        assert!(list.contains("Lantor inbox for @inbox-tool"));
        assert!(list.contains("kind=dm"));
        assert!(list.contains("please inspect inbox tools"));

        let read = agent_context_inbox_read(
            &pool,
            &[
                "inbox-read".to_owned(),
                "--target".to_owned(),
                "@inbox-tool".to_owned(),
                "--inbox-id".to_owned(),
                short_id(inbox_id),
            ],
        )
        .await?;
        assert!(read.contains("source_message:"));
        assert!(read.contains("please inspect inbox tools"));
        assert!(read.contains(&attachment_id.to_string()));
        assert!(read.contains("inbox.txt"));
        assert!(read.contains("attachment-info"));
        assert!(read.contains(&format!("--target \"{dm_channel_id}")));
        assert!(!read.contains("--target \"dm:"));
        assert!(
            read.contains("archive_note=linked work-item inbox items are archived automatically")
        );
        assert!(!read.contains("archive_when_done="));

        let archived = agent_context_inbox_archive(
            &pool,
            &[
                "inbox-archive".to_owned(),
                "--target".to_owned(),
                "@inbox-tool".to_owned(),
                "--inbox-id".to_owned(),
                short_id(inbox_id),
            ],
        )
        .await?;
        assert!(archived.contains("Archived Lantor inbox item"));
        let state: String = sqlx::query_scalar("select state from agent_inbox_items where id = $1")
            .bind(inbox_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
        assert_eq!(state, "archived");
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}

#[tokio::test]
async fn wiki_tools_write_read_log_and_surface_search_matches() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let result: Result<(), String> = async {
        insert_test_channel(&pool, "wiki-cli").await?;

        let read_empty = agent_context_wiki_read(
            &pool,
            &[
                "wiki-read".to_owned(),
                "--channel".to_owned(),
                "#wiki-cli".to_owned(),
            ],
        )
        .await?;
        assert!(read_empty.contains("has no wiki yet"));

        let write_args = |content: &str, parent: Option<&str>| {
            let mut args = vec![
                "wiki-write".to_owned(),
                "--channel".to_owned(),
                "#wiki-cli".to_owned(),
                "--content".to_owned(),
                content.to_owned(),
                "--note".to_owned(),
                "test edit".to_owned(),
            ];
            if let Some(parent) = parent {
                args.push("--parent".to_owned());
                args.push(parent.to_owned());
            }
            args
        };

        let published =
            agent_context_wiki_write(&pool, &write_args("# Playbook\nzanzibar convention", None))
                .await?;
        assert!(published.contains("Published wiki rev"));

        // Missing --parent while a head exists must fail with the current rev.
        let missing_parent = agent_context_wiki_write(&pool, &write_args("# v2", None)).await;
        let err = missing_parent
            .err()
            .ok_or("second write without --parent must fail")?;
        assert!(err.contains("pass --parent"), "unexpected error: {err}");

        let read = agent_context_wiki_read(
            &pool,
            &[
                "wiki-read".to_owned(),
                "--channel".to_owned(),
                "#wiki-cli".to_owned(),
            ],
        )
        .await?;
        assert!(read.contains("Channel wiki for #wiki-cli"));
        assert!(read.contains("zanzibar convention"));
        assert!(read.contains("note: test edit"));
        let head_rev = read
            .lines()
            .find_map(|line| line.strip_prefix("rev ").map(|rest| rest[..8].to_owned()))
            .ok_or("wiki-read output missing rev line")?;

        let updated =
            agent_context_wiki_write(&pool, &write_args("# Playbook v2", Some(&head_rev))).await?;
        assert!(updated.contains("Published wiki rev"));

        // Reusing the stale parent must report a conflict with merge guidance.
        let conflict =
            agent_context_wiki_write(&pool, &write_args("# stale", Some(&head_rev))).await;
        let err = conflict.err().ok_or("stale parent write must conflict")?;
        assert!(err.contains("wiki head moved"), "unexpected error: {err}");

        let log = agent_context_wiki_log(
            &pool,
            &[
                "wiki-log".to_owned(),
                "--channel".to_owned(),
                "#wiki-cli".to_owned(),
            ],
        )
        .await?;
        assert!(log.contains("2 revisions"));
        assert!(log.contains("[head]"));

        // The head content is searchable; superseded content is not.
        let search = agent_context_message_search(
            &pool,
            &[
                "message-search".to_owned(),
                "--query".to_owned(),
                "Playbook".to_owned(),
                "--target".to_owned(),
                "#wiki-cli".to_owned(),
            ],
        )
        .await?;
        assert!(search.contains("Channel wiki matches (1):"));
        assert!(search.contains("[wiki target=#wiki-cli rev="));
        let stale_search = agent_context_message_search(
            &pool,
            &[
                "message-search".to_owned(),
                "--query".to_owned(),
                "zanzibar".to_owned(),
            ],
        )
        .await?;
        assert!(!stale_search.contains("Channel wiki matches"));
        Ok(())
    }
    .await;
    drop_test_schema(pool, schema).await;
    assert!(result.is_ok(), "{:?}", result.err());
}
