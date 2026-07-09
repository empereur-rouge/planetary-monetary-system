use thiserror::Error;

#[derive(Debug, Error)]
pub enum SimError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Server error ({status}): {message}")]
    ServerError { status: u16, message: String },

    #[error("Gemini API error: {0}")]
    Gemini(String),

    #[error("Insufficient balance for agent {agent}: has {available}, needs {required}")]
    InsufficientBalance {
        agent: String,
        available: String,
        required: String,
    },

    #[error("Config error: {0}")]
    Config(String),

    #[error("Bootstrap failed: {0}")]
    Bootstrap(String),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl SimError {
    /// Discrimine les erreurs d'ÉTAT durable des erreurs transitoires
    /// (règle globale « state-divergence vs transient errors »).
    ///
    /// **Erreur d'état** (`true`) : le serveur a répondu et affirme que
    /// l'état distant rend l'opération impossible — solde insuffisant,
    /// ressource inconnue, déjà consommée. Retenter immédiatement ne peut
    /// pas réussir tant qu'un tiers n'a pas changé l'état : l'appelant doit
    /// passer en backoff (cf. [`crate::backoff::StateBackoff`]).
    ///
    /// **Erreur transitoire** (`false`) : réseau, timeout, 5xx — l'état
    /// distant est peut-être inchangé, un retry rapide est légitime.
    pub fn is_state_error(&self) -> bool {
        match self {
            SimError::InsufficientBalance { .. } => true,
            SimError::ServerError { status, message } => {
                // 404 = signal canonique de state-divergence, quel que soit
                // le body (cf. règle globale : la ressource n'existe pas/plus).
                if *status == 404 {
                    return true;
                }
                if !(400..=499).contains(status) {
                    return false;
                }
                // Wire format cible `{"code": NNNN, "message": "..."}`
                // (migration ApiError planifiée) : la tranche 3xxx =
                // état/business. Prioritaire sur les substrings car les
                // messages publics migrés seront volontairement vagues.
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(message) {
                    if let Some(code) = v.get("code").and_then(|c| c.as_u64()) {
                        return (3000..=3999).contains(&code);
                    }
                }
                // Format legacy : wording du serveur dans le body brut.
                let m = message.to_lowercase();
                m.contains("insufficient balance")
                    || m.contains("not found")
                    || m.contains("already spent")
                    || m.contains("already burned")
            }
            _ => false,
        }
    }
}

pub type SimResult<T> = Result<T, SimError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_state_error_classification() {
        // Message de production réel (testnet 2026-07-09, refuel coordinator à sec)
        let cases: Vec<(SimError, bool, &str)> = vec![
            (
                SimError::ServerError {
                    status: 422,
                    message: r#"{"error":"insufficient balance: available=0.07962557, required=51.5000001"}"#.into(),
                },
                true,
                "422 insufficient balance (prod)",
            ),
            (
                SimError::InsufficientBalance {
                    agent: "spammer-1".into(),
                    available: "0.08".into(),
                    required: "51.5".into(),
                },
                true,
                "variante structurée InsufficientBalance",
            ),
            (
                SimError::ServerError {
                    status: 404,
                    message: r#"{"error":"wallet not found"}"#.into(),
                },
                true,
                "404 not found",
            ),
            (
                SimError::ServerError {
                    status: 404,
                    message: String::new(),
                },
                true,
                "404 body vide (statut seul = state-divergence)",
            ),
            (
                SimError::ServerError {
                    status: 422,
                    message: r#"{"code":3005,"message":"operation not permitted"}"#.into(),
                },
                true,
                "code ApiError 3xxx (état/business, message vague)",
            ),
            (
                SimError::ServerError {
                    status: 422,
                    message: r#"{"code":2001,"message":"invalid address format"}"#.into(),
                },
                false,
                "code ApiError 2xxx (validation, pas un état distant)",
            ),
            (
                SimError::ServerError {
                    status: 503,
                    message: r#"{"error":"read_only","reason":"memory"}"#.into(),
                },
                false,
                "503 read-only (transitoire: retry légitime)",
            ),
            (
                SimError::ServerError {
                    status: 429,
                    message: "rate limited".into(),
                },
                false,
                "429 rate-limit (transitoire)",
            ),
            (
                SimError::Other(anyhow::anyhow!("connection reset by peer")),
                false,
                "erreur réseau (transitoire)",
            ),
        ];

        for (err, expected, label) in cases {
            let got = err.is_state_error();
            println!("{label}: is_state_error = {got} (attendu {expected}) — {err}");
            assert_eq!(got, expected, "{label}");
        }
    }
}
