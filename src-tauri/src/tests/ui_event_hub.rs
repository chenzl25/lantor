use super::*;
use crate::{
    test_support::{drop_test_schema, test_pool},
    ui_notifications::{enqueue_ui_event_in_tx, UiEvent},
};
use std::sync::atomic::Ordering;

async fn receive(subscription: &mut UiEventSubscription) -> UiEventReplay {
    tokio::time::timeout(Duration::from_secs(2), subscription.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn subscribers_share_one_reader_and_only_observe_commits() {
    let (pool, path) = test_pool().await.expect("fixture");
    let mut first = UiEventSubscription::connect(&pool, Some(0)).await.unwrap();
    let mut second = UiEventSubscription::connect(&pool, Some(0)).await.unwrap();
    let mut third = UiEventSubscription::connect(&pool, Some(0)).await.unwrap();
    assert!(Arc::ptr_eq(&first._hub, &second._hub));
    assert!(Arc::ptr_eq(&first._hub, &third._hub));
    for subscriber in [&mut first, &mut second, &mut third] {
        assert!(receive(subscriber).await.events.is_empty());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let stats = first._hub.stats.clone();
    let reads = stats.event_reads.load(Ordering::Relaxed);
    let checks = stats.pragma_reads.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(180)).await;
    assert_eq!(
        stats.event_reads.load(Ordering::Relaxed),
        reads,
        "no idle event-table reads"
    );
    assert!(stats.pragma_reads.load(Ordering::Relaxed) > checks);
    let mut tx = pool.begin().await.unwrap();
    enqueue_ui_event_in_tx(
        &mut tx,
        &UiEvent::Refresh {
            reason: "rolled_back",
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        stats.event_reads.load(Ordering::Relaxed),
        reads,
        "uncommitted rows do not wake readers"
    );
    tx.rollback().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        stats.event_reads.load(Ordering::Relaxed),
        reads,
        "rollback is not a delivery"
    );
    let mut tx = pool.begin().await.unwrap();
    for reason in ["first", "second"] {
        enqueue_ui_event_in_tx(&mut tx, &UiEvent::Refresh { reason })
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
    let expected = receive(&mut first).await;
    assert_eq!(expected.events.len(), 2);
    for subscriber in [&mut second, &mut third] {
        let received = receive(subscriber).await;
        assert_eq!(
            serde_json::to_value(received).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
    }
    assert_eq!(
        stats.event_reads.load(Ordering::Relaxed),
        reads + 1,
        "one shared read, not three"
    );
    let weak = Arc::downgrade(&first._hub);
    drop((first, second, third));
    assert!(
        weak.upgrade().is_none(),
        "last disconnect disposes the observer"
    );
    drop_test_schema(pool, path).await;
}

#[tokio::test]
async fn subscribe_before_replay_deduplicates_live_overlap_and_recovers_lag() {
    let (pool, path) = test_pool().await.expect("fixture");
    let mut subscriber = UiEventSubscription::connect(&pool, Some(0)).await.unwrap();
    assert!(receive(&mut subscriber).await.events.is_empty());
    let mut tx = pool.begin().await.unwrap();
    for _ in 0..100 {
        enqueue_ui_event_in_tx(&mut tx, &UiEvent::Refresh { reason: "burst" })
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
    let replay = load_ui_event_replay_from_cursor(&pool, 0).await.unwrap();
    // Deterministically saturate the live queue while the consumer is paused.
    for event in &replay.events {
        subscriber
            ._hub
            .sender
            .send(Arc::new(UiEventReplay {
                cursor: event.cursor,
                replay_gap: false,
                events: vec![event.clone()],
            }))
            .unwrap();
    }
    let recovered = receive(&mut subscriber).await;
    assert_eq!(recovered.events.len(), 100);
    assert!(!recovered.replay_gap);
    assert!(
        tokio::time::timeout(Duration::from_millis(120), subscriber.recv())
            .await
            .is_err(),
        "queued overlap is discarded"
    );
    let mut reconnect = UiEventSubscription::connect(&pool, Some(replay.events[89].cursor))
        .await
        .unwrap();
    assert_eq!(receive(&mut reconnect).await.events.len(), 10);
    sqlx::query("delete from ui_events where id < $1")
        .bind(replay.cursor)
        .execute(&pool)
        .await
        .unwrap();
    let mut expired = UiEventSubscription::connect(&pool, Some(0)).await.unwrap();
    let gap = receive(&mut expired).await;
    assert!(gap.replay_gap);
    assert_eq!(gap.cursor, replay.cursor);
    assert!(gap.events.is_empty());
    let mut fresh = UiEventSubscription::connect(&pool, None).await.unwrap();
    assert!(
        receive(&mut fresh).await.events.is_empty(),
        "new clients start at head"
    );
    drop((subscriber, reconnect, expired, fresh));
    drop_test_schema(pool, path).await;
}

#[tokio::test]
async fn event_bounds_seek_endpoints_instead_of_scanning_history() {
    use sqlx::Row;
    let (pool, path) = test_pool().await.expect("fixture");
    let query = format!(
        "explain query plan {}",
        crate::ui_notifications::UI_EVENT_BOUNDS_SQL
    );
    let plan = sqlx::query(&query)
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect::<Vec<_>>();
    assert_eq!(
        plan.iter()
            .filter(|line| line.contains("SEARCH ui_events"))
            .count(),
        2,
        "{plan:?}"
    );
    drop_test_schema(pool, path).await;
}
