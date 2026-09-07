use tauri::State;
use uuid::Uuid;

use crate::{
    app::{AppState, CommandResult},
    application::messages::{
        self as application, LoadActivityMessagesRequest, LoadChannelMessagesRequest,
        LoadOlderChannelMessagesRequest, MessageIdRequest, SearchMessagesRequest,
        SendMessageRequest, SetMessageSavedRequest, UpdateMessageRequest,
    },
    models::{AttachmentUpload, ChannelMessagePage, Message},
};

#[tauri::command]
pub(crate) async fn send_message(
    message_id: Option<Uuid>,
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    body: String,
    as_task: bool,
    attachments: Option<Vec<AttachmentUpload>>,
    state: State<'_, AppState>,
) -> CommandResult<Message> {
    application::send_message(
        &state.pool,
        SendMessageRequest {
            message_id,
            channel_id,
            thread_root_id,
            body,
            as_task,
            attachments,
        },
    )
    .await
}

#[tauri::command]
pub(crate) async fn load_channel_messages(
    channel_id: Uuid,
    state: State<'_, AppState>,
) -> CommandResult<ChannelMessagePage> {
    application::load_channel_messages(&state.pool, LoadChannelMessagesRequest { channel_id }).await
}

#[tauri::command]
pub(crate) async fn load_channel_previews(
    state: State<'_, AppState>,
) -> CommandResult<Vec<Message>> {
    application::load_channel_previews(&state.pool).await
}

#[tauri::command]
pub(crate) async fn load_activity_messages(
    mention_handles: Vec<String>,
    state: State<'_, AppState>,
) -> CommandResult<Vec<Message>> {
    application::load_activity_messages(
        &state.pool,
        LoadActivityMessagesRequest { mention_handles },
    )
    .await
}

#[tauri::command]
pub(crate) async fn search_messages(
    query: String,
    after: Option<String>,
    limit: i64,
    state: State<'_, AppState>,
) -> CommandResult<Vec<Message>> {
    application::search_messages(
        &state.pool,
        SearchMessagesRequest {
            query,
            after,
            limit,
        },
    )
    .await
}

#[tauri::command]
pub(crate) async fn load_message(
    message_id: Uuid,
    state: State<'_, AppState>,
) -> CommandResult<Message> {
    application::load_message(&state.pool, MessageIdRequest { message_id }).await
}

#[tauri::command]
pub(crate) async fn load_older_channel_messages(
    channel_id: Uuid,
    before_seq: i64,
    limit: i64,
    state: State<'_, AppState>,
) -> CommandResult<ChannelMessagePage> {
    application::load_older_channel_messages(
        &state.pool,
        LoadOlderChannelMessagesRequest {
            channel_id,
            before_seq,
            limit,
        },
    )
    .await
}

#[tauri::command]
pub(crate) async fn update_message(
    message_id: Uuid,
    body: String,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    application::update_message(&state.pool, UpdateMessageRequest { message_id, body }).await
}

#[tauri::command]
pub(crate) async fn delete_message(
    message_id: Uuid,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    application::delete_message(&state.pool, MessageIdRequest { message_id }).await
}

#[tauri::command]
pub(crate) async fn set_message_saved(
    message_id: Uuid,
    saved: bool,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    application::set_message_saved(&state.pool, SetMessageSavedRequest { message_id, saved }).await
}
