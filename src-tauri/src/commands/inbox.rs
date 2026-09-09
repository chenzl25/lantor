use tauri::State;
use uuid::Uuid;

use crate::owner_inbox::feed::{self, FeedCounts, FeedPage, FeedRequest};

#[tauri::command]
pub(crate) async fn load_activity_feed(
    request: FeedRequest,
    state: State<'_, AppState>,
) -> CommandResult<FeedPage> {
    feed::page(&state.pool, request).await
}

#[tauri::command]
pub(crate) async fn load_activity_counts(
    mention_handles: Vec<String>,
    state: State<'_, AppState>,
) -> CommandResult<FeedCounts> {
    feed::counts(&state.pool, &mention_handles).await
}

use crate::{
    app::{AppState, CommandResult},
    application::inbox::{
        self as application, InboxItemTimestamp, InboxItemsRequest, MarkChannelReadRequest,
        UpdateThreadFollowedRequest,
    },
};

#[tauri::command]
pub(crate) async fn mark_channel_read(
    channel_id: Uuid,
    through_seq: Option<i64>,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    application::mark_channel_read(
        &state.pool,
        MarkChannelReadRequest {
            channel_id,
            through_seq,
        },
    )
    .await
}

#[tauri::command]
pub(crate) async fn dismiss_inbox_items(
    items: Vec<InboxItemTimestamp>,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    application::dismiss_inbox_items(&state.pool, InboxItemsRequest { items }).await
}

#[tauri::command]
pub(crate) async fn mark_inbox_items_read(
    items: Vec<InboxItemTimestamp>,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    application::mark_inbox_items_read(&state.pool, InboxItemsRequest { items }).await
}

#[tauri::command]
pub(crate) async fn mark_all_inbox_read(state: State<'_, AppState>) -> CommandResult<()> {
    application::mark_all_inbox_read(&state.pool).await
}

#[tauri::command]
pub(crate) async fn update_thread_followed(
    thread_root_id: Uuid,
    followed: bool,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    application::update_thread_followed(
        &state.pool,
        UpdateThreadFollowedRequest {
            thread_root_id,
            followed,
        },
    )
    .await
}
