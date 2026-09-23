use tauri::State;
use uuid::Uuid;

use crate::{
    app::{AppState, CommandResult},
    application::decisions::{self as application, AnswerDecisionRequest, DecisionIdRequest},
    models::Decision,
};

#[tauri::command]
pub(crate) async fn answer_decision(
    decision_id: Uuid,
    option_id: Option<String>,
    note: Option<String>,
    state: State<'_, AppState>,
) -> CommandResult<Decision> {
    application::answer_decision(
        &state.pool,
        AnswerDecisionRequest {
            decision_id,
            option_id,
            note,
        },
    )
    .await
}

#[tauri::command]
pub(crate) async fn dismiss_decision(
    decision_id: Uuid,
    state: State<'_, AppState>,
) -> CommandResult<()> {
    application::dismiss_decision(&state.pool, DecisionIdRequest { decision_id }).await
}
