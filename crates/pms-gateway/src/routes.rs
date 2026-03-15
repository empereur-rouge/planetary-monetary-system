use crate::GatewayState;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::Json;
use http::StatusCode;
use pms_wire::WireBlock;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct HealthResp {
    status: String,
    block_count: usize,
}

pub async fn healthz(State(state): State<GatewayState>) -> impl IntoResponse {
    match state
        .engine_client
        .get::<HealthResp>("/internal/health")
        .await
    {
        Ok(resp) => (StatusCode::OK, Json(resp)),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthResp {
                status: "engine_unreachable".into(),
                block_count: 0,
            }),
        ),
    }
}

#[derive(Serialize, Deserialize)]
pub struct TipsResp {
    tips: Vec<String>,
}

pub async fn get_tips(State(state): State<GatewayState>) -> impl IntoResponse {
    match state.engine_client.get::<TipsResp>("/internal/tips").await {
        Ok(resp) => (StatusCode::OK, Json(resp)),
        Err(_) => (StatusCode::BAD_GATEWAY, Json(TipsResp { tips: vec![] })),
    }
}

#[derive(Serialize, Deserialize)]
pub struct UtxoItem {
    txid: String,
    index: u32,
    amount: String,
    address: String,
}

#[derive(Serialize, Deserialize)]
pub struct UtxosResp {
    utxos: Vec<UtxoItem>,
}

pub async fn get_utxos(
    State(state): State<GatewayState>,
    Path(address): Path<String>,
) -> impl IntoResponse {
    let path = format!("/internal/utxos/{}", address);
    match state.engine_client.get::<UtxosResp>(&path).await {
        Ok(resp) => (StatusCode::OK, Json(resp)),
        Err(_) => (StatusCode::BAD_GATEWAY, Json(UtxosResp { utxos: vec![] })),
    }
}

#[derive(Serialize, Deserialize)]
pub struct BlockResp {
    id: String,
    parents: Vec<String>,
    payload_json: Option<String>,
    nonce: u64,
}

pub async fn get_block(
    State(state): State<GatewayState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let path = format!("/internal/block/{}", id);
    match state.engine_client.get::<Option<BlockResp>>(&path).await {
        Ok(Some(resp)) => (StatusCode::OK, Json(Some(resp))),
        Ok(None) => (StatusCode::NOT_FOUND, Json(None)),
        Err(_) => (StatusCode::BAD_GATEWAY, Json(None)),
    }
}

#[derive(Serialize, Deserialize)]
pub struct SubmitBlockReq {
    block: WireBlock,
}

#[derive(Serialize, Deserialize)]
pub struct SubmitBlockResp {
    status: String,
    id: String,
    reason: Option<String>,
}

pub async fn submit_block(
    State(state): State<GatewayState>,
    Json(req): Json<SubmitBlockReq>,
) -> impl IntoResponse {
    match state
        .engine_client
        .post::<_, SubmitBlockResp>("/internal/submit_block", &req)
        .await
    {
        Ok(resp) => {
            let status = match resp.status.as_str() {
                "inserted" => StatusCode::CREATED,
                "duplicate" => StatusCode::CONFLICT,
                "rejected" => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(resp))
        }
        Err(_) => (
            StatusCode::BAD_GATEWAY,
            Json(SubmitBlockResp {
                status: "gateway_error".into(),
                id: String::new(),
                reason: Some("Engine unreachable".into()),
            }),
        ),
    }
}

#[derive(Serialize, Deserialize)]
pub struct ConfigResp {
    fee_rate_bps: u32,
    base_fee: String,
    coordinator_fee_bps: u32,
    treasury_fee_bps: u32,
}

pub async fn get_config(State(state): State<GatewayState>) -> impl IntoResponse {
    match state
        .engine_client
        .get::<ConfigResp>("/internal/config")
        .await
    {
        Ok(resp) => (StatusCode::OK, Json(resp)),
        Err(_) => (
            StatusCode::BAD_GATEWAY,
            Json(ConfigResp {
                fee_rate_bps: 0,
                base_fee: "0".into(),
                coordinator_fee_bps: 0,
                treasury_fee_bps: 0,
            }),
        ),
    }
}

/// Catch-all fallback: proxies any unmatched request to Engine.
/// Supports all HTTP methods (GET, POST, PUT, PATCH, DELETE).
/// The Engine's response (status, headers, body) is forwarded as-is.
pub async fn proxy_fallback(
    State(state): State<GatewayState>,
    method: http::Method,
    axum::extract::OriginalUri(original_uri): axum::extract::OriginalUri,
    headers: http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let path = original_uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(original_uri.path());

    match state
        .engine_client
        .proxy_request(method.clone(), path, headers, body)
        .await
    {
        Ok((status, resp_body, content_type)) => {
            let ct = content_type.unwrap_or_else(|| "application/json".to_string());
            (status, [(http::header::CONTENT_TYPE, ct)], resp_body).into_response()
        }
        Err(e) => {
            // Use {:?} (Debug) to show the full error chain — reqwest's Display
            // only shows the top-level "error sending request for url" without
            // the underlying cause (TLS failure, DNS error, connection refused…).
            tracing::warn!("Proxy {method} {path} failed: {e:?}");
            (
                StatusCode::BAD_GATEWAY,
                [(http::header::CONTENT_TYPE, "text/plain".to_string())],
                format!("Gateway error: {e}"),
            )
                .into_response()
        }
    }
}

/// Proxy handler for streaming GET requests - forwards to Engine
pub async fn proxy_stream(
    State(state): State<GatewayState>,
    axum::extract::OriginalUri(original_uri): axum::extract::OriginalUri,
    headers: http::HeaderMap,
) -> impl IntoResponse {
    let path = original_uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(original_uri.path());

    match state.engine_client.proxy_stream(path, headers).await {
        Ok((status, body, content_type)) => {
            let ct = content_type.unwrap_or_else(|| "text/event-stream".to_string());
            (status, [(http::header::CONTENT_TYPE, ct)], body)
        }
        Err(e) => {
            tracing::warn!("Proxy STREAM {} failed: {:?}", path, e);
            let body = axum::body::Body::from(format!("Gateway error: {}", e));
            (
                StatusCode::BAD_GATEWAY,
                [(http::header::CONTENT_TYPE, "text/plain".to_string())],
                body,
            )
        }
    }
}
