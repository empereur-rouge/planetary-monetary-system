use crate::api::AppState;
use axum::extract::State;
use axum::{Json, http::StatusCode};
use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct GetTipsReq {
    #[serde(default)]
    pub limit: usize,
}

/// Retrieves the top tips of the DAG for use as parents for new blocks.
/// Route: POST /v1/dag/tips
pub async fn get_tips(
    State(state): State<AppState>,
    Json(req): Json<GetTipsReq>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let limit = if req.limit == 0 { 10 } else { req.limit };

    match state.srv.adapter_arc().top_tips(limit).await {
        Ok(tips) => Ok(Json(tips)),
        Err(e) => {
            tracing::error!("Failed to get tips: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}
