use serde::Deserialize;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{
    app::CommandResult,
    decision_store::{answer_decision_in_pool, dismiss_decision_in_pool},
    models::Decision,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AnswerDecisionRequest {
    pub(crate) decision_id: Uuid,
    #[serde(default)]
    pub(crate) option_id: Option<String>,
    #[serde(default)]
    pub(crate) note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DecisionIdRequest {
    pub(crate) decision_id: Uuid,
}

pub(crate) async fn answer_decision(
    pool: &SqlitePool,
    request: AnswerDecisionRequest,
) -> CommandResult<Decision> {
    answer_decision_in_pool(
        pool,
        request.decision_id,
        request.option_id.as_deref(),
        request.note.as_deref(),
    )
    .await
}

pub(crate) async fn dismiss_decision(
    pool: &SqlitePool,
    request: DecisionIdRequest,
) -> CommandResult<()> {
    dismiss_decision_in_pool(pool, request.decision_id).await
}
