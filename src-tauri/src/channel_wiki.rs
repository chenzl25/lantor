use chrono::{DateTime, Utc};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::app::{to_string, CommandResult};

/// Hard limit for wiki content. The wiki is required reading before working in
/// a channel, so it must stay compact and index-like; details belong in
/// artifacts, notes, or linked documents.
pub(crate) const CHANNEL_WIKI_MAX_BYTES: usize = 16 * 1024;

/// Wikis at or below this size are inlined into inbox wake envelopes so agents
/// can read them without an extra context-tool round trip.
pub(crate) const CHANNEL_WIKI_INLINE_BYTES: usize = 2 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct ChannelWikiRevision {
    pub(crate) id: Uuid,
    pub(crate) parent_id: Option<Uuid>,
    pub(crate) content: String,
    pub(crate) author: String,
    pub(crate) note: String,
    pub(crate) created_at: DateTime<Utc>,
}

#[derive(Debug)]
pub(crate) enum ChannelWikiPublishOutcome {
    Published(ChannelWikiRevision),
    /// The supplied parent revision no longer matches the current head. The
    /// caller should re-read the current head, merge, and retry.
    Conflict(ChannelWikiRevision),
}

fn revision_from_row(row: &sqlx::sqlite::SqliteRow) -> ChannelWikiRevision {
    ChannelWikiRevision {
        id: row.get("id"),
        parent_id: row.get("parent_id"),
        content: row.get("content"),
        author: row.get("author"),
        note: row.get("note"),
        created_at: row.get("created_at"),
    }
}

