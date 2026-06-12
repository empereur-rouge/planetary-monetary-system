//! Conditions de déverrouillage des outputs (protocole 2.2) : MultiSig M-of-N
//! et HashLock, généralisation du binding C-1 ([`super::ownership`]).
//!
//! # Modèle
//!
//! La condition est portée par l'OUTPUT (pattern Bitcoin scriptPubKey) et
//! figée à sa création. À la dépense, le validateur lit la condition depuis
//! l'UTXO **stocké** — jamais depuis les données fournies par le dépensier —
//! puis vérifie que l'`Unlock` apparié la satisfait :
//!
//! - `None` / `PubKey`  → binding C-1 : la pubkey de l'unlock dérive l'adresse.
//! - `MultiSig {m, pubkeys}` → ≥ `m` signatures valides de clés DISTINCTES du
//!   set. Les signatures (principale + cosigners) sont vérifiées
//!   cryptographiquement par `verify_tx_signatures` ; ici on vérifie
//!   l'appartenance au set et le quorum. L'adresse de l'output engage la
//!   policy : elle DOIT être [`multisig_address`] (vérifié à la création).
//! - `HashLock {hash_hex}` → l'unlock fournit `preimage_hex` tel que
//!   `SHA256(preimage) == hash_hex`.
//!
//! # Sécurité
//!
//! - Le quorum MultiSig compte les pubkeys **normalisées et dédupliquées** —
//!   répéter la même cosignature N fois ne fabrique pas un quorum.
//! - `m == 0` est rejeté à la création ET à la dépense (défense en
//!   profondeur : un UTXO forgé hors pipeline ne devient pas un anyone-can-spend).
//! - Les échecs de dépense renvoient `SpendConditionNotMet` volontairement
//!   vague (anti-enumeration) ; le détail est loggé via `tracing`.

use pms_errors::ValidationError;
use pms_types::{SpendCondition, TxOutput, Unlock};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

use super::ownership::unlock_matches_address;

/// Nombre maximal de clés d'une policy MultiSig.
pub const MAX_MULTISIG_KEYS: usize = 16;

/// Préfixe humain des adresses multisig canoniques.
pub const MULTISIG_ADDR_PREFIX: &str = "msig1";

/// Tag de domaine du hash d'adresse multisig (versionné).
const MULTISIG_DOMAIN_TAG: &[u8] = b"pms-multisig-v1";

/// Normalise une pubkey hex pour comparaison/hachage : trim, sans préfixe
/// `0x`, lowercase. Même convention que le binding C-1.
fn normalize_pk(pk: &str) -> String {
    pk.trim().trim_start_matches("0x").to_ascii_lowercase()
}

/// Dérive l'adresse multisig CANONIQUE d'une policy M-of-N.
///
/// `addr = "msig1" + hex(SHA256("pms-multisig-v1" || m || n || pk_1 || … || pk_n)[..20])`
/// où les pubkeys sont normalisées puis TRIÉES — l'ordre de déclaration ne
/// change pas l'adresse. L'adresse engage donc la policy complète : impossible
/// de dépenser en présentant un autre set de clés que celui de la création.
///
/// # Examples
///
/// ```
/// use pms_core::validations::conditions::multisig_address;
/// let a1 = multisig_address(2, &["04AA".into(), "04bb".into()]);
/// let a2 = multisig_address(2, &["04bb".into(), "0x04aa".into()]);
/// assert_eq!(a1, a2); // ordre + casse + 0x indifférents
/// assert!(a1.starts_with("msig1"));
/// ```
pub fn multisig_address(m: u8, pubkeys: &[String]) -> String {
    let mut keys: Vec<String> = pubkeys.iter().map(|p| normalize_pk(p)).collect();
    keys.sort();
    let mut hasher = Sha256::new();
    hasher.update(MULTISIG_DOMAIN_TAG);
    hasher.update([m, keys.len() as u8]);
    for k in &keys {
        hasher.update(k.as_bytes());
    }
    let digest = hasher.finalize();
    format!("{MULTISIG_ADDR_PREFIX}{}", hex::encode(&digest[..20]))
}

