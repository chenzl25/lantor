use tauri::State;
use uuid::Uuid;

use crate::{
    app::{AppState, CommandResult},
    application::github::{
        self as application, BindGithubRepositoryRequest, CreateGithubIssueTaskRequest,
        CreateGithubReviewTaskRequest, GithubChannelRequest, GithubIssueRequest,
        RereviewGithubPullRequestRequest,
    },
    github::{
        GithubChannelOverview, GithubIssueDetail, GithubIssueTaskResult, GithubRepositoryBinding,
        GithubRereviewTaskResult, GithubReviewComparisons, GithubReviewTaskResult,
    },
};

#[tauri::command]
pub(crate) async fn load_github_review_queue(
    channel_id: Uuid,
    state: State<'_, AppState>,
) -> CommandResult<GithubChannelOverview> {
    application::load_github_review_queue(&state.pool, GithubChannelRequest { channel_id }).await
}

#[tauri::command]
pub(crate) async fn load_github_review_comparisons(
    channel_id: Uuid,
    state: State<'_, AppState>,
) -> CommandResult<GithubReviewComparisons> {
    application::load_github_review_comparisons(&state.pool, GithubChannelRequest { channel_id })
        .await
}

#[tauri::command]
pub(crate) async fn refresh_github_review_queue(
    channel_id: Uuid,
    state: State<'_, AppState>,
) -> CommandResult<GithubChannelOverview> {
    application::refresh_github_review_queue(&state.pool, GithubChannelRequest { channel_id }).await
}

#[tauri::command]
pub(crate) async fn refresh_github_issue_queue(
    channel_id: Uuid,
    state: State<'_, AppState>,
) -> CommandResult<GithubChannelOverview> {
    application::refresh_github_issue_queue(&state.pool, GithubChannelRequest { channel_id }).await
}

#[tauri::command]
pub(crate) async fn mark_github_review_attention_read(
    channel_id: Uuid,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    application::mark_github_review_attention_read(&state.pool, GithubChannelRequest { channel_id })
        .await
}

#[tauri::command]
pub(crate) async fn load_github_issue_detail(
    channel_id: Uuid,
    issue_number: i64,
    state: State<'_, AppState>,
) -> CommandResult<GithubIssueDetail> {
    application::load_github_issue_detail(
        &state.pool,
        GithubIssueRequest {
            channel_id,
            issue_number,
        },
    )
    .await
}

#[tauri::command]
pub(crate) async fn bind_github_repository(
    channel_id: Uuid,
    repository: String,
    local_path: Option<String>,
    review_login: Option<String>,
    state: State<'_, AppState>,
) -> CommandResult<GithubRepositoryBinding> {
    application::bind_github_repository(
        &state.pool,
        BindGithubRepositoryRequest {
            channel_id,
            repository,
            local_path,
            review_login,
        },
    )
    .await
}

#[tauri::command]
pub(crate) async fn create_github_review_task(
    channel_id: Uuid,
    pull_number: i64,
    agent_id: Uuid,
    state: State<'_, AppState>,
) -> CommandResult<GithubReviewTaskResult> {
    application::create_github_review_task(
        &state.pool,
        CreateGithubReviewTaskRequest {
            channel_id,
            pull_number,
            agent_id,
        },
    )
    .await
}

#[tauri::command]
pub(crate) async fn rereview_github_pull_request(
    channel_id: Uuid,
    pull_number: i64,
    state: State<'_, AppState>,
) -> CommandResult<GithubRereviewTaskResult> {
    application::rereview_github_pull_request(
        &state.pool,
        RereviewGithubPullRequestRequest {
            channel_id,
            pull_number,
        },
    )
    .await
}

#[tauri::command]
pub(crate) async fn create_github_issue_task(
    channel_id: Uuid,
    issue_number: i64,
    agent_id: Uuid,
    state: State<'_, AppState>,
) -> CommandResult<GithubIssueTaskResult> {
    application::create_github_issue_task(
        &state.pool,
        CreateGithubIssueTaskRequest {
            channel_id,
            issue_number,
            agent_id,
        },
    )
    .await
}
