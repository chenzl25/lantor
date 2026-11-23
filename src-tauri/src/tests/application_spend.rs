use uuid::Uuid;

use super::{load_agent_spend, SpendTotals};
use crate::test_support::{drop_test_schema, insert_test_agent, test_pool};

async fn run(pool: &sqlx::SqlitePool, agent_id: Uuid, age: &str, cost_micros: i64) {
    sqlx::query(
        "insert into agent_runs (agent_id, command, status, cost_micros, started_at, stopped_at) values ($1, 'codex', 'exited', $2, strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now', $3), strftime('%Y-%m-%dT%H:%M:%f+00:00', 'now', $3))",
    )
    .bind(agent_id)
    .bind(cost_micros)
    .bind(age)
    .execute(pool)
    .await
    .unwrap();
}

async fn account(pool: &sqlx::SqlitePool, agent_id: Uuid, account_id: Option<&str>) {
    sqlx::query(
        "insert into agent_subscription_status (agent_id, provider, status, billing, account_id) values ($1, 'codex', 'available', 'credits', $2)",
    )
    .bind(agent_id)
    .bind(account_id)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn spend_adds_up_the_agent_and_every_agent_on_its_account() {
    let Some((pool, schema)) = test_pool().await else {
        return;
    };
    let first = insert_test_agent(&pool, "spend-first").await.unwrap();
    let second = insert_test_agent(&pool, "spend-second").await.unwrap();
    let elsewhere = insert_test_agent(&pool, "spend-elsewhere").await.unwrap();
    account(&pool, first, Some("account-1")).await;
    account(&pool, second, Some("account-1")).await;
    account(&pool, elsewhere, Some("account-2")).await;
    run(&pool, first, "-1 hours", 1_000_000).await;
    run(&pool, first, "-3 days", 2_000_000).await;
    run(&pool, first, "-20 days", 4_000_000).await;
    run(&pool, first, "-40 days", 8_000_000).await;
    run(&pool, second, "-2 hours", 500_000).await;
    run(&pool, elsewhere, "-2 hours", 9_000_000).await;

    let spend = load_agent_spend(&pool, first).await.unwrap();
    assert_eq!(
        spend.agent,
        SpendTotals {
            last_day_micros: 1_000_000,
            last_week_micros: 3_000_000,
            last_month_micros: 7_000_000,
            runs_last_month: 3,
        }
    );
    let account = spend
        .account
        .expect("agents on the same account are added up");
    assert_eq!(account.agent_count, 2);
    assert_eq!(
        account.totals,
        SpendTotals {
            last_day_micros: 1_500_000,
            last_week_micros: 3_500_000,
            last_month_micros: 7_500_000,
            runs_last_month: 4,
        }
    );

    // Without a known account there is only the agent's own spend.
    let lone = insert_test_agent(&pool, "spend-lone").await.unwrap();
    run(&pool, lone, "-1 hours", 250_000).await;
    let spend = load_agent_spend(&pool, lone).await.unwrap();
    assert_eq!(spend.agent.last_day_micros, 250_000);
    assert!(spend.account.is_none());
    drop_test_schema(pool, schema).await;
}
