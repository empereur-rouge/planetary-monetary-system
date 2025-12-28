use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use crate::api::AppState;

#[derive(Deserialize)]
pub struct StreamQuery { after: Option<String>, pub(crate) limit: Option<usize> }

pub async fn stream_blocks(
    State(app): State<AppState>,
    Query(q): Query<StreamQuery>,
) -> Result<Json<Vec<pms_wire::WireBlock>>, (StatusCode, String)> {
    let lim = q.limit.unwrap_or(200).min(500);
    // Impl simple: depuis le store, récupérer les derniers IDs > after, ordre topo/temps selon votre index
    // MVP: si pas d’index d’ordre, renvoyer “les N plus récents”
    let ids = app.srv.adapter_arc().recent_ids(lim).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let blocks = app.srv.adapter_arc().get_blocks_by_ids(&ids).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(blocks))
}