use tauri::State;
use uuid::Uuid;

use crate::{
    app::{AppState, CommandResult},
    message_store::{delete_message_in_pool, set_message_saved_in_pool, update_message_in_pool},
    models::{AttachmentUpload, Message},
};

#[tauri::command]
pub(crate) async fn send_message(
    channel_id: Uuid,
    thread_root_id: Option<Uuid>,
    body: String,
    as_task: bool,
    client_message_id: Option<String>,
    attachments: Option<Vec<AttachmentUpload>>,
    state: State<'_, AppState>,
) -> CommandResult<Message> {
    crate::message_store::send_owner_message_in_pool_with_client_id(
        &state.pool,
        channel_id,
        thread_root_id,
        &body,
        as_task,
        attachments.unwrap_or_default(),
        client_message_id.as_deref(),
    )
    .await
}

#[tauri::command]
pub(crate) async fn update_message(
    message_id: Uuid,
    body: String,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    update_message_in_pool(&state.pool, message_id, &body).await
}

#[tauri::command]
pub(crate) async fn delete_message(
    message_id: Uuid,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    delete_message_in_pool(&state.pool, message_id).await
}

#[tauri::command]
pub(crate) async fn set_message_saved(
    message_id: Uuid,
    saved: bool,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    set_message_saved_in_pool(&state.pool, message_id, saved).await
}
