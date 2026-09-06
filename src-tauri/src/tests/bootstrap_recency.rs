use std::{path::PathBuf, sync::Arc, time::Instant};

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use tower::ServiceExt;

use super::{web_router, WebState};
use crate::{
    activity_store::{load_agent_run_summaries, load_agent_runs, load_agent_work_items},
    db::{db_connect_with_url, migrate},
    test_support::{drop_test_schema, insert_test_agent, test_pool},
};

// Match the bootstrap readers' wide projections, joins, ordering and limits.
const RECENT_QUERIES: [(&str, &str); 2] = [
    (
        "agent_runs_started_idx",
        "select r.*, a.handle from agent_runs r join agents a on a.id = r.agent_id \
         order by r.started_at desc limit 30",
    ),
    (
        "agent_work_items_created_idx",
        "select w.*, a.handle, c.name, t.number from agent_work_items w \
         join agents a on a.id = w.agent_id \
         left join channels c on c.id = w.channel_id \
         left join tasks t on t.id = w.task_id order by w.created_at desc limit 80",
    ),
];

async fn query_plans(pool: &SqlitePool) -> Vec<Vec<String>> {
    let mut plans = Vec::new();
    for (_, query) in RECENT_QUERIES {
        plans.push(
            sqlx::query(&format!("explain query plan {query}"))
                .fetch_all(pool)
                .await
                .unwrap()
                .iter()
                .map(|row| row.get("detail"))
                .collect(),
        );
    }
    plans
}

async fn drop_recency_indexes(pool: &SqlitePool) {
    for (index, _) in RECENT_QUERIES {
        sqlx::query(&format!("drop index {index}"))
            .execute(pool)
            .await
            .unwrap();
    }
}

struct BenchmarkDatabase(PathBuf);

