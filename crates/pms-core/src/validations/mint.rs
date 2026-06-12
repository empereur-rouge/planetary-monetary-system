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
use pms_types::{TokenMetadata, TxOutput};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;

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

/// Montants mintés par asset custom (`asset_id` = `Some(..)`) d'un payload Mint.
/// Les outputs PMS natifs (`asset_id = None`) sont ignorés — ils restent
/// couverts par `validate_mint_policy` + le gate Coordinator.
pub fn minted_amounts_by_custom_asset(
    outputs: &[TxOutput],
) -> Result<HashMap<String, Decimal>, ValidationError> {
    let mut by_asset: HashMap<String, Decimal> = HashMap::new();
    for out in outputs {
        let Some(asset) = &out.asset_id else { continue };
        let amount =
            Decimal::from_str(&out.amount).map_err(|_| ValidationError::InvalidAmount {
                reason: format!("mint output amount is not a decimal: {}", out.amount),
            })?;
        if amount <= Decimal::ZERO {
            return Err(ValidationError::InvalidAmount {
                reason: "mint output amount must be > 0".into(),
            });
        }
        *by_asset.entry(asset.clone()).or_insert(Decimal::ZERO) += amount;
    }
    Ok(by_asset)
}

/// Enforcement protocole du mint d'assets custom (plan 2.3 / 2.4).
///
/// Avant la v0.10.0, le mint n'était gardé que par « signataire ==
/// Coordinator » : `TokenMetadata.mint_authority` et `max_supply` existaient
/// dans le registre mais n'étaient PAS appliqués au niveau du protocole (le
/// handler `/admin/tokens/mint` les vérifiait côté API, contournable par tout
/// producteur de bloc autorisé). Cette fonction ferme le gap dans le hot path.
///
/// Pour chaque asset custom minté :
/// 1. **Enregistrement** : l'asset doit exister dans le token registry
///    (`TokenCreate` préalable) → sinon [`ValidationError::TokenNotRegistered`].
/// 2. **Autorité** : `signer == metadata.mint_authority` (hex,
///    case-insensitive). Le gate Coordinator de [`validate_mint_security`]
///    reste appliqué en amont (défense en profondeur) — ce check AJOUTE le
///    binding per-asset, il ne remplace pas le gate global.
/// 3. **Granularité (2.4)** : chaque montant ne dépasse pas
///    `metadata.decimals` décimales (un asset `decimals=0` ne mint pas 0.5).
/// 4. **Supply cap** : `circulating + minted <= max_supply` (si définie),
///    `circulating` venant du supply cache du `ShardedUtxoSet` (fourni par
///    l'appelant, déjà résolu).
///
/// `metadata` / `circulating` sont des maps pré-résolues par l'appelant
/// (persist.rs fait les lookups store + supply cache async) — la fonction
/// reste pure et testable sans RocksDB.
pub fn validate_custom_asset_mints(
    outputs: &[TxOutput],
    signer_pk: &str,
    metadata: &HashMap<String, Option<TokenMetadata>>,
    circulating: &HashMap<String, Decimal>,
) -> Result<(), ValidationError> {
    let minted = minted_amounts_by_custom_asset(outputs)?;
    let signer = signer_pk.trim();

    for (asset_id, mint_amount) in &minted {
        // 1. Asset enregistré
        let Some(Some(meta)) = metadata.get(asset_id) else {
            tracing::warn!("🚫 Mint of unregistered asset blocked: {asset_id}");
            return Err(ValidationError::TokenNotRegistered(asset_id.clone()));
        };

        // 2. Autorité per-asset
        if !signer.eq_ignore_ascii_case(meta.mint_authority.trim()) {
            tracing::error!(
                "🚫 Unauthorized token mint: asset={asset_id}, signer={} != mint_authority",
                &signer[..16.min(signer.len())]
            );
            return Err(ValidationError::UnauthorizedTokenMint(asset_id.clone()));
        }

        // 3. Granularité : decimals de l'asset respectées par chaque output
        for out in outputs.iter().filter(|o| o.asset_id.as_deref() == Some(asset_id)) {
            let amount = Decimal::from_str(&out.amount).unwrap_or(Decimal::ZERO);
            if amount.normalize().scale() > meta.decimals as u32 {
                return Err(ValidationError::InvalidAmount {
                    reason: format!(
                        "asset {asset_id} supports {} decimals, got amount {}",
                        meta.decimals, out.amount
                    ),
                });
            }
        }

        // 4. Supply cap
        if let Some(max_supply_str) = &meta.max_supply {
            let max_supply =
                Decimal::from_str(max_supply_str).map_err(|_| ValidationError::InvalidAmount {
                    reason: format!("token registry max_supply is not a decimal: {max_supply_str}"),
                })?;
            let current = circulating
                .get(asset_id)
                .copied()
                .unwrap_or(Decimal::ZERO);
            if current + mint_amount > max_supply {
                tracing::warn!(
                    "🚫 Max supply exceeded for {asset_id}: circulating={current} + mint={mint_amount} > max={max_supply}"
                );
                return Err(ValidationError::MaxSupplyExceeded(asset_id.clone()));
            }
        }
    }
    Ok(())
}
