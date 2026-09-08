use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{
    app::CommandResult,
    owner_inbox::{
        dismiss_inbox_items_in_pool, mark_all_owner_inbox_read_in_pool,
        mark_channel_read_through_in_pool, mark_inbox_items_read_in_pool,
        update_thread_followed_in_pool,
    },
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InboxItemTimestamp {
    pub(crate) item_id: String,
    pub(crate) dismissed_until: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InboxItemsRequest {
    pub(crate) items: Vec<InboxItemTimestamp>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MarkChannelReadRequest {
    pub(crate) channel_id: Uuid,
    pub(crate) through_seq: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateThreadFollowedRequest {
    pub(crate) thread_root_id: Uuid,
    pub(crate) followed: bool,
}

pub(crate) async fn mark_channel_read(
    pool: &SqlitePool,
    request: MarkChannelReadRequest,
) -> CommandResult<()> {
    mark_channel_read_through_in_pool(pool, request.channel_id, request.through_seq).await
}

pub(crate) async fn dismiss_inbox_items(
    pool: &SqlitePool,
    request: InboxItemsRequest,
) -> CommandResult<()> {
    dismiss_inbox_items_in_pool(
        pool,
        request
            .items
            .into_iter()
            .map(|item| (item.item_id, item.dismissed_until)),
    )
    .await
}

pub(crate) async fn mark_inbox_items_read(
    pool: &SqlitePool,
    request: InboxItemsRequest,
) -> CommandResult<()> {
    mark_inbox_items_read_in_pool(
        pool,
        request
            .items
            .into_iter()
            .map(|item| (item.item_id, item.dismissed_until)),
    )
    .await
}

pub(crate) async fn mark_all_inbox_read(pool: &SqlitePool) -> CommandResult<()> {
    mark_all_owner_inbox_read_in_pool(pool).await
}

pub(crate) async fn update_thread_followed(
    pool: &SqlitePool,
    request: UpdateThreadFollowedRequest,
) -> CommandResult<()> {
    update_thread_followed_in_pool(pool, request.thread_root_id, request.followed).await
}
