use super::*;

#[tokio::test]
async fn archived_snapshot_uses_uuid_file_names_and_run_start_time() {
    use crate::test_support::{drop_test_schema, insert_test_agent, test_pool};
    let (pool, path) = test_pool().await.unwrap();
    let agent_id = insert_test_agent(&pool, "snapshot-fixture").await.unwrap();
    let config = ProbeConfig {
        agent_id,
        handle: "snapshot-fixture".into(),
        runtime: "claude".into(),
        working_directory: "/synthetic/workspace".into(),
        environment_variables: String::new(),
        model: String::new(),
        reasoning_effort: String::new(),
    };
    let mut result = AgentTools::unavailable(&config);
    load_claude_snapshot(&pool, &config, &mut result).await;
    assert_eq!(result.plugins.state, "not_run");
    let run = Uuid::new_v4();
    sqlx::query("insert into agent_runs(id,agent_id,command,started_at) values($1,$2,'fixture','2026-01-01T00:00:00Z')")
        .bind(run).bind(agent_id).execute(&pool).await.unwrap();
    let dir = PathBuf::from(&path).with_extension("run-logs");
    std::fs::create_dir_all(&dir).unwrap();
    let init = serde_json::json!({"type":"system","subtype":"init","cwd":"/synthetic/workspace",
        "plugins":[{"name":"user@market"},{"name":"internal","source":"internal@builtin","path":"builtin"}],"skills":["workspace-skill"],
        "mcp_servers":[{"name":"stale-server","status":"connected"}]});
    std::fs::write(dir.join(format!("{run}.log")), format!("[claude] {init}\n")).unwrap();
    load_claude_snapshot(&pool, &config, &mut result).await;
    assert_eq!(result.plugins.state, "ready");
    assert_eq!(result.plugins.items.iter().filter(|p| p.builtin).count(), 1);
    assert_eq!(
        result.plugins.observed_at.as_deref(),
        Some("2026-01-01T00:00:00Z")
    );
    assert_eq!(result.skills.items[0].name, "workspace-skill");
    assert_eq!(
        result.mcp.state, "unavailable",
        "historical MCP status is never used"
    );
    drop_test_schema(pool, path).await;
}

#[tokio::test]
async fn watched_apps_default_normalize_and_survive_an_upgrade() {
    use crate::agent_profile::load_owner_profile;
    use crate::test_support::{drop_test_schema, test_pool};
    let (pool, path) = test_pool().await.unwrap();
    sqlx::query("alter table owner_profile drop column watched_apps")
        .execute(&pool)
        .await
        .unwrap();
    crate::db::migrate(&pool).await.unwrap();
    assert_eq!(
        load_owner_profile(&pool).await.unwrap().watched_apps,
        ["Notion", "Slack", "Gmail"]
    );
    let saved = update_watched_apps(
        &pool,
        WatchedAppsRequest {
            apps: vec![
                "  Google   Drive ".into(),
                "Notion".into(),
                "notion".into(),
                " ".into(),
            ],
        },
    )
    .await
    .unwrap();
    assert_eq!(saved, ["Google Drive", "Notion"]);
    assert_eq!(load_owner_profile(&pool).await.unwrap().watched_apps, saved);
    for apps in [
        vec!["x".repeat(61)],
        (0..31).map(|n| format!("App {n}")).collect(),
    ] {
        assert!(update_watched_apps(&pool, WatchedAppsRequest { apps })
            .await
            .is_err());
    }
    assert_eq!(load_owner_profile(&pool).await.unwrap().watched_apps, saved);
    drop_test_schema(pool, path).await;
}

