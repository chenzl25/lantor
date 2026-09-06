use std::{
    env,
    net::{IpAddr, SocketAddr},
    time::Instant,
};

use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{
    activity_store::{
        load_agent_activities, load_agent_run_summaries, load_agent_runs, load_agent_work_items,
    },
    agent_profile::{load_agents, load_owner_profile},
    app::{to_string, CommandResult},
    channels::{load_channel_members, load_channels, load_thread_activities},
    domain::{reminders::load_reminders, schedules::load_agent_schedules},
    launch_agent,
    message_store::{
        channel_message_history_from_messages, load_artifact_summaries, load_artifacts,
        load_bootstrap_messages, load_messages, load_saved_messages, COMPACT_BOOTSTRAP_ROOTS,
    },
    models::{
        Bootstrap, BootstrapPerf, BootstrapPerfCounts, BootstrapPerfOptions, BootstrapPerfPhase,
    },
    owner_inbox::{load_dismissed_inbox_items, load_read_inbox_items},
    runtime::supervisor::load_supervisor_status,
    task_store::load_tasks,
    web,
};

fn configured_web_base_url() -> Option<String> {
    if let Ok(value) = env::var("LANTOR_WEB_PUBLIC_URL") {
        let trimmed = value.trim().trim_end_matches('/').to_owned();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    let bind = web::resolve_web_bind()?;
    let addr = bind.parse::<SocketAddr>().ok()?;
    let host = match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => "127.0.0.1".to_owned(),
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) if ip.is_unspecified() => "[::1]".to_owned(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    Some(format!("http://{host}:{}", addr.port()))
}

pub(crate) async fn load_bootstrap(pool: &SqlitePool, db_url: String) -> CommandResult<Bootstrap> {
    load_bootstrap_with_options(
        pool,
        db_url,
        BootstrapLoadOptions {
            runtime: "tauri",
            messages: BootstrapMessageLoad::All,
            include_run_logs: true,
            compact_agent_activities: false,
            include_artifact_content: true,
        },
    )
    .await
}

pub(crate) async fn load_tauri_bootstrap(
    pool: &SqlitePool,
    db_url: String,
    requested_channel_id: Option<Uuid>,
    current_channel_only: bool,
) -> CommandResult<Bootstrap> {
    if !current_channel_only {
        return load_bootstrap(pool, db_url).await;
    }
    load_bootstrap_with_options(
        pool,
        db_url,
        BootstrapLoadOptions {
            runtime: "tauri",
            messages: BootstrapMessageLoad::RecentChannel(
                requested_channel_id,
                COMPACT_BOOTSTRAP_ROOTS,
            ),
            include_run_logs: false,
            compact_agent_activities: true,
            include_artifact_content: false,
        },
    )
    .await
}

pub(crate) async fn load_web_bootstrap(
    pool: &SqlitePool,
    db_url: String,
    requested_channel_id: Option<Uuid>,
    current_channel_only: bool,
) -> CommandResult<Bootstrap> {
    let messages = if current_channel_only {
        BootstrapMessageLoad::RecentChannel(requested_channel_id, COMPACT_BOOTSTRAP_ROOTS)
    } else {
        BootstrapMessageLoad::RecentPerChannel(COMPACT_BOOTSTRAP_ROOTS)
    };
    load_bootstrap_with_options(
        pool,
        db_url,
        BootstrapLoadOptions {
            runtime: "web",
            messages,
            include_run_logs: false,
            compact_agent_activities: true,
            include_artifact_content: false,
        },
    )
    .await
}

struct BootstrapLoadOptions {
    runtime: &'static str,
    messages: BootstrapMessageLoad,
    include_run_logs: bool,
    compact_agent_activities: bool,
    include_artifact_content: bool,
}

#[derive(Clone, Copy)]
enum BootstrapMessageLoad {
    All,
    RecentPerChannel(i64),
    RecentChannel(Option<Uuid>, i64),
}

impl BootstrapMessageLoad {
    fn label(self) -> String {
        match self {
            BootstrapMessageLoad::All => "All".to_owned(),
            BootstrapMessageLoad::RecentPerChannel(limit) => {
                format!("RecentPerChannel({limit})")
            }
            BootstrapMessageLoad::RecentChannel(_, limit) => format!("RecentChannel({limit})"),
        }
    }
}

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn push_phase(
    phases: &mut Vec<BootstrapPerfPhase>,
    name: &str,
    started_at: Instant,
    rows: Option<usize>,
) {
    phases.push(BootstrapPerfPhase {
        name: name.to_owned(),
        duration_ms: elapsed_ms(started_at),
        rows,
    });
}

fn should_measure_bootstrap_payload() -> bool {
    env::var("LANTOR_PERF").is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        || env::var("LANTOR_PERF_PAYLOAD")
            .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

async fn load_bootstrap_with_options(
    pool: &SqlitePool,
    db_url: String,
    options: BootstrapLoadOptions,
) -> CommandResult<Bootstrap> {
    let total_started_at = Instant::now();
    let mut phases = Vec::new();

    // Capture the event cursor before reading the snapshot. Any event committed
    // while the snapshot is loading can then be replayed by the web client.
    let started_at = Instant::now();
    let ui_event_cursor: i64 = sqlx::query_scalar("select coalesce(max(id), 0) from ui_events")
        .fetch_one(pool)
        .await
        .map_err(to_string)?;
    push_phase(&mut phases, "ui_event_cursor", started_at, None);

    let started_at = Instant::now();
    let owner_profile = load_owner_profile(pool).await?;
    push_phase(&mut phases, "owner_profile", started_at, Some(1));

    let started_at = Instant::now();
    let channels = load_channels(pool).await?;
    push_phase(&mut phases, "channels", started_at, Some(channels.len()));

    let started_at = Instant::now();
    let mut thread_activities = load_thread_activities(pool).await?;
    push_phase(
        &mut phases,
        "thread_activities",
        started_at,
        Some(thread_activities.len()),
    );

    let started_at = Instant::now();
    let channel_members = load_channel_members(pool).await?;
    push_phase(
        &mut phases,
        "channel_members",
        started_at,
        Some(channel_members.len()),
    );

    let started_at = Instant::now();
    let compact = options.compact_agent_activities;
    let agents = if compact {
        crate::agent_profile::load_agent_summaries(pool).await?
    } else {
        load_agents(pool).await?
    };
    push_phase(&mut phases, "agents", started_at, Some(agents.len()));

    let started_at = Instant::now();
    let messages = match options.messages {
        BootstrapMessageLoad::All => load_messages(pool).await?,
        BootstrapMessageLoad::RecentPerChannel(limit) => {
            load_bootstrap_messages(pool, None, limit).await?
        }
        BootstrapMessageLoad::RecentChannel(requested_channel_id, limit) => {
            let channel_id = requested_channel_id
                .filter(|channel_id| channels.iter().any(|channel| channel.id == *channel_id))
                .or_else(|| channels.first().map(|channel| channel.id));
            match channel_id {
                Some(channel_id) => load_bootstrap_messages(pool, Some(channel_id), limit).await?,
                None => Vec::new(),
            }
        }
    };
    let channel_message_history = match options.messages {
        BootstrapMessageLoad::All => Vec::new(),
        BootstrapMessageLoad::RecentPerChannel(limit) => {
            channel_message_history_from_messages(&messages, limit)
        }
        BootstrapMessageLoad::RecentChannel(_, limit) => {
            channel_message_history_from_messages(&messages, limit)
        }
    };
    push_phase(&mut phases, "messages", started_at, Some(messages.len()));

    let started_at = Instant::now();
    let saved_messages = load_saved_messages(pool).await?;
    push_phase(
        &mut phases,
        "saved_messages",
        started_at,
        Some(saved_messages.len()),
    );

    let started_at = Instant::now();
    let mut dismissed_inbox_items = load_dismissed_inbox_items(pool).await?;
    push_phase(
        &mut phases,
        "dismissed_inbox_items",
        started_at,
        Some(dismissed_inbox_items.len()),
    );

    let started_at = Instant::now();
    let mut read_inbox_items = load_read_inbox_items(pool).await?;
    push_phase(
        &mut phases,
        "read_inbox_items",
        started_at,
        Some(read_inbox_items.len()),
    );

    let started_at = Instant::now();
    let artifacts = if options.include_artifact_content {
        load_artifacts(pool).await?
    } else {
        load_artifact_summaries(pool).await?
    };
    push_phase(&mut phases, "artifacts", started_at, Some(artifacts.len()));

    let started_at = Instant::now();
    let tasks = load_tasks(pool).await?;
    push_phase(&mut phases, "tasks", started_at, Some(tasks.len()));

    let started_at = Instant::now();
    let reminders = load_reminders(pool).await?;
    push_phase(&mut phases, "reminders", started_at, Some(reminders.len()));

    let started_at = Instant::now();
    let agent_schedules = load_agent_schedules(pool).await?;
    push_phase(
        &mut phases,
        "agent_schedules",
        started_at,
        Some(agent_schedules.len()),
    );

    let started_at = Instant::now();
    let agent_runs = if options.include_run_logs {
        load_agent_runs(pool).await?
    } else {
        load_agent_run_summaries(pool).await?
    };
    push_phase(
        &mut phases,
        "agent_runs",
        started_at,
        Some(agent_runs.len()),
    );

    let started_at = Instant::now();
    let agent_work_items = if compact {
        crate::activity_store::load_agent_work_item_summaries(pool).await?
    } else {
        load_agent_work_items(pool).await?
    };
    push_phase(
        &mut phases,
        "agent_work_items",
        started_at,
        Some(agent_work_items.len()),
    );

    let started_at = Instant::now();
    let agent_activities = if options.compact_agent_activities {
        crate::activity_store::load_bootstrap_activities(pool).await?
    } else {
        load_agent_activities(pool).await?
    };
    push_phase(
        &mut phases,
        "agent_activities",
        started_at,
        Some(agent_activities.len()),
    );

    let started_at = Instant::now();
    let supervisor = load_supervisor_status(pool).await?;
    push_phase(&mut phases, "supervisor", started_at, None);

    let started_at = Instant::now();
    let launch_agent = launch_agent::load_launch_agent_status()?;
    push_phase(&mut phases, "launch_agent", started_at, None);

    if compact {
        let message_ids: std::collections::HashSet<_> =
            messages.iter().map(|message| message.id).collect();
        thread_activities.retain(|activity| {
            activity.unread_count > 0 || message_ids.contains(&activity.thread_root_id)
        });
        let mut relevant_ids: std::collections::HashSet<String> =
            message_ids.iter().map(ToString::to_string).collect();
        relevant_ids.extend(channels.iter().map(|item| item.id.to_string()));
        relevant_ids.extend(tasks.iter().map(|item| item.id.to_string()));
        relevant_ids.extend(reminders.iter().map(|item| item.id.to_string()));
        relevant_ids.extend(agent_work_items.iter().map(|item| item.id.to_string()));
        relevant_ids.extend(
            thread_activities
                .iter()
                .map(|item| item.thread_root_id.to_string()),
        );
        let relevant = |key: &String| {
            key == "saved-messages" || key.rsplit(':').next().is_some_and(|id| relevant_ids.contains(id))
        };
        read_inbox_items.retain(|key, _| relevant(key));
        dismissed_inbox_items.retain(|key, _| relevant(key));
    }

    let counts = BootstrapPerfCounts {
        channels: channels.len(),
        thread_activities: thread_activities.len(),
        channel_members: channel_members.len(),
        agents: agents.len(),
        messages: messages.len(),
        saved_messages: saved_messages.len(),
        dismissed_inbox_items: dismissed_inbox_items.len(),
        read_inbox_items: read_inbox_items.len(),
        artifacts: artifacts.len(),
        tasks: tasks.len(),
        reminders: reminders.len(),
        agent_schedules: agent_schedules.len(),
        agent_runs: agent_runs.len(),
        agent_work_items: agent_work_items.len(),
        agent_activities: agent_activities.len(),
    };

    let mut bootstrap = Bootstrap {
        db_url,
        web_base_url: configured_web_base_url(),
        owner_profile,
        channels,
        thread_activities,
        channel_members,
        agents,
        messages,
        channel_message_history,
        saved_messages,
        dismissed_inbox_items,
        read_inbox_items,
        artifacts,
        tasks,
        reminders,
        agent_schedules,
        agent_runs,
        agent_work_items,
        agent_activities,
        supervisor,
        launch_agent,
        ui_event_cursor,
        perf: None,
    };

    let (serialize_ms, payload_bytes) = if should_measure_bootstrap_payload() {
        let serialize_started_at = Instant::now();
        let payload_bytes = serde_json::to_vec(&bootstrap)
            .map(|payload| payload.len())
            .ok();
        (Some(elapsed_ms(serialize_started_at)), payload_bytes)
    } else {
        (None, None)
    };
    bootstrap.perf = Some(BootstrapPerf {
        options: BootstrapPerfOptions {
            runtime: options.runtime.to_owned(),
            message_load: options.messages.label(),
            include_run_logs: options.include_run_logs,
            compact_agent_activities: options.compact_agent_activities,
            include_artifact_content: options.include_artifact_content,
        },
        total_ms: elapsed_ms(total_started_at),
        serialize_ms,
        payload_bytes,
        phases,
        counts,
    });

    Ok(bootstrap)
}

#[cfg(test)]
mod tests {
    use crate::test_support::{drop_test_schema, insert_test_channel, test_pool};

    use super::{load_bootstrap, load_tauri_bootstrap, load_web_bootstrap};

    #[tokio::test]
    async fn bootstrap_only_scopes_messages_for_opted_in_clients() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let requested_channel_id = insert_test_channel(&pool, "bootstrap-requested").await?;
            let other_channel_id = insert_test_channel(&pool, "bootstrap-other").await?;
            for (channel_id, body) in [
                (requested_channel_id, "requested message"),
                (other_channel_id, "other message"),
            ] {
                sqlx::query(
                    r#"
                    insert into messages (
                        channel_id, sender_name, sender_role, body, is_task, created_at
                    )
                    values ($1, 'Dylan', 'owner', $2, false, '2026-01-01T00:00:00.000+00:00')
                    "#,
                )
                .bind(channel_id)
                .bind(body)
                .execute(&pool)
                .await
                .map_err(|err| err.to_string())?;
            }
            let expected_event_cursor: i64 = sqlx::query_scalar(
                "insert into ui_events (event_json) values ('{\"type\":\"refresh\"}') returning id",
            )
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let legacy = load_web_bootstrap(&pool, "sqlite:test".to_owned(), None, false).await?;
            assert_eq!(legacy.ui_event_cursor, expected_event_cursor);
            assert!(legacy
                .messages
                .iter()
                .any(|message| message.body == "requested message"));
            assert!(legacy
                .messages
                .iter()
                .any(|message| message.body == "other message"));

            let web = load_web_bootstrap(
                &pool,
                "sqlite:test".to_owned(),
                Some(requested_channel_id),
                true,
            )
            .await?;
            assert!(web
                .messages
                .iter()
                .all(|message| message.channel_id == requested_channel_id));
            assert!(web
                .messages
                .iter()
                .any(|message| message.body == "requested message"));
            assert!(!web
                .messages
                .iter()
                .any(|message| message.body == "other message"));

            let desktop = load_tauri_bootstrap(
                &pool,
                "sqlite:test".to_owned(),
                Some(requested_channel_id),
                true,
            )
            .await?;
            assert!(desktop
                .messages
                .iter()
                .all(|message| message.channel_id == requested_channel_id));
            assert!(desktop
                .messages
                .iter()
                .any(|message| message.body == "requested message"));
            assert!(!desktop
                .messages
                .iter()
                .any(|message| message.body == "other message"));
            assert_eq!(
                desktop
                    .perf
                    .as_ref()
                    .expect("desktop bootstrap should report perf options")
                    .options
                    .runtime,
                "tauri"
            );
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn web_bootstrap_omits_artifact_content() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: Result<(), String> = async {
            let channel_id = insert_test_channel(&pool, "web-artifacts").await?;
            let message_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into messages (
                    channel_id, sender_name, sender_role, body, is_task, created_at
                )
                values ($1, 'Dylan', 'owner', 'artifact message', false, '2026-01-01T00:00:00.000+00:00')
                returning id
                "#,
            )
            .bind(channel_id)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;
            let artifact_content = "large artifact content that should not ride the web bootstrap";
            let artifact_id: uuid::Uuid = sqlx::query_scalar(
                r#"
                insert into artifacts (
                    message_id, channel_id, kind, title, summary, content
                )
                values ($1, $2, 'markdown', 'Large report', 'short summary', $3)
                returning id
                "#,
            )
            .bind(message_id)
            .bind(channel_id)
            .bind(artifact_content)
            .fetch_one(&pool)
            .await
            .map_err(|err| err.to_string())?;

            let desktop = load_bootstrap(&pool, "sqlite:test".to_owned()).await?;
            let desktop_artifact = desktop
                .artifacts
                .iter()
                .find(|artifact| artifact.id == artifact_id)
                .expect("desktop bootstrap should include the artifact");
            assert_eq!(desktop_artifact.content, artifact_content);
            let desktop_message_artifact = desktop
                .messages
                .iter()
                .find(|message| message.id == message_id)
                .and_then(|message| message.artifacts.iter().find(|artifact| artifact.id == artifact_id))
                .expect("desktop bootstrap should include the nested message artifact");
            assert_eq!(desktop_message_artifact.content, artifact_content);

            let compact_desktop = load_tauri_bootstrap(
                &pool,
                "sqlite:test".to_owned(),
                Some(channel_id),
                true,
            )
            .await?;
            let compact_desktop_artifact = compact_desktop
                .artifacts
                .iter()
                .find(|artifact| artifact.id == artifact_id)
                .expect("compact desktop bootstrap should include artifact metadata");
            assert_eq!(compact_desktop_artifact.summary, "short summary");
            assert_eq!(compact_desktop_artifact.content, "");
            let compact_desktop_message_artifact = compact_desktop
                .messages
                .iter()
                .find(|message| message.id == message_id)
                .and_then(|message| message.artifacts.iter().find(|artifact| artifact.id == artifact_id))
                .expect("compact desktop bootstrap should include nested artifact metadata");
            assert_eq!(compact_desktop_message_artifact.summary, "short summary");
            assert_eq!(compact_desktop_message_artifact.content, "");

            let web = load_web_bootstrap(
                &pool,
                "sqlite:test".to_owned(),
                Some(channel_id),
                true,
            )
            .await?;
            let web_artifact = web
                .artifacts
                .iter()
                .find(|artifact| artifact.id == artifact_id)
                .expect("web bootstrap should include artifact metadata");
            assert_eq!(web_artifact.summary, "short summary");
            assert_eq!(web_artifact.content, "");
            let web_message_artifact = web
                .messages
                .iter()
                .find(|message| message.id == message_id)
                .and_then(|message| message.artifacts.iter().find(|artifact| artifact.id == artifact_id))
                .expect("web bootstrap should include nested artifact metadata");
            assert_eq!(web_message_artifact.summary, "short summary");
            assert_eq!(web_message_artifact.content, "");
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }
}
