//! Agents often hand files to the owner as Markdown links to local paths, which
//! only resolve on the host. When an agent message is complete, files it links
//! from its own workspace are snapshotted as ordinary attachments that record
//! the link target in `source_path`; the renderer resolves such links through
//! the attachment. The message body itself is never rewritten.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use pulldown_cmark::{Event, Options, Parser, Tag};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use super::{insert_message_attachments_tx, load_message_patch_in_tx};
use crate::{
    app::{to_string, CommandResult},
    attachments::{
        attachment_exceeds_size_limit, attachment_root_dir, infer_attachment_mime_type,
        StagedAttachment,
    },
    db::expand_home_path,
    models::AttachmentUpload,
    text::percent_decode_utf8,
    ui_notifications::{enqueue_ui_event_in_tx, UiEvent},
};

/// Bounds the filesystem work a single message can trigger.
const MAX_LINKED_FILES_PER_MESSAGE: usize = 16;

/// Snapshot failures never affect the message itself; links just stay plain.
pub(crate) async fn attach_linked_files_best_effort(pool: &SqlitePool, message_id: Uuid) {
    let result = match attachment_root_dir() {
        Ok(root) => attach_linked_files(pool, message_id, &root).await,
        Err(err) => Err(err),
    };
    if let Err(err) = result {
        eprintln!("Lantor could not attach files linked from message {message_id}: {err}");
    }
}

