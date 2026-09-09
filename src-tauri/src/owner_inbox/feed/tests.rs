use super::*;
use crate::owner_inbox::{dismiss_inbox_items_in_pool, mark_inbox_items_read_in_pool};
use crate::test_support::{drop_test_schema, insert_test_channel, test_pool};
use std::collections::HashSet;

#[tokio::test]
async fn large_history_keeps_page_payload_bounded() {
    let (pool, path) = test_pool().await.expect("SQLite test database");
    let channel = insert_test_channel(&pool, "large-activity").await.unwrap();
    sqlx::query("with recursive n(x) as (select 1 union all select x+1 from n where x<5000) insert into messages(channel_id,sender_name,sender_role,body,created_at) select ?1,'Owner','owner','Large thread '||x,'2026-01-01T00:00:00Z' from n")
        .bind(channel).execute(&pool).await.unwrap();
    sqlx::query("insert into messages(channel_id,thread_root_id,sender_name,sender_role,body,created_at) select channel_id,id,'Agent','agent',?2,'2026-01-02T00:00:00Z' from messages where channel_id=?1")
        .bind(channel).bind("Reply content ".repeat(400)).execute(&pool).await.unwrap();
    let started = std::time::Instant::now();
    let totals = counts(&pool, &["@Owner".into()]).await.unwrap();
    let count_time = started.elapsed();
    let started = std::time::Instant::now();
    let result = page(
        &pool,
        FeedRequest {
            mention_handles: vec!["@Owner".into()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!((totals.total, totals.unread), (5000, 5000));
    assert_eq!(result.items.len(), PAGE_SIZE);
    let bytes = serde_json::to_vec(&result).unwrap().len();
    assert!(bytes < 100_000);
    eprintln!("Activity 5000 threads / 10000 messages: counts={count_time:?}, page={:?}, payload={bytes} bytes, rows={}", started.elapsed(), result.items.len());
    drop_test_schema(pool, path).await;
}

#[tokio::test]
async fn channel_dm_reminder_and_stream_visibility() {
    let (pool, path) = test_pool().await.expect("SQLite test database");
    let channel = insert_test_channel(&pool, "channel-feed").await.unwrap();
    let dm = insert_test_channel(&pool, "dm-feed").await.unwrap();
    sqlx::query("update channels set kind='dm' where id=?1")
        .bind(dm)
        .execute(&pool)
        .await
        .unwrap();
    for id in [channel, dm] {
        sqlx::query("insert into messages(channel_id,sender_name,sender_role,body) values (?1,'Agent','agent','Unread root')").bind(id).execute(&pool).await.unwrap();
        sqlx::query("insert into messages(channel_id,sender_name,sender_role,body,delivery_state) values (?1,'Agent','agent','Unpublished content','streaming')").bind(id).execute(&pool).await.unwrap();
    }
    sqlx::query("insert into reminders(channel_id,title,note,status,due_at,fired_at,recurrence) values (?1,'Due reminder','Reminder detail','fired','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z','none')")
        .bind(channel).execute(&pool).await.unwrap();
    let all = page(&pool, FeedRequest::default()).await.unwrap();
    assert_eq!(all.items.len(), 3);
    assert!(all.items.iter().all(|i| !i.excerpt.contains("Unpublished")));
    for filter in [FeedFilter::Dm, FeedFilter::Channel, FeedFilter::Reminder] {
        let result = page(
            &pool,
            FeedRequest {
                filter,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(result.items.len(), 1);
        assert!(result.items[0].unread);
    }
    drop_test_schema(pool, path).await;
}

async fn seed(pool: &SqlitePool) -> Uuid {
    let channel = insert_test_channel(pool, "paged-activity").await.unwrap();
    sqlx::query("with recursive n(x) as (select 1 union all select x+1 from n where x<205) insert into messages(channel_id,sender_name,sender_role,body,created_at) select ?1,'Owner','owner','Review '||x,'2026-01-01T00:00:00Z' from n")
        .bind(channel).execute(pool).await.unwrap();
    sqlx::query("insert into tasks(message_id,channel_id,title,status,updated_at) select id,channel_id,body,'in_review','2026-01-01T00:00:00Z' from messages where channel_id=?1")
        .bind(channel).execute(pool).await.unwrap();
    // More recent read threads than the former global 120-item cap.
    sqlx::query("with recursive n(x) as (select 1 union all select x+1 from n where x<130) insert into messages(channel_id,sender_name,sender_role,body,created_at) select ?1,'Owner','owner','Thread '||x,'2026-01-02T00:00:00Z' from n")
        .bind(channel).execute(pool).await.unwrap();
    sqlx::query("insert into messages(channel_id,thread_root_id,sender_name,sender_role,body,created_at) select channel_id,id,'Owner','owner','Waiting for reply','2026-01-03T00:00:00Z' from messages where channel_id=?1 and body like 'Thread %'")
        .bind(channel).execute(pool).await.unwrap();
    channel
}

#[tokio::test]
async fn filter_before_pagination_and_count_independently() {
    let (pool, path) = test_pool().await.expect("SQLite test database");
    seed(&pool).await;
    let totals = counts(&pool, &[]).await.unwrap();
    assert_eq!((totals.total, totals.unread), (335, 205));
    let all = page(&pool, FeedRequest::default()).await.unwrap();
    assert_eq!(all.items.len(), 30);
    assert!(all
        .items
        .iter()
        .all(|item| item.kind == "thread" && !item.unread));
    assert!(all.previous_cursor.is_none());
    let mut cursor = None;
    let mut ids = HashSet::new();
    loop {
        let result = page(
            &pool,
            FeedRequest {
                filter: FeedFilter::Unread,
                after: cursor,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(result.items.len() <= PAGE_SIZE);
        for item in result.items {
            assert_eq!(item.kind, "task");
            assert!(
                ids.insert(item.id),
                "equal timestamps must not duplicate a cursor page"
            );
        }
        cursor = result.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(ids.len(), 205, "all old unread tasks remain reachable");
    let second = page(
        &pool,
        FeedRequest {
            after: all.next_cursor.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let previous = page(
        &pool,
        FeedRequest {
            before: second.previous_cursor,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        all.items.iter().map(|i| &i.id).collect::<Vec<_>>(),
        previous.items.iter().map(|i| &i.id).collect::<Vec<_>>()
    );
    assert!(previous.previous_cursor.is_none());
    assert_eq!(counts(&pool, &[]).await.unwrap().unread, 205);
    drop_test_schema(pool, path).await;
}

#[tokio::test]
async fn read_dismiss_and_new_activity_preserve_existing_semantics() {
    let (pool, path) = test_pool().await.expect("SQLite test database");
    let channel = seed(&pool).await;
    let task_page = page(
        &pool,
        FeedRequest {
            filter: FeedFilter::Task,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let first = &task_page.items[0];
    let cutoff = chrono::DateTime::parse_from_rfc3339("2026-01-04T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    mark_inbox_items_read_in_pool(&pool, [(first.id.clone(), cutoff)])
        .await
        .unwrap();
    assert_eq!(counts(&pool, &[]).await.unwrap().unread, 204);
    dismiss_inbox_items_in_pool(&pool, [(first.id.clone(), cutoff)])
        .await
        .unwrap();
    assert_eq!(counts(&pool, &[]).await.unwrap().total, 334);
    sqlx::query("update tasks set updated_at='2026-01-05T00:00:00Z' where id=?1")
        .bind(first.task_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(counts(&pool, &[]).await.unwrap().unread, 205);
    assert_eq!(
        page(&pool, FeedRequest::default()).await.unwrap().items[0].id,
        first.id
    );
    let root: Uuid =
        sqlx::query_scalar("select id from messages where channel_id=?1 and body='Thread 1'")
            .bind(channel)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query("insert into messages(channel_id,thread_root_id,sender_name,sender_role,body,created_at) values (?1,?2,'Agent','agent','@Owner response','2026-01-06T00:00:00Z')")
        .bind(channel).bind(root).execute(&pool).await.unwrap();
    let mentions = page(
        &pool,
        FeedRequest {
            filter: FeedFilter::Mention,
            mention_handles: vec!["@Owner".into()],
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(mentions.items.len(), 1);
    assert!(mentions.items[0].unread);
    assert_eq!(mentions.items[0].thread_id, Some(root));
    drop_test_schema(pool, path).await;
}

#[tokio::test]
async fn bounded_summaries_and_cursor_validation() {
    let (pool, path) = test_pool().await.expect("SQLite test database");
    seed(&pool).await;
    sqlx::query("update messages set body=?1 where thread_root_id is not null")
        .bind("x".repeat(100_000))
        .execute(&pool)
        .await
        .unwrap();
    let result = page(&pool, FeedRequest::default()).await.unwrap();
    assert_eq!(result.items.len(), PAGE_SIZE);
    assert!(result
        .items
        .iter()
        .all(|i| i.excerpt.len() <= 2048 && i.title.len() <= 240));
    assert!(serde_json::to_vec(&result).unwrap().len() < 100_000);
    let invalid = FeedCursor {
        timestamp: "invalid".into(),
        id: "thread:x".into(),
    };
    assert!(page(
        &pool,
        FeedRequest {
            after: Some(invalid),
            ..Default::default()
        }
    )
    .await
    .is_err());
    assert!(counts(&pool, &["".into()]).await.is_err());
    drop_test_schema(pool, path).await;
}

#[tokio::test]
async fn thread_targets_first_unread_and_page_cutoff_preserves_later_reply() {
    let (pool, path) = test_pool().await.expect("SQLite test database");
    let channel = insert_test_channel(&pool, "activity-cutoff").await.unwrap();
    let root: Uuid = sqlx::query_scalar("insert into messages(channel_id,sender_name,sender_role,body,created_at) values (?1,'Owner','owner','Root','2026-01-01T00:00:00Z') returning id")
        .bind(channel).fetch_one(&pool).await.unwrap();
    let mut replies = Vec::new();
    for (day, role) in [(2, "agent"), (3, "owner"), (4, "agent")] {
        let id: Uuid = sqlx::query_scalar("insert into messages(channel_id,thread_root_id,sender_name,sender_role,body,created_at) values (?1,?2,?3,?3,?4,?5) returning id")
            .bind(channel).bind(root).bind(role).bind(format!("Reply {day}"))
            .bind(format!("2026-01-0{day}T00:00:00Z")).fetch_one(&pool).await.unwrap();
        replies.push(id);
    }
    let first = page(&pool, FeedRequest::default()).await.unwrap();
    assert_eq!(
        first.items.len(),
        1,
        "thread suppresses duplicate channel row"
    );
    assert_eq!(first.items[0].message_id, Some(replies[0]));
    assert_eq!(first.items[0].new_count, 2, "owner reply never adds unread");
    assert_eq!(first.items[0].excerpt, "Reply 4");
    // New activity arrives while the user reads the page, before its action is sent.
    sqlx::query("insert into messages(channel_id,thread_root_id,sender_name,sender_role,body,created_at) values (?1,?2,'Agent','agent','Later reply','2026-01-05T00:00:00Z')")
        .bind(channel).bind(root).execute(&pool).await.unwrap();
    let cutoff = chrono::DateTime::parse_from_rfc3339(&first.items[0].timestamp)
        .unwrap()
        .with_timezone(&chrono::Utc);
    mark_inbox_items_read_in_pool(&pool, [(first.items[0].id.clone(), cutoff)])
        .await
        .unwrap();
    dismiss_inbox_items_in_pool(&pool, [(first.items[0].id.clone(), cutoff)])
        .await
        .unwrap();
    let newer = page(&pool, FeedRequest::default()).await.unwrap();
    assert_eq!(
        newer.items.len(),
        1,
        "page dismissal cannot hide later activity"
    );
    assert_eq!(newer.items[0].new_count, 1);
    assert_eq!(newer.items[0].excerpt, "Later reply");
    assert!(newer.items[0].unread);
    crate::owner_inbox::mark_channel_read_in_pool(&pool, channel)
        .await
        .unwrap();
    assert_eq!(counts(&pool, &[]).await.unwrap().unread, 0);
    sqlx::query("update messages set thread_followed=0 where id=?1")
        .bind(root)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        counts(&pool, &[]).await.unwrap().total,
        0,
        "unfollowed read thread is excluded"
    );
    drop_test_schema(pool, path).await;
}

#[tokio::test]
async fn deleted_cursor_and_newer_items_do_not_shift_forward_page() {
    let (pool, path) = test_pool().await.expect("SQLite test database");
    seed(&pool).await;
    let first = page(
        &pool,
        FeedRequest {
            filter: FeedFilter::Task,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let after = first.next_cursor.unwrap();
    let expected = page(
        &pool,
        FeedRequest {
            filter: FeedFilter::Task,
            after: Some(after.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    sqlx::query("delete from tasks where id=?1")
        .bind(first.items.last().unwrap().task_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update tasks set updated_at='2027-01-01T00:00:00Z' where id=?1")
        .bind(first.items[0].task_id)
        .execute(&pool)
        .await
        .unwrap();
    let actual = page(
        &pool,
        FeedRequest {
            filter: FeedFilter::Task,
            after: Some(after),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        expected.items.iter().map(|i| &i.id).collect::<Vec<_>>(),
        actual.items.iter().map(|i| &i.id).collect::<Vec<_>>()
    );
    drop_test_schema(pool, path).await;
}