/// Valide la STRUCTURE des conditions portées par des outputs en création
/// (côté `TxUtxo` comme côté `Mint`). Erreurs spécifiques (`InvalidSpendCondition`)
/// — c'est de la validation de requête, pas une fuite d'état.
pub fn validate_output_conditions(outputs: &[TxOutput]) -> Result<(), ValidationError> {
    for (i, out) in outputs.iter().enumerate() {
        let Some(cond) = &out.spend_condition else {
            continue;
        };
        match cond {
            SpendCondition::PubKey => {}
            SpendCondition::MultiSig { m, pubkeys } => {
                if *m == 0 {
                    return Err(ValidationError::InvalidSpendCondition {
                        reason: format!("output {i}: MultiSig m must be >= 1"),
                    });
                }
                if pubkeys.is_empty() || pubkeys.len() > MAX_MULTISIG_KEYS {
                    return Err(ValidationError::InvalidSpendCondition {
                        reason: format!(
                            "output {i}: MultiSig requires 1..={MAX_MULTISIG_KEYS} pubkeys, got {}",
                            pubkeys.len()
                        ),
                    });
                }
                if (*m as usize) > pubkeys.len() {
                    return Err(ValidationError::InvalidSpendCondition {
                        reason: format!(
                            "output {i}: MultiSig m={m} exceeds pubkey count {}",
                            pubkeys.len()
                        ),
                    });
                }
                let mut seen = HashSet::new();
                for pk in pubkeys {
                    let norm = normalize_pk(pk);
                    if hex::decode(&norm).is_err() || norm.is_empty() {
                        return Err(ValidationError::InvalidSpendCondition {
                            reason: format!("output {i}: MultiSig pubkey is not valid hex"),
                        });
                    }
                    if !seen.insert(norm) {
                        return Err(ValidationError::InvalidSpendCondition {
                            reason: format!("output {i}: duplicate pubkey in MultiSig set"),
                        });
                    }
                }
                // L'adresse DOIT être l'adresse canonique de la policy —
                // c'est elle qui engage le set de clés on-DAG.
                let expected = multisig_address(*m, pubkeys);
                if out.address != expected {
                    return Err(ValidationError::InvalidSpendCondition {
                        reason: format!(
                            "output {i}: address does not commit to the MultiSig policy \
                             (expected canonical multisig address)"
                        ),
                    });
                }
            }
            SpendCondition::HashLock { hash_hex } => {
                let h = hash_hex.trim();
                if h.len() != 64 || hex::decode(h).is_err() {
                    return Err(ValidationError::InvalidSpendCondition {
                        reason: format!(
                            "output {i}: HashLock hash_hex must be 64 hex chars (SHA-256)"
                        ),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Vérifie qu'un unlock satisfait la condition de l'UTXO dépensé (un input).
///
/// PRÉREQUIS : les signatures de `unlock` (principale + cosigners) ont déjà
/// été vérifiées cryptographiquement par `verify_tx_signatures` sur le message
/// canonique. Ici on vérifie l'AUTORISATION (binding adresse / quorum /
/// préimage), pas la cryptographie.
pub fn check_input_spend_condition(
    input_index: usize,
    utxo: &TxOutput,
    unlock: &Unlock,
) -> Result<(), ValidationError> {
    match &utxo.spend_condition {
        // Défaut historique : binding C-1.
        None | Some(SpendCondition::PubKey) => {
            if !unlock_matches_address(&unlock.pubkey_hex, &utxo.address) {
                tracing::warn!(
                    "🚫 Ownership mismatch: input {input_index} is not owned by unlock pubkey"
                );
                return Err(ValidationError::OwnershipMismatch { input_index });
            }
            Ok(())
        }
        Some(SpendCondition::MultiSig { m, pubkeys }) => {
            // Défense en profondeur : un set dégénéré stocké hors pipeline
            // ne doit jamais devenir anyone-can-spend.
            if *m == 0 || pubkeys.is_empty() {
                tracing::error!(
                    "🚫 Degenerate MultiSig condition on spent UTXO (input {input_index}): m={m}, n={}",
                    pubkeys.len()
                );
                return Err(ValidationError::SpendConditionNotMet { input_index });
            }
            let allowed: HashSet<String> = pubkeys.iter().map(|p| normalize_pk(p)).collect();
            // Pubkeys distinctes ∈ set ayant signé : principale + cosigners.
            // Les doublons ne comptent qu'une fois (anti quorum-stuffing).
            let mut signers: HashSet<String> = HashSet::new();
            let primary = normalize_pk(&unlock.pubkey_hex);
            if allowed.contains(&primary) {
                signers.insert(primary);
            }
            for co in &unlock.cosigners {
                let pk = normalize_pk(&co.pubkey_hex);
                if allowed.contains(&pk) {
                    signers.insert(pk);
                }
            }
            if signers.len() < *m as usize {
                tracing::warn!(
                    "🚫 MultiSig quorum not met on input {input_index}: {}/{} valid signers",
                    signers.len(),
                    m
                );
                return Err(ValidationError::SpendConditionNotMet { input_index });
            }
            Ok(())
        }
        Some(SpendCondition::HashLock { hash_hex }) => {
            let Some(preimage_hex) = &unlock.preimage_hex else {
                tracing::warn!("🚫 HashLock input {input_index}: missing preimage");
                return Err(ValidationError::SpendConditionNotMet { input_index });
            };
            let Ok(preimage) = hex::decode(preimage_hex.trim()) else {
                tracing::warn!("🚫 HashLock input {input_index}: preimage is not valid hex");
                return Err(ValidationError::SpendConditionNotMet { input_index });
            };
            let digest = hex::encode(Sha256::digest(&preimage));
            if !digest.eq_ignore_ascii_case(hash_hex.trim()) {
                tracing::warn!("🚫 HashLock input {input_index}: preimage hash mismatch");
                return Err(ValidationError::SpendConditionNotMet { input_index });
            }
            Ok(())
        }
    }
}

/// Vérifie l'autorisation de dépense de TOUS les inputs (C-1 généralisé).
///
/// `input_outputs` = les `TxOutput` dépensés, dans l'ordre des inputs
/// (appariement positionnel input\[i\] ↔ unlock\[i\] déjà vérifié en amont).
pub fn check_spend_authorization(
    unlocks: &[Unlock],
    input_outputs: &[TxOutput],
) -> Result<(), ValidationError> {
    for (i, out) in input_outputs.iter().enumerate() {
        check_input_spend_condition(i, out, &unlocks[i])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multisig_address_is_order_and_case_insensitive() {
        let a = multisig_address(2, &["04AABB".into(), "04ccdd".into(), "04eeff".into()]);
        let b = multisig_address(2, &["0x04eeff".into(), "04aabb".into(), "04CCDD".into()]);
        println!("a={a} b={b}");
        assert_eq!(a, b);
        assert!(a.starts_with(MULTISIG_ADDR_PREFIX));
    }

    #[test]
    fn multisig_address_commits_to_m_and_keys() {
        let base = multisig_address(2, &["04aa".into(), "04bb".into()]);
        let other_m = multisig_address(1, &["04aa".into(), "04bb".into()]);
        let other_keys = multisig_address(2, &["04aa".into(), "04cc".into()]);
        println!("base={base} other_m={other_m} other_keys={other_keys}");
        assert_ne!(base, other_m, "m doit changer l'adresse");
        assert_ne!(base, other_keys, "le set de clés doit changer l'adresse");
    }
}
