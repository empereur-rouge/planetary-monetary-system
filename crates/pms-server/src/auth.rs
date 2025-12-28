use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    RequestExt,
};
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use pms_config::load_config;
use crate::api::AppState;

pub struct AdminAuth;

#[derive(Debug)]
pub struct AuthError;

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        (StatusCode::UNAUTHORIZED, "unauthorized").into_response()
    }
}

impl AdminAuth {
    pub async fn check<B>(
        State(state): State<AppState>,
        req: axum::http::Request<B>,
    ) -> Result<axum::http::Request<B>, AuthError> {
        let settings = load_config();
        let expected = format!("Bearer {:?}", settings.unwrap().auth.admin_api_token);

        let auth = req
            .headers()
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if auth == expected {
            Ok(req)
        } else {
            Err(AuthError)
        }
    }
}