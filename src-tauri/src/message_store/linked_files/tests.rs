use std::{fs, os::unix::fs::symlink, path::PathBuf};

use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use super::{attach_linked_files, linked_local_paths, snapshot_source};
use crate::message_store::insert_agent_message;
use crate::test_support::{drop_test_schema, insert_test_agent, insert_test_channel, test_pool};

fn temp_dir(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("lantor-linked-{label}-{}", Uuid::new_v4()));
    fs::create_dir_all(&path).unwrap();
    path.canonicalize().unwrap()
}

async fn agent_with_workspace(
    pool: &SqlitePool,
    handle: &str,
    workspace: &std::path::Path,
) -> Uuid {
    let agent_id = insert_test_agent(pool, handle).await.unwrap();
    sqlx::query("update agents set working_directory = $1 where id = $2")
        .bind(workspace.to_string_lossy().as_ref())
        .bind(agent_id)
        .execute(pool)
        .await
        .unwrap();
    agent_id
}

async fn insert_complete_agent_message(
    pool: &SqlitePool,
    agent_id: Uuid,
    channel_id: Uuid,
    body: &str,
) -> Uuid {
    sqlx::query_scalar(
        "insert into messages (channel_id, sender_agent_id, sender_name, sender_role, body) values ($1, $2, 'agent', 'agent', $3) returning id",
    )
    .bind(channel_id)
    .bind(agent_id)
    .bind(body)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn attachment_rows(
    pool: &SqlitePool,
    message_id: Uuid,
) -> Vec<(String, String, String, String)> {
    sqlx::query(
        "select original_name, mime_type, storage_path, source_path from message_attachments where message_id = $1 order by created_at, original_name",
    )
    .bind(message_id)
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .map(|row| (row.get("original_name"), row.get("mime_type"), row.get("storage_path"), row.get("source_path")))
    .collect()
}

#[test]
fn linked_local_paths_reads_markdown_link_destinations_like_the_renderer() {
    let body = r#"
Deck: [deck](/ws/out/deck.pptx), again [same](/ws/out/deck.pptx)
Spaces: [notes](</ws/out/meeting notes.md>) and encoded [cn](/ws/out/%E5%BE%B7%E8%8A%99.docx)
Schemes: [pdf](file:///ws/out/a.pdf) [host](file://localhost/ws/out/b.pdf) [home](~/out/c.txt)
Reference: [ref][r]

| file | link |
| --- | --- |
| sheet | [xlsx](/ws/out/plan.xlsx) |

Ignored: [web](https://example.com/x.pdf) [rel](out/x.md) [net](//host/x.md) `[code](/ws/inline.md)`

```md
[fenced](/ws/fenced.md)
```

[r]: /ws/out/ref.md
"#;
    assert_eq!(
        linked_local_paths(body),
        [
            "/ws/out/deck.pptx",
            "/ws/out/meeting notes.md",
            "/ws/out/德芙.docx",
            "/ws/out/a.pdf",
            "/ws/out/b.pdf",
            "~/out/c.txt",
            "/ws/out/ref.md",
            "/ws/out/plan.xlsx",
        ]
    );
}

#[test]
fn snapshot_source_requires_a_non_empty_file_inside_an_allowed_root() {
    let workspace = temp_dir("workspace");
    let outside = temp_dir("outside");
    fs::create_dir_all(workspace.join("out")).unwrap();
    fs::write(workspace.join("out/deck.pptx"), b"deck").unwrap();
    fs::write(workspace.join("out/empty.txt"), b"").unwrap();
    fs::write(outside.join("secret.txt"), b"secret").unwrap();
    symlink(outside.join("secret.txt"), workspace.join("out/escape.txt")).unwrap();
    let roots = [workspace.clone()];
    let link = |path: &str| workspace.join(path).to_string_lossy().into_owned();

    assert_eq!(
        snapshot_source(&link("out/deck.pptx"), &roots),
        Some(workspace.join("out/deck.pptx"))
    );
    assert_eq!(
        snapshot_source(&link("out/deck.pptx:12"), &roots),
        None,
        "code links with a line are not files"
    );
    assert_eq!(
        snapshot_source(&link("out"), &roots),
        None,
        "directories are not snapshotted"
    );
    assert_eq!(snapshot_source(&link("out/empty.txt"), &roots), None);
    assert_eq!(
        snapshot_source(&link("out/escape.txt"), &roots),
        None,
        "symlinks cannot leave the root"
    );
    assert_eq!(snapshot_source(&link("out/../../x"), &roots), None);
    assert_eq!(
        snapshot_source(&outside.join("secret.txt").to_string_lossy(), &roots),
        None
    );

    fs::remove_dir_all(workspace).unwrap();
    fs::remove_dir_all(outside).unwrap();
}

#[tokio::test]
async fn completed_agent_messages_snapshot_linked_workspace_files_once() {
    let Some((pool, database)) = test_pool().await else {
        return;
    };
    let workspace = temp_dir("workspace");
    let outside = temp_dir("outside");
    let attachment_root = temp_dir("attachments");
    fs::create_dir_all(workspace.join("out")).unwrap();
    fs::write(workspace.join("out/deck.pptx"), b"deck v1").unwrap();
    fs::write(workspace.join("out/meeting notes.md"), b"# notes").unwrap();
    fs::write(outside.join("secret.txt"), b"secret").unwrap();
    let agent_id = agent_with_workspace(&pool, "linker", &workspace).await;
    let channel_id = insert_test_channel(&pool, "linked-files").await.unwrap();
    let deck = workspace
        .join("out/deck.pptx")
        .to_string_lossy()
        .into_owned();
    let notes = workspace
        .join("out/meeting notes.md")
        .to_string_lossy()
        .into_owned();
    let body = format!(
        "Deck: [deck]({}) notes: [notes](<{notes}>) secret: [s]({}) code: [l]({deck}:3) `[c]({deck})`",
        deck.replace(' ', "%20"),
        outside.join("secret.txt").display(),
    );
    let message_id = insert_complete_agent_message(&pool, agent_id, channel_id, &body).await;

    assert_eq!(
        attach_linked_files(&pool, message_id, &attachment_root)
            .await
            .unwrap(),
        2
    );
    let rows = attachment_rows(&pool, message_id).await;
    assert_eq!(rows.len(), 2);
    let pptx = rows
        .iter()
        .find(|row| row.3 == deck)
        .expect("deck snapshot");
    assert_eq!(pptx.0, "deck.pptx");
    assert_eq!(
        pptx.1,
        "application/vnd.openxmlformats-officedocument.presentationml.presentation"
    );
    assert!(pptx.2.starts_with(
        &*attachment_root
            .join(message_id.to_string())
            .to_string_lossy()
    ));
    assert_eq!(fs::read(&pptx.2).unwrap(), b"deck v1");
    let md = rows
        .iter()
        .find(|row| row.3 == notes)
        .expect("notes snapshot");
    assert_eq!(
        (md.0.as_str(), md.1.as_str()),
        ("meeting notes.md", "text/markdown")
    );

    // The snapshot is frozen: later edits to the workspace file do not leak in.
    fs::write(workspace.join("out/deck.pptx"), b"deck v2").unwrap();
    assert_eq!(fs::read(&pptx.2).unwrap(), b"deck v1");
    assert_eq!(
        attach_linked_files(&pool, message_id, &attachment_root)
            .await
            .unwrap(),
        0
    );
    assert_eq!(attachment_rows(&pool, message_id).await.len(), 2);
    let event: String =
        sqlx::query_scalar("select event_json from ui_events order by id desc limit 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(event.contains("linked_files_attached") && event.contains("source_path"));

    // Streaming placeholders and owner messages are left alone.
    let streaming =
        insert_complete_agent_message(&pool, agent_id, channel_id, &format!("[d]({deck})")).await;
    sqlx::query("update messages set delivery_state = 'streaming' where id = $1")
        .bind(streaming)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        attach_linked_files(&pool, streaming, &attachment_root)
            .await
            .unwrap(),
        0
    );
    let owner: Uuid = sqlx::query_scalar(
        "insert into messages (channel_id, sender_name, sender_role, body) values ($1, 'owner', 'owner', $2) returning id",
    )
    .bind(channel_id)
    .bind(format!("[d]({deck})"))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        attach_linked_files(&pool, owner, &attachment_root)
            .await
            .unwrap(),
        0
    );

    drop_test_schema(pool, database).await;
    for dir in [workspace, outside, attachment_root] {
        fs::remove_dir_all(dir).unwrap();
    }
}

#[tokio::test]
async fn agent_message_insert_attaches_linked_files() {
    let Some((pool, database)) = test_pool().await else {
        return;
    };
    let workspace = temp_dir("workspace");
    fs::write(workspace.join("report.pdf"), b"%PDF").unwrap();
    let agent_id = agent_with_workspace(&pool, "inserter", &workspace).await;
    let channel_id = insert_test_channel(&pool, "linked-insert").await.unwrap();
    let report = workspace.join("report.pdf").to_string_lossy().into_owned();
    let message_id = insert_agent_message(
        &pool,
        agent_id,
        channel_id,
        None,
        &format!("[report]({report})"),
        false,
    )
    .await
    .unwrap();

    let rows = attachment_rows(&pool, message_id).await;
    // This path uses the configured attachment root; remove what it wrote.
    crate::attachments::remove_attachment_files(
        &rows.iter().map(|row| row.2.clone()).collect::<Vec<_>>(),
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].0.as_str(), rows[0].1.as_str(), rows[0].3.as_str()),
        ("report.pdf", "application/pdf", report.as_str())
    );

    drop_test_schema(pool, database).await;
    fs::remove_dir_all(workspace).unwrap();
}