impl Drop for BenchmarkDatabase {
    fn drop(&mut self) {
        // The backup can contain private data; clean it up even if an assertion fails.
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

#[tokio::test]
async fn recency_indexes_upgrade_existing_databases_without_changing_recent_results() {
    let (pool, path) = test_pool().await.expect("create migrated SQLite fixture");
    // DROP (without IF EXISTS) also verifies installation on a fresh database.
    drop_recency_indexes(&pool).await;
    let agent_id = insert_test_agent(&pool, "recency-test").await.unwrap();
    for index in (0..100).rev() {
        let timestamp = format!("+{index} seconds");
        sqlx::query(
            "insert into agent_runs (agent_id, command, log, started_at) \
             values ($1, $2, 'fixture log', strftime('%Y-%m-%dT%H:%M:%f+00:00', '2026-01-01', $3))",
        )
        .bind(agent_id)
        .bind(format!("run-{index}"))
        .bind(&timestamp)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "insert into agent_work_items (agent_id, title, context, created_at) \
             values ($1, $2, 'fixture context', strftime('%Y-%m-%dT%H:%M:%f+00:00', '2026-01-01', $3))",
        )
        .bind(agent_id)
        .bind(format!("work-{index}"))
        .bind(&timestamp)
        .execute(&pool)
        .await
        .unwrap();
    }

    let before_runs = load_agent_runs(&pool).await.unwrap();
    let before_work = load_agent_work_items(&pool).await.unwrap();
    assert_eq!(
        before_runs
            .iter()
            .map(|run| run.command.clone())
            .collect::<Vec<_>>(),
        (70..100)
            .rev()
            .map(|index| format!("run-{index}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        before_work
            .iter()
            .map(|work| work.title.clone())
            .collect::<Vec<_>>(),
        (20..100)
            .rev()
            .map(|index| format!("work-{index}"))
            .collect::<Vec<_>>()
    );
    for plan in query_plans(&pool).await {
        assert!(
            plan.iter().any(|step| step.contains("TEMP B-TREE")),
            "{plan:?}"
        );
    }

    migrate(&pool).await.unwrap();
    migrate(&pool).await.unwrap(); // Existing databases and repeated startup are both supported.
    for ((index, _), plan) in RECENT_QUERIES.into_iter().zip(query_plans(&pool).await) {
        // SQLite calls an ordered index traversal SCAN ... USING INDEX. It is the
        // full table scan plus temporary sort that this migration must eliminate.
        assert!(plan.iter().any(|step| step.contains(index)), "{plan:?}");
        assert!(
            !plan.iter().any(|step| step.contains("TEMP B-TREE")),
            "{plan:?}"
        );
    }
    assert_eq!(
        serde_json::to_value(load_agent_runs(&pool).await.unwrap()).unwrap(),
        serde_json::to_value(before_runs).unwrap()
    );
    assert_eq!(
        serde_json::to_value(load_agent_work_items(&pool).await.unwrap()).unwrap(),
        serde_json::to_value(before_work).unwrap()
    );
    let summaries = load_agent_run_summaries(&pool).await.unwrap();
    assert_eq!(summaries.len(), 30);
    assert!(summaries.iter().all(|run| run.log.is_empty()));
    drop_test_schema(pool, path).await;
}

/// Run with LANTOR_BENCH_SOURCE_DATABASE=/path/to/lantor.sqlite and --ignored --nocapture.
/// The source is opened read-only by SQLite's online backup; only the disposable copy is changed.
#[tokio::test]
#[ignore = "manual bootstrap benchmark using a read-only backup of a representative database"]
async fn benchmark_bootstrap_recency_indexes() {
    let source = std::env::var("LANTOR_BENCH_SOURCE_DATABASE")
        .expect("set LANTOR_BENCH_SOURCE_DATABASE to a representative SQLite database");
    let path = std::env::temp_dir().join(format!(
        "lantor-bootstrap-bench-{}.sqlite",
        uuid::Uuid::new_v4().simple()
    ));
    let _backup_guard = BenchmarkDatabase(path.clone());
    let backup = std::process::Command::new("sqlite3")
        .args(["-readonly", &source])
        .arg(format!(
            ".backup '{}'",
            path.to_string_lossy().replace('\'', "''")
        ))
        .output()
        .expect("SQLite CLI is required for the read-only online backup");
    assert!(
        backup.status.success(),
        "{}",
        String::from_utf8_lossy(&backup.stderr)
    );

    let database_url = format!("sqlite://{}", path.display());
    let pool = db_connect_with_url(&database_url, 5).await.unwrap();
    migrate(&pool).await.unwrap();
    let (run_count, log_bytes): (i64, i64) = sqlx::query_as(
        "select count(*), coalesce(sum(length(cast(log as blob))), 0) from agent_runs",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let work_count: i64 = sqlx::query_scalar("select count(*) from agent_work_items")
        .fetch_one(&pool)
        .await
        .unwrap();
    println!(
        "BOOTSTRAP_RECENCY_DATASET {}",
        json!({
            "agent_runs": run_count, "agent_run_log_bytes": log_bytes, "agent_work_items": work_count,
        })
    );
    let app = web_router(
        Arc::new(WebState {
            pool: pool.clone(),
            db_url: database_url,
        }),
        PathBuf::from("dist"),
    );
    let mut expected_recent = None;
    // Repeat A/B to make warm-cache and ordering effects visible. No timing thresholds in CI.
    for phase in ["before", "after", "before", "after"] {
        if phase == "before" {
            drop_recency_indexes(&pool).await;
        } else {
            migrate(&pool).await.unwrap();
        }
        let mut samples = Vec::new();
        for iteration in 0..6 {
            let started = Instant::now();
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/api/bootstrap?currentChannelOnly=true")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            let payload: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                payload["agent_runs"].as_array().unwrap().len(),
                run_count.min(30) as usize
            );
            assert_eq!(
                payload["agent_work_items"].as_array().unwrap().len(),
                work_count.min(80) as usize
            );
            assert!(
                payload["__perf"].is_object(),
                "bootstrap must report server timings"
            );
            let recent = json!([payload["agent_runs"], payload["agent_work_items"]]);
            if let Some(expected) = &expected_recent {
                assert!(
                    &recent == expected,
                    "indexes must not change recent results"
                );
            } else {
                expected_recent = Some(recent);
            }
            if iteration > 0 {
                samples.push(json!({"response_ms": elapsed_ms, "perf": payload["__perf"]}));
            }
        }
        // Print timings/counts only; do not expose messages, agent environment, or logs.
        println!(
            "BOOTSTRAP_RECENCY_BENCH {}",
            json!({
                "phase": phase, "plans": query_plans(&pool).await, "samples": samples,
            })
        );
    }
    drop_test_schema(pool, path.to_string_lossy().into_owned()).await;
}