pub(crate) async fn load_channel_wiki_head(
    pool: &SqlitePool,
    channel_id: Uuid,
) -> CommandResult<Option<ChannelWikiRevision>> {
    let row = sqlx::query(
        r#"
        select r.id, r.parent_id, r.content, r.author, r.note, r.created_at
        from channel_wiki_heads h
        join channel_wiki_revisions r on r.id = h.head_id
        where h.channel_id = $1
        "#,
    )
    .bind(channel_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?;
    Ok(row.as_ref().map(revision_from_row))
}

/// Publish a new wiki revision with compare-and-swap head advance: the new
/// revision only becomes the head when `parent_id` matches the current head.
/// Concurrent editors therefore never silently overwrite each other.
pub(crate) async fn publish_channel_wiki_revision(
    pool: &SqlitePool,
    channel_id: Uuid,
    parent_id: Option<Uuid>,
    content: &str,
    author: &str,
    note: &str,
) -> CommandResult<ChannelWikiPublishOutcome> {
    if content.trim().is_empty() {
        return Err("wiki content is empty; to intentionally clear a wiki, write a short placeholder explaining why".to_owned());
    }
    if content.len() > CHANNEL_WIKI_MAX_BYTES {
        return Err(format!(
            "wiki content is {} bytes; the hard limit is {} bytes. Keep the wiki index-like and move details into artifacts or linked notes.",
            content.len(),
            CHANNEL_WIKI_MAX_BYTES
        ));
    }

    let mut transaction = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    let current = sqlx::query(
        r#"
        select r.id, r.parent_id, r.content, r.author, r.note, r.created_at
        from channel_wiki_heads h
        join channel_wiki_revisions r on r.id = h.head_id
        where h.channel_id = $1
        "#,
    )
    .bind(channel_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(to_string)?;
    let current = current.as_ref().map(revision_from_row);

    if current.as_ref().map(|head| head.id) != parent_id {
        transaction.rollback().await.map_err(to_string)?;
        return match current {
            Some(head) => Ok(ChannelWikiPublishOutcome::Conflict(head)),
            None => Err(
                "wiki parent revision was supplied but this channel has no wiki yet; retry without --parent".to_owned(),
            ),
        };
    }

    let row = sqlx::query(
        r#"
        insert into channel_wiki_revisions (channel_id, parent_id, content, author, note)
        values ($1, $2, $3, $4, $5)
        returning id, parent_id, content, author, note, created_at
        "#,
    )
    .bind(channel_id)
    .bind(parent_id)
    .bind(content)
    .bind(author)
    .bind(note)
    .fetch_one(&mut *transaction)
    .await
    .map_err(to_string)?;
    let revision = revision_from_row(&row);

    sqlx::query(
        r#"
        insert into channel_wiki_heads (channel_id, head_id, updated_at)
        values ($1, $2, strftime('%Y-%m-%dT%H:%M:%f+00:00','now'))
        on conflict (channel_id) do update set
            head_id = excluded.head_id,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(channel_id)
    .bind(revision.id)
    .execute(&mut *transaction)
    .await
    .map_err(to_string)?;
    transaction.commit().await.map_err(to_string)?;
    Ok(ChannelWikiPublishOutcome::Published(revision))
}

pub(crate) async fn list_channel_wiki_revisions(
    pool: &SqlitePool,
    channel_id: Uuid,
    limit: i64,
) -> CommandResult<Vec<ChannelWikiRevision>> {
    let rows = sqlx::query(
        r#"
        select r.id, r.parent_id, r.content, r.author, r.note, r.created_at
        from channel_wiki_revisions r
        where r.channel_id = $1
        -- Revisions are append-only, so rowid reflects insertion order and
        -- breaks same-millisecond created_at ties deterministically (random
        -- blob ids do not).
        order by r.created_at desc, r.rowid desc
        limit $2
        "#,
    )
    .bind(channel_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(to_string)?;
    Ok(rows.iter().map(revision_from_row).collect())
}

pub(crate) fn short_revision_id(id: Uuid) -> String {
    id.to_string().chars().take(8).collect()
}

/// Post a system message in the channel announcing a published wiki revision,
/// so required-reading changes never land silently. Best-effort: the revision
/// itself is already committed when this runs.
pub(crate) async fn announce_channel_wiki_publish(
    pool: &SqlitePool,
    channel_id: Uuid,
    revision: &ChannelWikiRevision,
) {
    let action = if revision.parent_id.is_none() {
        "created"
    } else {
        "updated"
    };
    let mut body = format!(
        "📖 {} {action} the channel wiki (rev {})",
        revision.author,
        short_revision_id(revision.id),
    );
    let note = revision.note.trim();
    if !note.is_empty() {
        body.push_str(&format!(" — {note}"));
    }
    let _ = crate::ui_notifications::insert_system_message(pool, channel_id, None, body).await;
}

/// Build the wake-envelope wiki block for a channel, or None when the channel
/// has no wiki. Small wikis are inlined; larger ones are announced with their
/// revision id so agents can skip re-reading unchanged content.
pub(crate) async fn channel_wiki_wake_block(
    pool: &SqlitePool,
    channel_id: Uuid,
    channel_label: &str,
) -> CommandResult<Option<String>> {
    let Some(head) = load_channel_wiki_head(pool, channel_id).await? else {
        return Ok(None);
    };
    let rev = short_revision_id(head.id);
    let updated = head.created_at.format("%Y-%m-%dT%H:%M:%SZ");
    if head.content.len() <= CHANNEL_WIKI_INLINE_BYTES {
        Ok(Some(format!(
            "Channel wiki for {channel_label} (rev {rev}, updated {updated} by {author}) — required reading before substantive work; skip if you already read this revision:\n{content}",
            author = head.author,
            content = head.content.trim(),
        )))
    } else {
        Ok(Some(format!(
            "Channel wiki for {channel_label}: rev {rev}, updated {updated} by {author}, {size} bytes. Required reading before substantive work: run wiki-read --channel \"{channel_label}\" unless you already read this revision.",
            author = head.author,
            size = head.content.len(),
        )))
    }
}

#[cfg(test)]
#[path = "tests/channel_wiki.rs"]
mod relocated_tests;
