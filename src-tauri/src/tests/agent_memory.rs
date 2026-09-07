use super::{format_memory_index_entry, insert_memory_index_entry};

#[test]
fn memory_append_can_add_work_log_link_without_timestamp_log() {
    let memory = "# @agent\n\n## Role\nLantor agent.\n\n## Key Knowledge\n- Add stable facts and links that help a restarted agent recover quickly.\n\n## Active Context\n- Currently working on: none.";

    let updated = insert_memory_index_entry(
        memory,
        &format_memory_index_entry("`notes/work-log.md` - staged durable updates."),
    );

    assert!(updated.contains("## Key Knowledge\n- `notes/work-log.md` - staged durable updates."));
    assert!(updated.contains("\n## Active Context"));
    assert!(!updated.contains("Memory update"));
    assert!(!updated.contains("Add stable facts and links"));
}

#[tokio::test]
async fn run_log_archive_survives_database_tail_truncation() {
    use crate::test_support::{drop_test_schema, insert_test_agent, test_pool};
    let (pool, database) = test_pool().await.expect("SQLite fixture must initialize");
    let agent = insert_test_agent(&pool, "run-log-test").await.unwrap();
    let run: uuid::Uuid = sqlx::query_scalar("insert into agent_runs (agent_id, command, status) values ($1, 'claude', 'running') returning id")
        .bind(agent).fetch_one(&pool).await.unwrap();
    let first = format!("[claude] first delta\n{}\n", "正文🦀".repeat(12000));
    let last = "[claude] complete result\n";
    super::append_run_log(&pool, run, first.clone())
        .await
        .unwrap();
    super::append_run_log(&pool, run, last.to_owned())
        .await
        .unwrap();
    let tail: String = sqlx::query_scalar("select log from agent_runs where id=$1")
        .bind(run)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(tail.chars().count(), 20000);
    assert!(!tail.contains("first delta"));
    assert!(tail.ends_with(last));
    let directory = std::path::Path::new(&database).with_extension("run-logs");
    let archive = std::fs::read_to_string(directory.join(format!("{run}.log"))).unwrap();
    assert_eq!(archive, format!("{first}{last}"));
    // Unavailable archival storage must not prevent event processing.
    std::fs::remove_dir_all(&directory).unwrap();
    std::fs::write(&directory, "not a directory").unwrap();
    super::append_run_log(&pool, run, "still delivered".to_owned())
        .await
        .unwrap();
    let tail: String = sqlx::query_scalar("select log from agent_runs where id=$1")
        .bind(run)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(tail.contains("still delivered"));
    assert!(tail.contains("run log archive unavailable"));
    std::fs::remove_file(directory).unwrap();
    drop_test_schema(pool, database).await;
}