#[tokio::test]
async fn cache_coalesces_refreshes_and_invalidates_changed_configuration() {
    let cache = ToolsCache::default();
    let config = ProbeConfig {
        agent_id: Uuid::new_v4(),
        handle: "fixture".into(),
        runtime: "codex".into(),
        working_directory: "/synthetic".into(),
        environment_variables: String::new(),
        model: String::new(),
        reasoning_effort: String::new(),
    };
    let key = (PathBuf::from("/synthetic/database"), config.agent_id);
    let requested = Instant::now();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let discover = || async {
        let _ = started_tx.send(());
        let _ = release_rx.await;
        let mut result = AgentTools::unavailable(&config);
        result.checked_at = "first".into();
        result
    };
    let first = cache.obtain(key.clone(), config.clone(), requested, true, discover);
    let second = async {
        started_rx.await.unwrap();
        let _ = release_tx.send(());
        cache
            .obtain(key.clone(), config.clone(), requested, true, || async {
                panic!("an overlapping refresh must return the completed discovery");
            })
            .await
    };
    let (first, second) = tokio::join!(first, second);
    assert_eq!(first.checked_at, "first");
    assert_eq!(second.checked_at, "first");
    let cached = cache
        .obtain(
            key.clone(),
            config.clone(),
            Instant::now(),
            false,
            || async {
                panic!("fresh cache");
            },
        )
        .await;
    assert_eq!(cached.checked_at, "first");
    let mut changed = config.clone();
    changed.environment_variables = "FIXTURE=changed".into();
    let changed_result = cache
        .obtain(
            key.clone(),
            changed.clone(),
            Instant::now(),
            false,
            || async {
                let mut result = AgentTools::unavailable(&changed);
                result.checked_at = "changed".into();
                result
            },
        )
        .await;
    assert_eq!(changed_result.checked_at, "changed");
    let refreshed = cache
        .obtain(key, changed.clone(), Instant::now(), true, || async {
            let mut result = AgentTools::unavailable(&changed);
            result.checked_at = "refreshed".into();
            result
        })
        .await;
    assert_eq!(refreshed.checked_at, "refreshed");
}

#[test]
fn history_skips_repo_init_and_uses_the_latest_workspace_init() {
    let dir = std::env::temp_dir().join(format!("lantor-tools-history-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let older = dir.join("older.log");
    let newer = dir.join("newer.log");
    std::fs::write(&older, "[claude] {\"type\":\"system\",\"subtype\":\"init\",\"cwd\":\"/synthetic/workspace\",\"skills\":[\"workspace-skill\"],\"plugins\":[{\"name\":\"installed\"},{\"name\":\"internal@builtin\"}]}\n").unwrap();
    std::fs::write(&newer, "[claude] {\"type\":\"system\",\"subtype\":\"init\",\"cwd\":\"/synthetic/workspace/code/repo\",\"skills\":[\"repo-skill\"]}\n").unwrap();
    let found = latest_workspace_init(
        vec![(newer, "new".into()), (older, "old".into())],
        "/synthetic/workspace",
    )
    .unwrap()
    .unwrap();
    assert_eq!(found.1, "old");
    assert_eq!(found.0["skills"], serde_json::json!(["workspace-skill"]));
    std::fs::remove_dir_all(dir).unwrap();
}

// Explicit opt-in only: calls discovery directly, never starts the application or supervisor.
// LANTOR_TOOLS_TEST_DB and LANTOR_TOOLS_TEST_AGENT select a read-only source.
#[tokio::test]
#[ignore]
async fn real_cli_inventory() {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    let path = std::env::var("LANTOR_TOOLS_TEST_DB").expect("explicit database path");
    let handle = std::env::var("LANTOR_TOOLS_TEST_AGENT").expect("explicit agent handle");
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(SqliteConnectOptions::new().filename(path).read_only(true))
        .await
        .unwrap();
    let agent_id = sqlx::query_scalar("select id from agents where handle=$1")
        .bind(&handle)
        .fetch_one(&pool)
        .await
        .unwrap();
    let before: i64 = sqlx::query_scalar("select count(*) from agent_runs where agent_id=$1")
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let result = load_cached(
        &pool,
        AgentToolsRequest {
            agent_id,
            refresh: true,
        },
        &ToolsCache::default(),
    )
    .await
    .unwrap();
    println!(
        "{}",
        serde_json::json!({"runtime":result.runtime,"apps_state":result.apps.state,
        "apps":result.apps.items.iter().map(|a|serde_json::json!({"name":a.name,"status":a.status,"source":a.source,"tools":a.tool_count,"plugin":a.plugin})).collect::<Vec<_>>(),
        "mcp_state":result.mcp.state,
        "servers":result.mcp.items.iter().map(|s|serde_json::json!({"name":s.name,"status":s.status,"tools":s.tools.len(),"auth":s.auth_status,"error":s.tools_error})).collect::<Vec<_>>(),
        "plugins_state":result.plugins.state,"installed_plugins":result.plugins.items.iter().filter(|p|!p.builtin).count(),
        "builtin_plugins":result.plugins.items.iter().filter(|p|p.builtin).count(),"skills_state":result.skills.state,"skills":result.skills.items.len()})
    );
    assert_eq!(result.apps.state, "ready");
    assert_eq!(result.mcp.state, "ready");
    assert_eq!(result.plugins.state, "ready");
    assert_eq!(result.skills.state, "ready");
    let after: i64 = sqlx::query_scalar("select count(*) from agent_runs where agent_id=$1")
        .bind(agent_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(before, after, "discovery creates no run");
    pool.close().await;
}
