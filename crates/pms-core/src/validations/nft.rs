//! Validation des actions NFT.
//!
//! Ce module vérifie que les actions NFT (Mint, Transfer, Use, Burn)
//! sont valides avant de les appliquer au stockage.
//!
//! ## Règles de validation
//! - **Mint** : Le token ne doit pas déjà exister, le signer doit correspondre au creator
//! - **Transfer** : Le token doit exister, le signer doit être le owner actuel
//! - **Use** : Le token doit exister, le signer doit être le owner
//! - **Burn** : Le token doit exister, le signer doit être le owner

use anyhow::{Result, anyhow};
use pms_storage::NftStorage;
use pms_types_nft::NftAction;

use super::cube_authority::validate_cube_authority_signature;

/// Erreurs de validation NFT.
#[derive(Debug, Clone)]
pub enum NftValidationError {
    /// Le token existe déjà lors d'un Mint
    TokenAlreadyExists { token_id: String },
    /// Le token n'existe pas lors d'un Transfer/Use/Burn
    TokenNotFound { token_id: String },
    /// Le signer n'est pas autorisé pour cette action
    Unauthorized {
        token_id: String,
        expected: String,
        got: String,
    },
}

impl std::fmt::Display for NftValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NftValidationError::TokenAlreadyExists { token_id } => {
                write!(f, "NFT already exists: {}", token_id)
            }
            NftValidationError::TokenNotFound { token_id } => {
                write!(f, "NFT not found: {}", token_id)
            }
            NftValidationError::Unauthorized {
                token_id,
                expected,
                got,
            } => {
                write!(
                    f,
                    "Unauthorized: token={}, expected owner={}, got signer={}",
                    token_id, expected, got
                )
            }
        }
    }
}

impl std::error::Error for NftValidationError {}

