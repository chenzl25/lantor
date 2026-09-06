use serde_json::Value;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::app::{to_string, CommandResult};
use crate::ui_notifications::enqueue_ui_agent_run_changed_in_tx;

fn value_i64_at(value: &Value, path: &str) -> Option<i64> {
    value.pointer(path).and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
            .or_else(|| value.as_f64().map(|value| value.round() as i64))
    })
}

pub(crate) fn usage_from_runtime_event(value: &Value) -> Option<(i64, i64)> {
    let input_tokens = [
        "/params/tokenUsage/last/inputTokens",
        "/params/tokenUsage/last/input_tokens",
        "/params/tokenUsage/last/promptTokens",
        "/params/tokenUsage/last/prompt_tokens",
        "/params/usage/input_tokens",
        "/params/usage/inputTokens",
        "/params/usage/input",
        "/params/usage/prompt_tokens",
        "/params/usage/promptTokens",
        "/usage/input_tokens",
        "/usage/inputTokens",
        "/usage/prompt_tokens",
        "/message/usage/input_tokens",
        "/message/usage/prompt_tokens",
        "/params/tokenUsage/total/inputTokens",
        "/params/tokenUsage/total/input_tokens",
    ]
    .iter()
    .find_map(|path| value_i64_at(value, path))
    .unwrap_or_default();
    let output_tokens = [
        "/params/tokenUsage/last/outputTokens",
        "/params/tokenUsage/last/output_tokens",
        "/params/tokenUsage/last/completionTokens",
        "/params/tokenUsage/last/completion_tokens",
        "/params/usage/output_tokens",
        "/params/usage/outputTokens",
        "/params/usage/output",
        "/params/usage/completion_tokens",
        "/params/usage/completionTokens",
        "/usage/output_tokens",
        "/usage/outputTokens",
        "/usage/completion_tokens",
        "/message/usage/output_tokens",
        "/message/usage/completion_tokens",
        "/params/tokenUsage/total/outputTokens",
        "/params/tokenUsage/total/output_tokens",
    ]
    .iter()
    .find_map(|path| value_i64_at(value, path))
    .unwrap_or_default();

    (input_tokens > 0 || output_tokens > 0).then_some((input_tokens.max(0), output_tokens.max(0)))
}

pub(crate) fn usage_from_run_log(log: &str) -> Option<(i64, i64)> {
    log.lines()
        .filter_map(|line| {
            let json_start = line.find('{')?;
            let value = serde_json::from_str::<Value>(&line[json_start..]).ok()?;
            usage_from_runtime_event(&value)
        })
        .next_back()
}

fn model_cost_micros(runtime: &str, model: &str, input_tokens: i64, output_tokens: i64) -> i64 {
    let model = model.to_lowercase();
    let runtime = runtime.to_lowercase();
    let (input_per_million, output_per_million) = if runtime == "claude" {
        if model.contains("fable") || model.contains("mythos") {
            (10_000_000_i64, 50_000_000_i64)
        } else if model.contains("opus") {
            (5_000_000_i64, 25_000_000_i64)
        } else if model.contains("haiku") {
            (1_000_000_i64, 5_000_000_i64)
        } else {
            (3_000_000_i64, 15_000_000_i64)
        }
    } else if runtime == "codex" && model == "gpt-6-astra" {
        (10_000_000_i64, 50_000_000_i64)
    } else if model.contains("mini") {
        (150_000_i64, 600_000_i64)
    } else if model.contains("codex") {
        (1_500_000_i64, 6_000_000_i64)
    } else {
        (1_000_000_i64, 5_000_000_i64)
    };
    ((input_tokens.max(0) * input_per_million) + (output_tokens.max(0) * output_per_million))
        / 1_000_000
}

pub(crate) async fn record_run_usage(
    pool: &SqlitePool,
    agent_id: Uuid,
    run_id: Uuid,
    input_tokens: i64,
    output_tokens: i64,
    cost_micros: Option<i64>,
) -> CommandResult<()> {
    let row = sqlx::query("select runtime, model from agents where id = $1")
        .bind(agent_id)
        .fetch_one(pool)
        .await
        .map_err(to_string)?;
    let runtime: String = row.get("runtime");
    let model: String = row.get("model");
    let estimated_cost = cost_micros
        .unwrap_or_else(|| model_cost_micros(&runtime, &model, input_tokens, output_tokens))
        .max(0);
    let mut transaction = pool.begin().await.map_err(to_string)?;
    sqlx::query(
        r#"
        update agent_runs
        set input_tokens = max(input_tokens, $2),
            current_input_tokens = $2,
            output_tokens = max(output_tokens, $3),
            cost_micros = max(cost_micros, $4)
        where id = $1
        "#,
    )
    .bind(run_id)
    .bind(input_tokens.max(0))
    .bind(output_tokens.max(0))
    .bind(estimated_cost)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    enqueue_ui_agent_run_changed_in_tx(&mut transaction, run_id, "run_usage").await?;
    transaction.commit().await.map_err(to_string)?;
    Ok(())
}

