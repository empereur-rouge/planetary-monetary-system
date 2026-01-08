use crate::api::AppState;
use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;

/// Résout une config de token admin.
/// - "env:VAR" → lit VAR dans l’environnement
/// - "valeur-littérale" → retourne la valeur brute
pub fn resolve_admin_token(spec: &str) -> Option<String> {
    if let Some(var) = spec.strip_prefix("env:") {
        std::env::var(var).ok()
    } else {
        Some(spec.to_string())
    }
}

/// Vérifie si la requête est autorisée en tant qu’admin.
///
/// On accepte :
///  - Authorization: Bearer <token>
///  - X-Admin-Token: <token>
pub fn is_admin_authorized(state: &AppState, headers: &HeaderMap) -> bool {
    let expected = match &state.admin_token {
        Some(t) if !t.is_empty() => t,
        _ => return false, // aucune config = aucune route admin
    };

    // 1) Authorization: Bearer ...
    if let Some(value) = headers.get(AUTHORIZATION) {
        if let Ok(s) = value.to_str() {
            if let Some(rest) = s.strip_prefix("Bearer ") {
                if rest.trim() == expected {
                    return true;
                }
            }
        }
    }

    // 2) X-Admin-Token
    if let Some(value) = headers.get("X-Admin-Token") {
        if let Ok(s) = value.to_str() {
            if s.trim() == expected {
                return true;
            }
        }
    }

    false
}
