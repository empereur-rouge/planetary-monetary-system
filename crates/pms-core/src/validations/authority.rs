//! Autorité par type de payload — qui a le droit de produire quel bloc.
//!
//! Extension de l'audit C-2 (v0.9.0) : les checks coordinator-only existaient
//! dans le `validate_block` legacy mais ce dernier a été retiré du hot path de
//! production. Résultat : `ConfigUpdate`, `Freeze`, `Seize`, `Reward`,
//! `TokenCreate`, `Bridge*`, `Contract*`… étaient appliqués par
//! `do_persist_block_internal` sans aucune vérification du signataire — seule
//! l'enforcement single-writer (couche bloc) les protégeait.
//!
//! Ce module centralise ces règles dans [`validate_payload_authority`],
//! appelée par les DEUX chemins (hot path `persist.rs` + legacy `check.rs`)
//! pour qu'elles ne puissent plus diverger.

use crate::validations::check::ValidatePolicy;
use pms_errors::ValidationError;
use pms_types::{PayloadEnvelope, PlainPayload};

/// Vérifie que `signer_pk` est le Coordinator attendu par la policy.
///
/// Si `policy.coordinator_public_key` est `None` (mode Dev pur, aucune clé
/// configurée ni hardcodée), l'enforcement est sauté avec un warn — en
/// Testnet/Mainnet la clé est toujours `Some` (constantes hardcodées dans
/// `pms-config`), donc ce chemin fail-open n'existe pas en production.
fn require_coordinator(
    signer_pk: Option<&str>,
    policy: &ValidatePolicy,
    action_name: &str,
) -> Result<(), ValidationError> {
    let Some(coord_pk) = &policy.coordinator_public_key else {
        tracing::warn!(
            "{action_name}: no coordinator_public_key configured (dev mode) — authority check skipped"
        );
        return Ok(());
    };
    match signer_pk {
        Some(spk) if spk.eq_ignore_ascii_case(coord_pk) => Ok(()),
        Some(spk) => Err(ValidationError::InvalidSignature(format!(
            "{action_name} signed by unauthorized key: {spk}. Expected Coordinator"
        ))),
        None => Err(ValidationError::InvalidSignature(format!(
            "{action_name} block must be signed by Coordinator"
        ))),
    }
}

