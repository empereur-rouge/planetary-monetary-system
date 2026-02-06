use crate::api::AppState;
use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use subtle::ConstantTimeEq;

/// Résout une config de token admin.
/// - "env:VAR" → lit VAR dans l'environnement
/// - "valeur-littérale" → retourne la valeur brute
pub fn resolve_admin_token(spec: &str) -> Option<String> {
    if let Some(var) = spec.strip_prefix("env:") {
        std::env::var(var).ok()
    } else {
        Some(spec.to_string())
    }
}

/// Comparaison constant-time pour éviter les timing attacks.
/// Retourne true si les deux chaînes sont identiques.
fn constant_time_compare(a: &str, b: &str) -> bool {
    // Si les longueurs diffèrent, on compare quand même en temps constant
    // pour ne pas révéler d'information sur la longueur
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();

    if a_bytes.len() != b_bytes.len() {
        // Compare avec lui-même pour maintenir le temps constant
        let _ = a_bytes.ct_eq(a_bytes);
        return false;
    }

    a_bytes.ct_eq(b_bytes).into()
}

/// Vérifie si la requête est autorisée en tant qu'admin.
///
/// On accepte :
///  - Authorization: Bearer <token>
///  - X-Admin-Token: <token>
///
/// SÉCURITÉ: Utilise une comparaison constant-time pour éviter les timing attacks.
pub fn is_admin_authorized(state: &AppState, headers: &HeaderMap) -> bool {
    let expected = match &state.admin_token {
        Some(t) if !t.is_empty() => t,
        _ => return false, // aucune config = aucune route admin
    };

    // 1) Authorization: Bearer ...
    if let Some(value) = headers.get(AUTHORIZATION) {
        if let Ok(s) = value.to_str() {
            if let Some(rest) = s.strip_prefix("Bearer ") {
                if constant_time_compare(rest.trim(), expected) {
                    return true;
                }
            }
        }
    }

    // 2) X-Admin-Token
    if let Some(value) = headers.get("X-Admin-Token") {
        if let Ok(s) = value.to_str() {
            if constant_time_compare(s.trim(), expected) {
                return true;
            }
        }
    }

    false
}