/// Idempotent: link targets that already have an attachment are skipped.
pub(super) async fn attach_linked_files(
    pool: &SqlitePool,
    message_id: Uuid,
    attachment_root: &Path,
) -> CommandResult<usize> {
    let Some(row) = sqlx::query(
        r#"
        select m.body, a.working_directory
        from messages m
        join agents a on a.id = m.sender_agent_id
        where m.id = $1 and m.delivery_state = 'complete'
        "#,
    )
    .bind(message_id)
    .fetch_optional(pool)
    .await
    .map_err(to_string)?
    else {
        return Ok(0);
    };
    let body: String = row.get("body");
    let mut links = linked_local_paths(&body);
    if links.is_empty() {
        return Ok(0);
    }
    let attached: HashSet<String> = sqlx::query_scalar(
        "select source_path from message_attachments where message_id = $1 and source_path is not null",
    )
    .bind(message_id)
    .fetch_all(pool)
    .await
    .map_err(to_string)?
    .into_iter()
    .collect();
    links.retain(|link| !attached.contains(link));
    links.truncate(MAX_LINKED_FILES_PER_MESSAGE);
    if links.is_empty() {
        return Ok(0);
    }

    let working_directory: String = row.get("working_directory");
    let root = attachment_root.to_path_buf();
    let uploads =
        tokio::task::spawn_blocking(move || stage_linked_files(&links, &working_directory, &root))
            .await
            .map_err(to_string)?;
    if uploads.is_empty() {
        return Ok(0);
    }

    let mut tx = pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(to_string)?;
    // Staging happens outside the write lock; drop the snapshot if the message
    // was edited or deleted meanwhile.
    let current_body: Option<String> =
        sqlx::query_scalar("select body from messages where id = $1")
            .bind(message_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(to_string)?;
    if current_body.as_deref() != Some(body.as_str()) {
        return Ok(0);
    }
    let count = uploads.len();
    let pending_writes = insert_message_attachments_tx(&mut tx, message_id, uploads).await?;
    let message = load_message_patch_in_tx(&mut tx, message_id).await?;
    enqueue_ui_event_in_tx(
        &mut tx,
        &UiEvent::MessageUpsert {
            reason: "linked_files_attached",
            message: &message,
        },
    )
    .await?;
    tx.commit().await.map_err(to_string)?;
    pending_writes.commit();
    Ok(count)
}

/// Local paths linked from Markdown in first-seen order, decoded the same way
/// the renderer decodes link hrefs. Code spans and blocks are not links.
fn linked_local_paths(body: &str) -> Vec<String> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut seen = HashSet::new();
    Parser::new_ext(body, options)
        .filter_map(|event| match event {
            Event::Start(Tag::Link { dest_url, .. }) => local_path_from_link(&dest_url),
            _ => None,
        })
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

fn local_path_from_link(destination: &str) -> Option<String> {
    let destination = destination.trim();
    let path = strip_file_scheme(destination).unwrap_or(destination);
    let is_local = (path.starts_with('/') && !path.starts_with("//")) || path.starts_with("~/");
    if !is_local {
        return None;
    }
    // Strip URL position syntax before decoding so %3A42 and %23L42 remain
    // literal filename characters. Different references share one snapshot.
    let path = strip_link_position(path);
    Some(percent_decode_utf8(path).unwrap_or_else(|| path.to_owned()))
}

fn strip_link_position(path: &str) -> &str {
    fn position(value: &str) -> bool {
        !value.is_empty()
            && value.bytes().all(|byte| byte.is_ascii_digit())
            && value.parse::<u32>().is_ok_and(|number| number > 0)
    }
    if let Some((file, location)) = path.rsplit_once("#L") {
        let valid = match location.split_once('-') {
            Some((start, end)) => position(start) && position(end.strip_prefix('L').unwrap_or(end)),
            None => position(location),
        };
        if valid {
            return file;
        }
    }
    if let Some((file, last)) = path.rsplit_once(':') {
        if position(last) {
            if let Some((file, line)) = file.rsplit_once(':') {
                if !line.is_empty() && line.bytes().all(|byte| byte.is_ascii_digit()) {
                    return if position(line) { file } else { path };
                }
            }
            return file;
        }
    }
    path
}

fn strip_file_scheme(destination: &str) -> Option<&str> {
    let (scheme, rest) = destination.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("file") {
        return None;
    }
    Some(rest.strip_prefix("localhost").unwrap_or(rest))
}

fn stage_linked_files(
    links: &[String],
    working_directory: &str,
    attachment_root: &Path,
) -> Vec<AttachmentUpload> {
    // Existing attachments are already shared with the owner, so an agent may
    // re-link them as well as files from its own workspace.
    let roots: Vec<PathBuf> = [Path::new(working_directory.trim()), attachment_root]
        .into_iter()
        .filter(|root| !root.as_os_str().is_empty())
        .filter_map(|root| root.canonicalize().ok())
        .collect();
    let mut uploads = Vec::new();
    for link in links {
        let Some(source) = snapshot_source(link, &roots) else {
            continue;
        };
        let staged = match StagedAttachment::copy_from(attachment_root, &source) {
            Ok(staged) => staged,
            Err(err) => {
                eprintln!("Lantor could not snapshot {}: {err}", source.display());
                continue;
            }
        };
        // Re-check the copy: the source may have changed after it was resolved.
        if staged.size_bytes == 0 || attachment_exceeds_size_limit(staged.size_bytes) {
            continue;
        }
        let original_name = Path::new(link)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("attachment")
            .to_owned();
        uploads.push(AttachmentUpload {
            mime_type: infer_attachment_mime_type(&source, &original_name),
            original_name,
            bytes: Vec::new(),
            staged: Some(staged),
            source_path: Some(link.clone()),
        });
    }
    uploads
}

/// An existing, non-empty regular file whose canonical path stays within one of
/// `roots`, so symlinks cannot reach outside them.
fn snapshot_source(link: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    let source = Path::new(&expand_home_path(link)).canonicalize().ok()?;
    if !roots.iter().any(|root| source.starts_with(root)) {
        return None;
    }
    let metadata = fs::metadata(&source).ok()?;
    let size = metadata.len();
    (metadata.is_file() && size > 0 && !attachment_exceeds_size_limit(size)).then_some(source)
}

#[cfg(test)]
mod tests;