/// Valide l'AUTORITÉ et la structure minimale d'un payload sensible.
///
/// Couvre tous les types de payload réservés au Coordinator :
/// `Milestone`, `ConfigUpdate`, `Reward`, `EncryptedReward`, `TokenCreate`,
/// `BridgeLock`, `BridgeMint`, `Freeze`, `Unfreeze`, `Seize`, `Reverse`,
/// `ContractRegister`, `ContractUpdate`, `LedgerOwnershipTransfer`,
/// `CoordinatorKeyRotate`.
///
/// Les payloads à validation dédiée (`TxUtxo` → `validate_transaction_full`,
/// `Mint` → `validate_mint_security`, `Nft` → `validate_nft_action`,
/// `Genesis`) ne sont PAS traités ici. Les payloads chiffrés sont opaques —
/// validés à la structure seulement, en amont.
///
/// `policy.coordinator_public_key` doit refléter l'autorité COURANTE
/// (rotation incluse) côté hot path.
pub fn validate_payload_authority(
    signer_pk: Option<&str>,
    payload: Option<&PayloadEnvelope>,
    policy: &ValidatePolicy,
) -> Result<(), ValidationError> {
    let Some(PayloadEnvelope::Plain(pp)) = payload else {
        return Ok(()); // None ou Encrypted : rien à vérifier ici.
    };

    match pp {
        PlainPayload::Milestone { .. } => require_coordinator(signer_pk, policy, "Milestone"),
        PlainPayload::ConfigUpdate(_) => require_coordinator(signer_pk, policy, "ConfigUpdate"),
        PlainPayload::Reward { .. } => require_coordinator(signer_pk, policy, "Reward"),
        PlainPayload::EncryptedReward { .. } => {
            require_coordinator(signer_pk, policy, "EncryptedReward")
        }
        PlainPayload::TokenCreate(_) => require_coordinator(signer_pk, policy, "TokenCreate"),
        PlainPayload::BridgeLock {
            inputs,
            dest_ledger_id,
            dest_address,
            ..
        } => {
            require_coordinator(signer_pk, policy, "BridgeLock")?;
            if inputs.is_empty() {
                return Err(ValidationError::Other(
                    "BridgeLock: at least one input required",
                ));
            }
            if dest_ledger_id.is_empty() || dest_address.is_empty() {
                return Err(ValidationError::Other(
                    "BridgeLock: dest_ledger_id and dest_address required",
                ));
            }
            Ok(())
        }
        PlainPayload::BridgeMint {
            outputs,
            lock_block_id,
            source_ledger_id,
        } => {
            require_coordinator(signer_pk, policy, "BridgeMint")?;
            if outputs.is_empty() {
                return Err(ValidationError::Other(
                    "BridgeMint: at least one output required",
                ));
            }
            if lock_block_id.is_empty() || source_ledger_id.is_empty() {
                return Err(ValidationError::Other(
                    "BridgeMint: lock_block_id and source_ledger_id required",
                ));
            }
            Ok(())
        }
        PlainPayload::Freeze { address, .. } => {
            require_coordinator(signer_pk, policy, "Freeze")?;
            if address.trim().is_empty() {
                return Err(ValidationError::Other("Freeze: address cannot be empty"));
            }
            Ok(())
        }
        PlainPayload::Unfreeze {
            address,
            freeze_block_id,
            ..
        } => {
            require_coordinator(signer_pk, policy, "Unfreeze")?;
            if address.trim().is_empty() || freeze_block_id.trim().is_empty() {
                return Err(ValidationError::Other(
                    "Unfreeze: address and freeze_block_id required",
                ));
            }
            Ok(())
        }
        PlainPayload::Seize {
            inputs,
            outputs,
            from_address,
            ..
        } => {
            require_coordinator(signer_pk, policy, "Seize")?;
            if inputs.is_empty() || outputs.is_empty() {
                return Err(ValidationError::Other("Seize: inputs and outputs required"));
            }
            if from_address.trim().is_empty() {
                return Err(ValidationError::Other(
                    "Seize: from_address cannot be empty",
                ));
            }
            Ok(())
        }
        PlainPayload::Reverse {
            original_block_id,
            inputs,
            outputs,
            ..
        } => {
            require_coordinator(signer_pk, policy, "Reverse")?;
            if original_block_id.trim().is_empty() || inputs.is_empty() || outputs.is_empty() {
                return Err(ValidationError::Other(
                    "Reverse: original_block_id, inputs and outputs required",
                ));
            }
            Ok(())
        }
        PlainPayload::ContractRegister(contract) => {
            require_coordinator(signer_pk, policy, "ContractRegister")?;
            if contract.contract_id.trim().is_empty() {
                return Err(ValidationError::Other(
                    "ContractRegister: contract_id cannot be empty",
                ));
            }
            if contract.name.trim().is_empty() {
                return Err(ValidationError::Other(
                    "ContractRegister: name cannot be empty",
                ));
            }
            if contract.actions.is_empty() {
                return Err(ValidationError::Other(
                    "ContractRegister: at least one action required",
                ));
            }
            Ok(())
        }
        PlainPayload::ContractUpdate { contract_id, .. } => {
            require_coordinator(signer_pk, policy, "ContractUpdate")?;
            if contract_id.trim().is_empty() {
                return Err(ValidationError::Other(
                    "ContractUpdate: contract_id cannot be empty",
                ));
            }
            Ok(())
        }
        PlainPayload::LedgerOwnershipTransfer { ledger_id, .. } => {
            require_coordinator(signer_pk, policy, "LedgerOwnershipTransfer")?;
            if ledger_id.trim().is_empty() {
                return Err(ValidationError::Other(
                    "LedgerOwnershipTransfer: ledger_id cannot be empty",
                ));
            }
            Ok(())
        }
        PlainPayload::ReserveSnapshot {
            state_root,
            total_supply,
            ..
        } => {
            require_coordinator(signer_pk, policy, "ReserveSnapshot")?;
            let root = state_root.trim();
            if root.len() != 64 || hex::decode(root).is_err() {
                return Err(ValidationError::Other(
                    "ReserveSnapshot: state_root must be 64 hex chars (SHA-256)",
                ));
            }
            for (_asset, amount) in total_supply {
                if rust_decimal::Decimal::from_str_exact(amount).is_err() {
                    return Err(ValidationError::Other(
                        "ReserveSnapshot: total_supply amounts must be decimals",
                    ));
                }
            }
            Ok(())
        }
        PlainPayload::CoordinatorKeyRotate { old_pk, new_pk, .. } => {
            // Le hot path applique en plus une règle plus stricte (signé par
            // la clé COURANTE + old_pk == courante) dans son handler dédié.
            require_coordinator(signer_pk, policy, "CoordinatorKeyRotate")?;
            if old_pk.trim().is_empty() || new_pk.trim().is_empty() {
                return Err(ValidationError::Other(
                    "CoordinatorKeyRotate: old_pk and new_pk must be non-empty",
                ));
            }
            if old_pk.trim() == new_pk.trim() {
                return Err(ValidationError::Other(
                    "CoordinatorKeyRotate: old_pk and new_pk must differ",
                ));
            }
            Ok(())
        }
        // Payloads à validation dédiée ailleurs (hot path + legacy).
        // TokenBurn est owner-signé (l'utilisateur brûle ses propres fonds) :
        // pas coordinator-only. Son autorisation (unlocks des inputs) +
        // conservation-burn sont vérifiées par `validate_token_burn_async` dans
        // le hot path, comme TxUtxo via `validate_transaction_full`.
        PlainPayload::Genesis
        | PlainPayload::Mint { .. }
        | PlainPayload::TxUtxo(_)
        | PlainPayload::TokenBurn { .. }
        | PlainPayload::Nft(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pms_types::PlainPayload;

    fn policy_with_coord(pk: Option<&str>) -> ValidatePolicy {
        let mut p = ValidatePolicy::default();
        p.coordinator_public_key = pk.map(Into::into);
        p
    }

    fn freeze_payload() -> PayloadEnvelope {
        PayloadEnvelope::Plain(PlainPayload::Freeze {
            address: "8e1someaddr".into(),
            reason: "test".into(),
        })
    }

    #[test]
    fn coordinator_signed_freeze_accepted() {
        let policy = policy_with_coord(Some("04coordkey"));
        let r = validate_payload_authority(Some("04coordkey"), Some(&freeze_payload()), &policy);
        println!("coordinator freeze: {r:?}");
        assert!(r.is_ok());
    }

    #[test]
    fn foreign_signed_freeze_rejected() {
        let policy = policy_with_coord(Some("04coordkey"));
        let r = validate_payload_authority(Some("04attacker"), Some(&freeze_payload()), &policy);
        println!("attacker freeze: {r:?}");
        let err = format!("{:?}", r.expect_err("must reject"));
        assert!(err.contains("InvalidSignature"), "got: {err}");
    }

    #[test]
    fn unsigned_config_update_rejected() {
        let policy = policy_with_coord(Some("04coordkey"));
        let payload = PayloadEnvelope::Plain(PlainPayload::ConfigUpdate(
            pms_config::ConfigUpdate::SetFeeRate { bps: 100 },
        ));
        let r = validate_payload_authority(None, Some(&payload), &policy);
        println!("unsigned config update: {r:?}");
        assert!(r.is_err());
    }

    #[test]
    fn txutxo_not_gated_here() {
        // TxUtxo a sa propre validation (validate_transaction_full) — pas
        // d'autorité coordinator requise.
        let policy = policy_with_coord(Some("04coordkey"));
        let payload = PayloadEnvelope::Plain(PlainPayload::TxUtxo(pms_types::Transaction {
            inputs: vec![],
            outputs: vec![],
            fee: "0".into(),
            unlocks: vec![],
        }));
        let r = validate_payload_authority(Some("04anyone"), Some(&payload), &policy);
        println!("txutxo authority: {r:?}");
        assert!(r.is_ok());
    }
}
