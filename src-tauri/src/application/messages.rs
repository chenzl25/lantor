use serde::Deserialize;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{
    app::CommandResult,
    message_store::{
        delete_message_in_pool, load_activity_messages_without_artifact_content,
        load_channel_preview_messages_without_artifact_content,
        load_message_without_artifact_content,
        load_older_channel_messages_without_artifact_content,
        load_recent_channel_message_page_without_artifact_content,
        search_messages_without_artifact_content, send_owner_message_in_pool,
        set_message_saved_in_pool, update_message_in_pool,
        CHANNEL_PREVIEW_ROOT_MESSAGES_PER_CHANNEL, WEB_BOOTSTRAP_ROOT_MESSAGES_PER_CHANNEL,
    },
    models::{AttachmentUpload, ChannelMessagePage, Message},
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SendMessageRequest {
    pub(crate) message_id: Option<Uuid>,
    pub(crate) channel_id: Uuid,
    pub(crate) thread_root_id: Option<Uuid>,
    pub(crate) body: String,
    pub(crate) as_task: bool,
    pub(crate) attachments: Option<Vec<AttachmentUpload>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LoadChannelMessagesRequest {
    pub(crate) channel_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LoadActivityMessagesRequest {
    pub(crate) mention_handles: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SearchMessagesRequest {
    pub(crate) query: String,
    pub(crate) after: Option<String>,
    pub(crate) limit: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LoadOlderChannelMessagesRequest {
    pub(crate) channel_id: Uuid,
    pub(crate) before_seq: i64,
    pub(crate) limit: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateMessageRequest {
    pub(crate) message_id: Uuid,
    pub(crate) body: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MessageIdRequest {
    pub(crate) message_id: Uuid,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetMessageSavedRequest {
    pub(crate) message_id: Uuid,
    pub(crate) saved: bool,
}

pub(crate) async fn send_message(
    pool: &SqlitePool,
    request: SendMessageRequest,
) -> CommandResult<Message> {
    send_owner_message_in_pool(
        pool,
        request.message_id,
        request.channel_id,
        request.thread_root_id,
        &request.body,
        request.as_task,
        request.attachments.unwrap_or_default(),
    )
    .await
}

pub(crate) async fn load_channel_messages(
    pool: &SqlitePool,
    request: LoadChannelMessagesRequest,
) -> CommandResult<ChannelMessagePage> {
    load_recent_channel_message_page_without_artifact_content(
        pool,
        request.channel_id,
        WEB_BOOTSTRAP_ROOT_MESSAGES_PER_CHANNEL,
    )
    .await
}

pub(crate) async fn load_channel_previews(pool: &SqlitePool) -> CommandResult<Vec<Message>> {
    load_channel_preview_messages_without_artifact_content(
        pool,
        CHANNEL_PREVIEW_ROOT_MESSAGES_PER_CHANNEL,
    )
    .await
}

pub(crate) async fn load_activity_messages(
    pool: &SqlitePool,
    request: LoadActivityMessagesRequest,
) -> CommandResult<Vec<Message>> {
    load_activity_messages_without_artifact_content(pool, &request.mention_handles).await
}

pub(crate) async fn search_messages(
    pool: &SqlitePool,
    request: SearchMessagesRequest,
) -> CommandResult<Vec<Message>> {
    search_messages_without_artifact_content(
        pool,
        &request.query,
        request.after.as_deref(),
        request.limit,
    )
    .await
}

pub(crate) async fn load_message(
    pool: &SqlitePool,
    request: MessageIdRequest,
) -> CommandResult<Message> {
    load_message_without_artifact_content(pool, request.message_id).await
}

pub(crate) async fn load_older_channel_messages(
    pool: &SqlitePool,
    request: LoadOlderChannelMessagesRequest,
) -> CommandResult<ChannelMessagePage> {
    load_older_channel_messages_without_artifact_content(
        pool,
        request.channel_id,
        request.before_seq,
        request.limit,
    )
    .await
}

pub(crate) async fn update_message(
    pool: &SqlitePool,
    request: UpdateMessageRequest,
) -> CommandResult<()> {
    update_message_in_pool(pool, request.message_id, &request.body).await
}

pub(crate) async fn delete_message(
    pool: &SqlitePool,
    request: MessageIdRequest,
) -> CommandResult<()> {
    delete_message_in_pool(pool, request.message_id).await
}

pub(crate) async fn set_message_saved(
    pool: &SqlitePool,
    request: SetMessageSavedRequest,
) -> CommandResult<()> {
    set_message_saved_in_pool(pool, request.message_id, request.saved).await
}
