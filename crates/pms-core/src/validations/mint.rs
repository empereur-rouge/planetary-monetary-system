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
        // checked_add : pas de panic d'overflow sur des montants near-MAX (audit S2).
        let e = by_asset.entry(asset.clone()).or_insert(Decimal::ZERO);
        *e = e.checked_add(amount).ok_or(ValidationError::InvalidAmount {
            reason: "mint amount sum overflow".into(),
        })?;
    }
    Ok(by_asset)
}

/// Somme du collatéral UTILISABLE d'une réserve : UTXOs de l'asset
/// `collateral_asset_id` ENCORE time-lockés (`locked_until > now_ms`,
/// protocole 2.1). Un UTXO dont le lock a expiré n'est plus du collatéral —
/// l'émetteur peut le retirer à tout instant, il ne couvre donc plus rien.
/// Un UTXO sans lock ne compte pas non plus (même raison).
///
/// `utxos` = les UTXOs détenus à l'adresse de réserve (résolus par
/// l'appelant via `utxos_by_address`).
pub fn sum_locked_collateral(
    utxos: &[(pms_types::OutputId, TxOutput)],
    collateral_asset_id: &Option<String>,
    now_ms: u64,
) -> Decimal {
    utxos
        .iter()
        .filter(|(_, out)| {
            out.asset_id == *collateral_asset_id
                && out.locked_until.is_some_and(|until| until > now_ms)
        })
        .filter_map(|(_, out)| Decimal::from_str(&out.amount).ok())
        .sum()
}

/// Enforcement protocole du mint d'assets custom (plan 2.3 / 2.4).
///
/// Avant la v0.10.0, le mint n'était gardé que par « signataire ==
/// Coordinator » : `TokenMetadata.mint_authority` et `max_supply` existaient
/// dans le registre mais n'étaient PAS appliqués au niveau du protocole (le
/// handler `/admin/tokens/mint` les vérifiait côté API, contournable par tout
/// producteur de bloc autorisé). Cette fonction ferme le gap dans le hot path.
///
/// Pour chaque asset custom minté **enregistré dans le token registry** :
/// 1. **Autorité** : `signer == metadata.mint_authority` (hex,
///    case-insensitive). Le gate Coordinator de [`validate_mint_security`]
///    reste appliqué en amont (défense en profondeur) — ce check AJOUTE le
///    binding per-asset, il ne remplace pas le gate global.
/// 2. **Granularité (2.4)** : chaque montant ne dépasse pas
///    `metadata.decimals` décimales (un asset `decimals=0` ne mint pas 0.5).
/// 3. **Supply cap** : `circulating + minted <= max_supply` (si définie),
///    `circulating` venant du supply cache du `ShardedUtxoSet` (fourni par
///    l'appelant, déjà résolu).
/// 4. **Collatéral (2.3 v2)** : si `collateral_address` est défini,
///    `(circulating + minted) × collateral_ratio_bps / 10_000 <=
///    locked_collateral[asset]` — la somme des UTXOs de réserve encore
///    time-lockés ([`sum_locked_collateral`], résolue par l'appelant).
///    L'invariant porte sur l'ÉMISSION TOTALE, pas sur le delta : une
///    réserve dont les locks expirent bloque les mints suivants tant
///    qu'elle n'est pas re-verrouillée.
///
/// Un asset SANS metadata (jamais de `TokenCreate`) garde le comportement
/// historique : seul le gate Coordinator s'applique. Les refunds de contrats
/// (ex: `edenite-cube-burn` sur le ledger eden) mintent des assets non
/// enregistrés depuis la v0.2.0 — les rejeter casserait le flux production.
/// L'enregistrement est donc l'OPT-IN des contraintes : un émetteur qui veut
/// cap/authority enforced enregistre son asset via `TokenCreate`.
///
/// `minted` / `metadata` / `circulating` sont des maps pré-résolues par
/// l'appelant (`minted` via [`minted_amounts_by_custom_asset`] — persist.rs
/// l'a déjà en main, pas de double calcul ; lookups store + supply cache
/// async côté hot path) — la fonction reste pure et testable sans RocksDB.
pub fn validate_custom_asset_mints(
    outputs: &[TxOutput],
    signer_pk: &str,
    minted: &HashMap<String, Decimal>,
    metadata: &HashMap<String, Option<TokenMetadata>>,
    circulating: &HashMap<String, Decimal>,
    locked_collateral: &HashMap<String, Decimal>,
) -> Result<(), ValidationError> {
    let signer = signer_pk.trim();

    // 1. AUTORITÉ per-asset : le signataire du bloc (= Coordinateur, single-writer)
    //    DOIT être le `mint_authority` enregistré. Un asset non enregistré est
    //    skippé (comportement historique : refunds de contrats). Le mint custodial
    //    (protocole 2.8) NE passe PAS par cette fonction — il prouve l'autorité par
    //    une signature détachée du `mint_authority` (cf. arm CustodialMint du
    //    persist), puis réutilise les MÊMES money-rules ci-dessous.
    for (asset_id, _mint_amount) in minted {
        let Some(Some(meta)) = metadata.get(asset_id) else {
            continue;
        };
        if !signer.eq_ignore_ascii_case(meta.mint_authority.trim()) {
            tracing::error!(
                "🚫 Unauthorized token mint: asset={asset_id}, signer={} != mint_authority",
                &signer[..16.min(signer.len())]
            );
            return Err(ValidationError::UnauthorizedTokenMint(asset_id.clone()));
        }
    }

    // 2/3/4. Money-rules (granularité + cap + collatéral), partagées.
    validate_custom_asset_mint_amounts(outputs, minted, metadata, circulating, locked_collateral)
}

