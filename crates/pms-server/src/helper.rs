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
///
/// AUDIT M-8 (v0.9.0) : l'ancienne version branchait sur l'égalité des
/// longueurs et exécutait un `ct_eq` factice dont le coût dépendait de la
/// longueur — un attaquant mesurant le timing pouvait en déduire la longueur
/// du token admin. On compare désormais les digests SHA-256 des deux côtés :
/// taille fixe 32 octets, aucune branche dépendante du secret. Le coût de
/// hachage de `a` ne dépend que de l'entrée de l'attaquant (information qu'il
/// possède déjà) et celui de `b` est constant pour un token donné.
fn constant_time_compare(a: &str, b: &str) -> bool {
    use sha2::{Digest, Sha256};
    let ha = Sha256::digest(a.as_bytes());
    let hb = Sha256::digest(b.as_bytes());
    ha.ct_eq(&hb).into()
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

#[cfg(test)]
mod tests {
    use super::constant_time_compare;

    #[test]
    fn equal_tokens_match() {
        println!("compare('secret-token', 'secret-token')");
        assert!(constant_time_compare("secret-token", "secret-token"));
    }

    #[test]
    fn different_tokens_reject() {
        println!("compare('secret-token', 'secret-tokeX')");
        assert!(!constant_time_compare("secret-token", "secret-tokeX"));
    }

    #[test]
    fn different_lengths_reject() {
        // Cas M-8 : longueurs différentes — doit rejeter sans branche
        // dépendante de la longueur du secret (digests SHA-256 fixes).
        println!("compare('short', 'a-much-longer-admin-token-value')");
        assert!(!constant_time_compare(
            "short",
            "a-much-longer-admin-token-value"
        ));
    }

    #[test]
    fn empty_vs_nonempty_reject() {
        assert!(!constant_time_compare("", "token"));
        assert!(constant_time_compare("", ""));
    }
}
