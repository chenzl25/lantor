use sqlx::SqlitePool;

use super::messages_from_rows;
use crate::{
    app::{to_string, CommandResult},
    models::Message,
};

const MESSAGE_SEARCH_LIMIT_MAX: i64 = 100;

pub(crate) async fn search_messages_without_artifact_content(
    pool: &SqlitePool,
    search_query: &str,
    after: Option<&str>,
    limit: i64,
) -> CommandResult<Vec<Message>> {
    let search_query = search_query.trim();
    if search_query.is_empty() {
        return Ok(Vec::new());
    }
    let escaped = search_query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let pattern = format!("%{escaped}%");
    let fts_query = fts_query(search_query);
    let sql = search_sql(fts_query.is_some());
    let mut statement = sqlx::query(&sql)
        .bind(pattern)
        .bind(after)
        .bind(limit.clamp(1, MESSAGE_SEARCH_LIMIT_MAX));
    if let Some(fts_query) = fts_query {
        statement = statement.bind(fts_query);
    }
    let rows = statement.fetch_all(pool).await.map_err(to_string)?;

    messages_from_rows(pool, rows, false).await
}

// MATCH treats punctuation and boolean operators as syntax unless the entire
// substring is quoted. LIKE stops at NUL, so retain its legacy path for NUL
// and queries shorter than three Unicode characters (not three UTF-8 bytes).
fn fts_query(query: &str) -> Option<String> {
    if query.contains('\0') || query.chars().take(3).count() < 3 {
        return None;
    }
    Some(format!("\"{}\"", query.replace('"', "\"\"")))
}

fn search_sql(use_fts: bool) -> String {
    // UNION channel matches separately so the OR with channel names cannot
    // turn the indexed lookup into a scan of all message bodies. The original
    // LIKE predicate below also preserves SQLite's ASCII-only case folding.
    let candidates = if use_fts {
        r#"m.rowid in (
            select rowid from messages_fts where messages_fts match $4
            union
            select rowid from messages where channel_id in (
                select id from channels where lower(name) like lower($1) escape '\'
            )
        ) and "#
    } else {
        ""
    };
    format!(
        r#"
        select
            m.id,
            m.seq,
            m.channel_id,
            m.thread_root_id,
            m.sender_agent_id,
            m.sender_name,
            m.sender_role,
            m.body,
            m.is_task,
            m.thread_followed,
            m.delivery_state,
            m.stream_key,
            t.number as task_number,
            t.status as task_status,
            m.created_at,
            m.updated_at
        from messages m
        join channels c on c.id = m.channel_id
        left join tasks t on t.message_id = m.id
        where {candidates}(
            lower(m.body) like lower($1) escape '\'
            or lower(m.sender_name) like lower($1) escape '\'
            or lower(c.name) like lower($1) escape '\'
        )
          and ($2 is null or julianday(m.created_at) >= julianday($2))
          and m.delivery_state <> 'streaming'
          and not (
            m.sender_role <> 'system'
            and m.sender_role <> 'owner'
            and m.stream_key glob '????????-????-????-????-????????????:*'
            and m.delivery_state = 'complete'
            and trim(m.body) = ''
            and not exists (
              select 1 from message_attachments ma where ma.message_id = m.id
            )
            and not exists (
              select 1 from artifacts ar where ar.message_id = m.id
            )
          )
        order by m.seq desc
        limit $3
    "#
    )
}

#[cfg(test)]
mod tests;