pub(crate) async fn backfill_agent_run_usage_from_logs(pool: &SqlitePool) -> sqlx::Result<()> {
    let rows = sqlx::query(
        r#"
        select id, agent_id, log
        from agent_runs
        where input_tokens = 0
          and output_tokens = 0
          and log like '%tokenUsage%'
        order by started_at desc
        limit 200
        "#,
    )
    .fetch_all(pool)
    .await?;

    for row in rows {
        let log: String = row.get("log");
        let Some((input_tokens, output_tokens)) = usage_from_run_log(&log) else {
            continue;
        };
        let run_id: Uuid = row.get("id");
        let agent_id: Uuid = row.get("agent_id");
        let agent = sqlx::query("select runtime, model from agents where id = $1")
            .bind(agent_id)
            .fetch_one(pool)
            .await?;
        let runtime: String = agent.get("runtime");
        let model: String = agent.get("model");
        let cost_micros = model_cost_micros(&runtime, &model, input_tokens, output_tokens);
        sqlx::query(
            r#"
            update agent_runs
            set input_tokens = $2,
                current_input_tokens = $2,
                output_tokens = $3,
                cost_micros = $4
            where id = $1
            "#,
        )
        .bind(run_id)
        .bind(input_tokens.max(0))
        .bind(output_tokens.max(0))
        .bind(cost_micros.max(0))
        .execute(pool)
        .await?;
    }

    let rows = sqlx::query(
        r#"
        select id, log
        from agent_runs
        where current_input_tokens = 0
          and log like '%tokenUsage%'
        order by started_at desc
        limit 200
        "#,
    )
    .fetch_all(pool)
    .await?;

    for row in rows {
        let log: String = row.get("log");
        let Some((input_tokens, _)) = usage_from_run_log(&log) else {
            continue;
        };
        sqlx::query(
            r#"
            update agent_runs
            set current_input_tokens = $2
            where id = $1
            "#,
        )
        .bind(row.get::<Uuid, _>("id"))
        .bind(input_tokens.max(0))
        .execute(pool)
        .await?;
    }

    Ok(())
}

pub(crate) async fn agent_budget_exhausted(
    pool: &SqlitePool,
    agent_id: Uuid,
) -> CommandResult<Option<String>> {
    let daily_budget_micros: i64 =
        sqlx::query_scalar("select daily_budget_micros from agents where id = $1")
            .bind(agent_id)
            .fetch_one(pool)
            .await
            .map_err(to_string)?;
    if daily_budget_micros <= 0 {
        return Ok(None);
    }
    let spent: i64 = sqlx::query_scalar(
        r#"
        select coalesce(sum(cost_micros), 0)
        from agent_runs
        where agent_id = $1
          and started_at >= strftime('%Y-%m-%dT00:00:00.000+00:00','now')
        "#,
    )
    .bind(agent_id)
    .fetch_one(pool)
    .await
    .map_err(to_string)?;
    if spent >= daily_budget_micros {
        Ok(Some(format!(
            "daily budget reached: spent ${:.4} / ${:.4}",
            spent as f64 / 1_000_000.0,
            daily_budget_micros as f64 / 1_000_000.0
        )))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{drop_test_schema, insert_test_agent, test_pool};

    #[test]
    fn gpt_6_astra_usage_uses_current_public_rate() {
        assert_eq!(
            model_cost_micros("codex", "gpt-6-astra", 1_000_000, 1_000_000),
            60_000_000
        );
    }

    #[test]
    fn claude_fable_usage_uses_current_public_rate() {
        assert_eq!(
            model_cost_micros("claude", "fable", 1_000_000, 1_000_000),
            60_000_000
        );
        assert_eq!(
            model_cost_micros("claude", "claude-fable-5-1", 1_000_000, 1_000_000),
            60_000_000
        );
    }

    #[test]
    fn claude_code_aliases_use_current_public_rates() {
        assert_eq!(
            model_cost_micros("claude", "opus", 1_000_000, 1_000_000),
            30_000_000
        );
        assert_eq!(
            model_cost_micros("claude", "sonnet", 1_000_000, 1_000_000),
            18_000_000
        );
        assert_eq!(
            model_cost_micros("claude", "haiku", 1_000_000, 1_000_000),
            6_000_000
        );
    }

    #[tokio::test]
    async fn record_run_usage_keeps_peak_and_tracks_current_input_tokens() {
        let Some((pool, schema)) = test_pool().await else {
            return;
        };
        let result: CommandResult<()> = async {
            let agent_id = insert_test_agent(&pool, "usage-current-agent").await?;
            let run_id: Uuid = sqlx::query_scalar(
                r#"
                insert into agent_runs (agent_id, command, status)
                values ($1, 'codex app-server', 'running')
                returning id
                "#,
            )
            .bind(agent_id)
            .fetch_one(&pool)
            .await
            .map_err(to_string)?;

            record_run_usage(&pool, agent_id, run_id, 226_829, 2_000, None).await?;
            record_run_usage(&pool, agent_id, run_id, 140_250, 3_000, None).await?;

            let row = sqlx::query(
                r#"
                select input_tokens, current_input_tokens, output_tokens
                from agent_runs
                where id = $1
                "#,
            )
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .map_err(to_string)?;
            assert_eq!(row.get::<i64, _>("input_tokens"), 226_829);
            assert_eq!(row.get::<i64, _>("current_input_tokens"), 140_250);
            assert_eq!(row.get::<i64, _>("output_tokens"), 3_000);
            Ok(())
        }
        .await;
        drop_test_schema(pool, schema).await;
        result.unwrap();
    }
}
