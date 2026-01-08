//! Validation de la sécurité du Minting.
//!
//! Ce module contient toute la logique de vérification que seul le
//! Coordinateur peut créer de nouveaux tokens (Mint).
//!
//! # Architecture
//!
//! La clé publique du Coordinateur est définie dans `pms-consensus`.
//! Chaque bloc `Mint` doit être signé par cette clé pour être accepté.
//!
//! # Exemple
//!
//! ```ignore
//! use pms_core::validations::mint::validate_mint_security;
//!
//! // Vérifier qu'un WireBlock Mint est signé par le Coordinateur
//! let result = validate_mint_security(&wire_block, &policy);
//! assert!(result.is_ok());
//! ```
//!
//! Voir Chapitre 10.2 du Rust Book pour comprendre les traits utilisés ici.

use crate::validations::check::ValidatePolicy;
use pms_errors::ValidationError;

/// Validation de sécurité pour les WireBlocks (appelée depuis net_adapter).
///
/// # Arguments
/// * `wb` - Le WireBlock reçu du réseau
/// * `policy` - La politique de validation (contient la clé coordinateur)
///
/// # Returns
/// * `Ok(())` si le signataire est le Coordinateur
/// * `Err(UnauthorizedMint)` sinon
///
/// # Sécurité
/// Cette fonction est CRITIQUE pour l'économie du réseau.
/// Elle empêche la création arbitraire de tokens par des acteurs malveillants.
pub fn validate_mint_security(
    wb: &pms_wire::WireBlock,
    policy: &ValidatePolicy,
) -> Result<(), ValidationError> {
    validate_mint_security_logic(&wb.signer_pk_hex, &wb.id, policy)
}

/// Logique centrale de vérification du droit de mint.
///
/// Compare la clé publique du signataire avec la clé Coordinateur configurée.
/// En mode Dev (sans clé configurée), un warning est émis mais le mint est autorisé.
///
/// # Arguments
/// * `signer_pk` - La clé publique hexadécimale du signataire
/// * `block_id` - L'identifiant du bloc (pour les messages d'erreur)
/// * `policy` - La politique de validation
///
/// # Règles
/// 1. Si une clé Coordinateur est configurée, le signataire DOIT correspondre
/// 2. Si aucune clé n'est configurée (Dev), on autorise avec un warning
/// 3. La comparaison est insensible à la casse (hex)
pub fn validate_mint_security_logic(
    signer_pk: &str,
    block_id: &str,
    policy: &ValidatePolicy,
) -> Result<(), ValidationError> {
    // Nettoie les espaces autour de la clé
    let signer = signer_pk.trim();

    if let Some(coord_key) = &policy.coordinator_public_key {
        // Règle de Production : Seul le Coordinateur peut Minter
        if !signer.eq_ignore_ascii_case(coord_key) {
            tracing::error!(
                "❌ Unauthorized Mint: Signer='{}' vs Expected Coordinator='{}'",
                signer,
                coord_key
            );
            return Err(ValidationError::UnauthorizedMint {
                id: block_id.to_string(),
                signer_pk_hex: signer.to_string(),
            });
        }
    } else {
        // Mode Dev : Pas de clé coordinateur configurée
        // On autorise le mint mais on log un avertissement
        tracing::warn!(
            "⚠️ Mint autorisé SANS clé Coordinateur configurée (Mode Dev?). Block: {}",
            block_id
        );
    }
    Ok(())
}