/// Money-rules per-asset **SANS contrôle d'autorité** : granularité `decimals`,
/// supply cap (`circulating + minted ≤ max_supply`), et couverture collatéral
/// (2.3 v2). Extrait de [`validate_custom_asset_mints`] pour être partagé, à
/// l'identique, par le `Mint` classique (autorité = signataire coordinateur,
/// vérifiée en amont) ET l'arm `CustodialMint` (autorité = signature
/// `mint_authority` détachée, vérifiée en amont). **Source unique** → la
/// granularité/cap/collatéral ne peuvent pas diverger entre les deux chemins.
///
/// Un asset absent de `metadata` (jamais de `TokenCreate`/`SftClassCreate`) est
/// skippé — comportement historique du `Mint` (refunds de contrats sur assets non
/// enregistrés). L'arm `CustodialMint` ne passe JAMAIS un asset non enregistré ici
/// (il résout fail-closed en amont et rejette l'inconnu).
pub fn validate_custom_asset_mint_amounts(
    outputs: &[TxOutput],
    minted: &HashMap<String, Decimal>,
    metadata: &HashMap<String, Option<TokenMetadata>>,
    circulating: &HashMap<String, Decimal>,
    locked_collateral: &HashMap<String, Decimal>,
) -> Result<(), ValidationError> {
    for (asset_id, mint_amount) in minted {
        let Some(Some(meta)) = metadata.get(asset_id) else {
            tracing::debug!(
                "Mint of unregistered asset {asset_id}: no TokenMetadata, per-asset constraints skipped"
            );
            continue;
        };

        // 2. Granularité : decimals de l'asset respectées par chaque output
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

        // 3. Supply cap
        if let Some(max_supply_str) = &meta.max_supply {
            let max_supply =
                Decimal::from_str(max_supply_str).map_err(|_| ValidationError::InvalidAmount {
                    reason: format!("token registry max_supply is not a decimal: {max_supply_str}"),
                })?;
            let current = circulating
                .get(asset_id)
                .copied()
                .unwrap_or(Decimal::ZERO);
            // `checked_add` : un `max_supply` proche de `Decimal::MAX` (le registry
            // n'impose pas de plafond) rendrait `current + mint` overflow → panic
            // sur input attaquant (S2 : même défense que `minted_amounts_by_custom_asset`).
            // Un overflow implique un total > tout `max_supply` valide → MaxSupplyExceeded.
            let total = current
                .checked_add(*mint_amount)
                .ok_or_else(|| ValidationError::MaxSupplyExceeded(asset_id.clone()))?;
            if total > max_supply {
                tracing::warn!(
                    "🚫 Max supply exceeded for {asset_id}: circulating={current} + mint={mint_amount} > max={max_supply}"
                );
                return Err(ValidationError::MaxSupplyExceeded(asset_id.clone()));
            }
        }

        // 4. Mint collatéralisé (2.3 v2) : l'émission totale doit rester
        // couverte par la réserve actuellement time-lockée.
        if meta.collateral_address.is_some() {
            // ratio garanti Some(>0) par validate_token_metadata au registry ;
            // défense en profondeur : metadata forgée hors registry → 10_000.
            let ratio = Decimal::from(meta.collateral_ratio_bps.unwrap_or(10_000));
            let current = circulating
                .get(asset_id)
                .copied()
                .unwrap_or(Decimal::ZERO);
            // Arithmétique checked (S2) : un `current + mint` ou un `× ratio` près de
            // `Decimal::MAX` overflow sinon → panic sur input attaquant. Un overflow
            // du collatéral requis = couverture impossible à prouver → rejet conservateur.
            let required = current
                .checked_add(*mint_amount)
                .and_then(|total| total.checked_mul(ratio))
                .and_then(|v| v.checked_div(Decimal::from(10_000)))
                .ok_or_else(|| ValidationError::InsufficientCollateral(asset_id.clone()))?;
            let locked = locked_collateral
                .get(asset_id)
                .copied()
                .unwrap_or(Decimal::ZERO);
            if locked < required {
                tracing::warn!(
                    "🚫 Insufficient collateral for {asset_id}: locked={locked} < required={required} \
                     ((circulating={current} + mint={mint_amount}) × {ratio} bps)"
                );
                return Err(ValidationError::InsufficientCollateral(asset_id.clone()));
            }
        }
    }
    Ok(())
}