/// Valide une action NFT avant de l'appliquer.
///
/// # Arguments
/// - `action` : L'action NFT à valider
/// - `signer_pk_hex` : La clé publique du signataire (en hex)
/// - `coordinator_pk` : Clé publique du coordinateur (si configurée)
/// - `authority_pks` : Liste des clés publiques Authority pour les Cubes
/// - `nft_store` : Le store NFT pour vérifier l'ownership
///
/// # Règles
/// - **Mint** :
///     - Token ne doit pas exister
///     - Si `coordinator_pk` est défini, signer doit être le coordinateur
///     - Si `nft_type == "cube"` et `authority_pks` non vide, signature Authority requise
///     - Sinon (Dev), todo: warning
/// - **Transfer** : Token existe, from == owner actuel, signer autorisé
/// - **Use** : Token existe, user == owner, signer autorisé
/// - **Burn** : Token existe, burner == owner, signer autorisé
///
/// # Voir aussi
/// - Chapitre 9 du Rust Book : Error Handling
///   https://doc.rust-lang.org/book/ch09-00-error-handling.html
pub fn validate_nft_action<S: NftStorage>(
    action: &NftAction,
    signer_pk_hex: &str,
    coordinator_pk: Option<&str>,
    authority_pks: &[String],
    nft_store: &S,
) -> Result<()> {
    match action {
        // ═══════════════════════════════════════════════════════════════
        // MINT: Création d'un nouveau NFT
        // ═══════════════════════════════════════════════════════════════
        NftAction::Mint {
            token_id,
            creator,
            metadata,
        } => {
            // 1. Le token ne doit pas déjà exister
            if nft_store.exists(token_id)? {
                return Err(anyhow!(NftValidationError::TokenAlreadyExists {
                    token_id: token_id.clone(),
                }));
            }

            // 2. Vérification du Coordinateur (Si configuré)
            if let Some(coord_key) = coordinator_pk {
                // Production/Testnet : Seul le coordinateur peut minter
                if !signer_pk_hex.eq_ignore_ascii_case(coord_key) {
                    return Err(anyhow!(NftValidationError::Unauthorized {
                        token_id: token_id.clone(),
                        expected: format!("Coordinator({})", coord_key),
                        got: signer_pk_hex.to_string(),
                    }));
                }
            } else {
                // Mode Dev (pas de coordinateur configuré)
                // On laisse passer, mais idéalement on loguerait un warning
            }

            // 3. Le signer doit être le creator
            // Note: Si c'est le coordinateur qui mint, il est le creator par défaut
            // ou bien il mint "pour" quelqu'un d'autre ?
            // Pour l'instant on garde la logique précédente : creator == signer
            // Sauf si on veut permettre au coord de minter POUR un user.
            // Dans le doute, on enforce que le creator déclaré soit le signer (donc le coord).
            if !creator.eq_ignore_ascii_case(signer_pk_hex) {
                return Err(anyhow!(NftValidationError::Unauthorized {
                    token_id: token_id.clone(),
                    expected: creator.clone(),
                    got: signer_pk_hex.to_string(),
                }));
            }

            // 4. Validation Cube: Si c'est un cube et que des Authority keys sont configurées,
            //    vérifier la signature des attributs
            if metadata.nft_type.as_deref() == Some("cube") {
                if !authority_pks.is_empty() {
                    validate_cube_authority_signature(metadata, authority_pks)?;
                    tracing::debug!("✅ Cube {} Authority signature validated", token_id);
                } else {
                    // Pas d'Authority configurée en mode Dev, on laisse passer
                    tracing::warn!(
                        "⚠️ Cube {} minted without Authority validation (no authority_pks configured)",
                        token_id
                    );
                }
            }

            Ok(())
        }

        // ═══════════════════════════════════════════════════════════════
        // TRANSFER: Transfert de propriété
        // ═══════════════════════════════════════════════════════════════
        NftAction::Transfer { token_id, from, .. } => {
            // 1. Le token doit exister
            let owner = nft_store.get_owner(token_id)?.ok_or_else(|| {
                anyhow!(NftValidationError::TokenNotFound {
                    token_id: token_id.clone(),
                })
            })?;

            // 2. `from` doit être le owner actuel
            if !owner.eq_ignore_ascii_case(from) {
                return Err(anyhow!(NftValidationError::Unauthorized {
                    token_id: token_id.clone(),
                    expected: owner,
                    got: from.clone(),
                }));
            }

            // 3. Le signer doit être autorisé (soit le owner, soit une clé associée)
            if !from.eq_ignore_ascii_case(signer_pk_hex) {
                return Err(anyhow!(NftValidationError::Unauthorized {
                    token_id: token_id.clone(),
                    expected: from.clone(),
                    got: signer_pk_hex.to_string(),
                }));
            }

            Ok(())
        }

        // ═══════════════════════════════════════════════════════════════
        // USE: Utilisation du NFT (sans transfert)
        // ═══════════════════════════════════════════════════════════════
        NftAction::Use { token_id, user, .. } => {
            // 1. Le token doit exister
            let owner = nft_store.get_owner(token_id)?.ok_or_else(|| {
                anyhow!(NftValidationError::TokenNotFound {
                    token_id: token_id.clone(),
                })
            })?;

            // 2. `user` doit être le owner
            if !owner.eq_ignore_ascii_case(user) {
                return Err(anyhow!(NftValidationError::Unauthorized {
                    token_id: token_id.clone(),
                    expected: owner,
                    got: user.clone(),
                }));
            }

            // 3. Le signer doit être le user
            if !user.eq_ignore_ascii_case(signer_pk_hex) {
                return Err(anyhow!(NftValidationError::Unauthorized {
                    token_id: token_id.clone(),
                    expected: user.clone(),
                    got: signer_pk_hex.to_string(),
                }));
            }

            Ok(())
        }

        // ═══════════════════════════════════════════════════════════════
        // BURN: Destruction du NFT
        // ═══════════════════════════════════════════════════════════════
        NftAction::Burn { token_id, burner } => {
            // 1. Le token doit exister
            let owner = nft_store.get_owner(token_id)?.ok_or_else(|| {
                anyhow!(NftValidationError::TokenNotFound {
                    token_id: token_id.clone(),
                })
            })?;

            // 2. `burner` doit être le owner
            if !owner.eq_ignore_ascii_case(burner) {
                return Err(anyhow!(NftValidationError::Unauthorized {
                    token_id: token_id.clone(),
                    expected: owner,
                    got: burner.clone(),
                }));
            }

            // 3. Le signer doit être le burner
            if !burner.eq_ignore_ascii_case(signer_pk_hex) {
                return Err(anyhow!(NftValidationError::Unauthorized {
                    token_id: token_id.clone(),
                    expected: burner.clone(),
                    got: signer_pk_hex.to_string(),
                }));
            }

            Ok(())
        }
    }
}
