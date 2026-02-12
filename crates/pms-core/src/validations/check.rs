//! Validation d’un bloc : un SEUL point d’entrée qui orchestre
//! de PETITES fonctions spécialisées, rapides, pures et testables.

use crate::Dag;
use crate::validations::amount::{amounts_positive_outputs, tx_amounts_valid};
use crate::validations::fees::validate_fee_recipient_output;
use crate::validations::parents::{no_cycle, parent_count};
use crate::validations::signature::verify_tx_signatures;
use crate::validations::transactions::{utxo_no_double_spend, utxo_sufficient_funds};
use k256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use pms_config::{ValidationSettings, load_config};
use pms_errors::ValidationError;
use pms_types::{Block, PayloadEnvelope, PlainPayload};
use rust_decimal::Decimal;
use std::str::FromStr;

/// Paramètres “policy” (faciles à tester/faire évoluer).
#[derive(Debug, Clone)]
pub struct ValidatePolicy {
    /// Taille maximale (octets) du payload sérialisé (anti-spam).
    pub max_payload_bytes: usize,
    /// Nombre minimal de parents quand le DAG est amorcé.
    pub min_parents_after_boot: usize, // typiquement 2
    // 👇 nouveaux quotas
    pub max_parents: usize,           // ex: 8
    pub require_unique_parents: bool, // ex: true
    pub forbid_self_parent: bool,     // ex: true

    // Quotas TX UTXO (si Plain::TxUtxo)
    pub max_inputs: usize,   // ex: 64
    pub max_outputs: usize,  // ex: 64
    pub max_tx_bytes: usize, // ex: 64*1024
    pub min_pow_leading_zero_bits: u8,
    pub max_fee_per_tx: Decimal,
    pub enforce_parent_existence: bool,

    pub enforce_fee_recipient: bool,
    pub allowed_fee_addresses: Vec<String>,

    // Optimisation Phase 4: Si true, on valide les UTXOs en amont (async)
    pub skip_utxo_checks: bool,

    // Split Fee Settings
    pub platform_address: Option<String>,
    pub platform_fee_ratio: Decimal,
    pub coordinator_public_key: Option<String>,
    pub enforce_single_writer: bool,
}

impl Default for ValidatePolicy {
    fn default() -> Self {
        Self {
            max_payload_bytes: 64 * 1024,
            min_parents_after_boot: 2,
            max_parents: 8,
            require_unique_parents: true,
            forbid_self_parent: true,
            max_inputs: 64,
            max_outputs: 64,
            max_tx_bytes: 64 * 1024,
            min_pow_leading_zero_bits: 0,
            max_fee_per_tx: Decimal::from(1000),
            enforce_parent_existence: true,
            enforce_fee_recipient: false,
            allowed_fee_addresses: vec![],
            skip_utxo_checks: false,
            platform_address: None,
            platform_fee_ratio: Decimal::ZERO,
            coordinator_public_key: None,
            enforce_single_writer: true,
        }
    }
}

impl ValidatePolicy {
    pub fn from_settings(v: &ValidationSettings) -> Self {
        Self {
            max_payload_bytes: v.max_payload_bytes,
            min_parents_after_boot: v.min_parents_after_boot,
            max_parents: v.max_parents,
            require_unique_parents: v.require_unique_parents,
            forbid_self_parent: v.forbid_self_parent,
            max_inputs: v.max_inputs,
            max_outputs: v.max_outputs,
            max_tx_bytes: v.max_tx_bytes,
            min_pow_leading_zero_bits: v.min_pow_leading_zero_bits,
            max_fee_per_tx: Decimal::from(v.max_fee_per_tx),
            enforce_parent_existence: v.enforce_parent_existence,
            enforce_fee_recipient: v.enforce_fee_recipient,
            allowed_fee_addresses: v.allowed_fee_addresses.clone(),
            skip_utxo_checks: false,
            platform_address: None,
            platform_fee_ratio: Decimal::ZERO,
            coordinator_public_key: v.coordinator_public_key.clone(),
            enforce_single_writer: v.enforce_single_writer,
        }
    }

    /// Version avec gestion d'erreur pour les problèmes de configuration de sécurité.
    /// Préférer cette version dans le code de production pour une meilleure gestion des erreurs.
    pub fn try_from_global_config() -> Result<Self, ValidationError> {
        let settings = load_config()
            .map_err(|e| ValidationError::ConfigError(format!("failed to load config: {e}")))?;
        let mut p = Self::from_settings(&settings.validation);

        // Logic for Platform Address Security via Signed Config
        // We use the same Coordinator Key (Master Key) for config signing
        let target_master_key = match settings.network.mode {
            pms_config::NetworkMode::Mainnet => Some(pms_consensus::COORDINATOR_PUBLIC_KEY_MAINNET),
            pms_config::NetworkMode::Testnet => Some(pms_consensus::COORDINATOR_PUBLIC_KEY_TESTNET),
            pms_config::NetworkMode::Dev => None, // Dev mode = no signature required
        };

        if let Some(master_pk_hex) = target_master_key {
            // PRODUCTION (Mainnet/Testnet) ENFORCEMENT
            if let Some(addr) = &settings.fees.platform_address {
                if let Some(sig_hex) = &settings.fees.platform_address_signature {
                    // Verify Signature
                    if verify_config_signature(addr, sig_hex, master_pk_hex) {
                        p.platform_address = Some(addr.clone());
                    } else {
                        return Err(ValidationError::InvalidPlatformSignature {
                            address: addr.clone(),
                        });
                    }
                } else {
                    return Err(ValidationError::MissingPlatformSignature);
                }
            } else {
                tracing::warn!("No platform_address configured. Fee splitting will be disabled.");
                p.platform_address = None;
            }
        } else {
            // DEV MODE: Allow whatever in config
            p.platform_address = settings.fees.platform_address.clone();
        }

        // Coordinator / Milestone Enforcement
        // Config override takes priority (for testing flexibility)
        // Otherwise, use hardcoded values based on network mode
        if let Some(ref custom_key) = settings.validation.coordinator_public_key {
            // ═══════════════════════════════════════════════════════════════
            // SÉCURITÉ - Empêcher l'utilisation des clés Prod en Dev
            // ═══════════════════════════════════════════════════════════════
            // En mode Dev, on ne doit JAMAIS utiliser les clés Mainnet/Testnet.
            // Cela évite une confusion accidentelle ou une tentative de fraude
            // où quelqu'un utiliserait le mode Dev (moins de validations) avec
            // des clés de production.
            if settings.network.mode == pms_config::NetworkMode::Dev {
                let is_mainnet_key = custom_key == pms_consensus::COORDINATOR_PUBLIC_KEY_MAINNET;
                let is_testnet_key = custom_key == pms_consensus::COORDINATOR_PUBLIC_KEY_TESTNET;

                if is_mainnet_key || is_testnet_key {
                    return Err(ValidationError::ProdKeyInDevMode {
                        network: if is_mainnet_key {
                            "MAINNET".to_string()
                        } else {
                            "TESTNET".to_string()
                        },
                    });
                }
            }

            // Config specifies a custom coordinator key (useful for tests)
            p.coordinator_public_key = Some(custom_key.clone());
        } else {
            // Use hardcoded keys based on network mode
            match settings.network.mode {
                pms_config::NetworkMode::Mainnet => {
                    p.coordinator_public_key =
                        Some(pms_consensus::COORDINATOR_PUBLIC_KEY_MAINNET.to_string());
                }
                pms_config::NetworkMode::Testnet => {
                    p.coordinator_public_key =
                        Some(pms_consensus::COORDINATOR_PUBLIC_KEY_TESTNET.to_string());
                }
                pms_config::NetworkMode::Dev => {
                    // Dev mode without explicit key = no coordinator check
                    p.coordinator_public_key = None;
                }
            }
        }

        p.platform_fee_ratio =
            Decimal::from_str(&settings.fees.platform_fee_ratio).unwrap_or(Decimal::ZERO);
        Ok(p)
    }

    /// Helper globale qui panic en cas d'erreur de configuration de sécurité.
    /// Pour une meilleure gestion des erreurs, utilisez `try_from_global_config()`.
    pub fn from_global_config() -> Self {
        match Self::try_from_global_config() {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("🚨 SECURITY CONFIG ERROR: {e}");
                panic!("Critical security configuration error: {e}");
            }
        }
    }

    /// Met à jour la policy avec les valeurs de RuntimeConfig (Hot-Swap).
    ///
    /// Cette méthode permet d'appliquer dynamiquement les paramètres
    /// modifiés via `PlainPayload::ConfigUpdate`.
    pub fn update_from_runtime_config(&mut self, config: &pms_config::RuntimeConfig) {
        // Convertir basis points en Decimal ratio
        // coordinator_fee_bps: 6700 = 67% = 0.67
        self.platform_fee_ratio = Decimal::from(config.coordinator_fee_bps) / Decimal::from(10000);

        // PoW minimum bits
        self.min_pow_leading_zero_bits = config.min_pow_bits;

        // Note: fee_rate_bps et max_mint_per_block seraient utilisés ailleurs
        // si on implémente la validation des montants de fee
    }
}

// Helper for verifying Secp256k1 signature of the address string
fn verify_config_signature(address: &str, sig_hex: &str, pk_hex: &str) -> bool {
    let Ok(pk_bytes) = hex::decode(pk_hex) else {
        return false;
    };
    let Ok(verify_key) = VerifyingKey::from_sec1_bytes(&pk_bytes) else {
        return false;
    };

    let Ok(sig_bytes) = hex::decode(sig_hex) else {
        return false;
    };
    let Ok(sig) = Signature::from_der(&sig_bytes).or_else(|_| Signature::from_slice(&sig_bytes))
    else {
        return false;
    };

    verify_key.verify(address.as_bytes(), &sig).is_ok()
}

/// Point d’entrée UNIQUE.
/// - Ordonne du moins cher → plus cher.
/// - Court-circuite dès qu’une règle échoue.
/// - Délègue le “métier” à de petites fonctions pures.
pub fn validate_block(
    dag: &Dag,
    b: &Block,
    policy: &ValidatePolicy,
) -> Result<(), ValidationError> {
    // 1) Anti-spam léger
    payload_size_limit(b, policy)?;

    // 2) Structure rapide
    //
    // NOTE: La vérification parents_exist() a été DÉPLACÉE dans net_adapter.rs
    // où elle s'appelle parents_exist_in_store() et vérifie contre RocksDB.
    //
    // Cela corrige la race condition où les tips viennent de RocksDB mais
    // la validation se faisait contre le DAG RAM (qui peut ne pas contenir
    // tous les blocs récemment insérés).
    //
    // La vérification RAM ici est SUPPRIMÉE pour éviter les faux rejets
    // sous charge parallèle.
    no_cycle(b, policy)?;
    parent_count(dag, b, policy)?;

    // 3) Sémantique par type de payload (si visible)
    match &b.payload {
        None => { /* MVP: bloc sans payload = OK si structure ok */ }
        Some(PayloadEnvelope::Plain(pp)) => match pp {
            PlainPayload::Genesis => genesis_rules(dag, b)?,
            PlainPayload::Mint { outputs } => {
                amounts_positive_outputs(outputs)?;
                // (MVP) : autres règles mint ici si besoin
            }
            PlainPayload::TxUtxo(tx) => {
                // ⚠️ Ces fonctions sont des *hooks* : implémenter dans pms-ledger/pms-crypto plus tard.
                verify_tx_signatures(tx)?; // TODO (MVP rapide: stub “Ok(())”)
                validate_fee_recipient_output(tx, policy)?;
                tx_amounts_valid(tx, policy)?; // basique sur chaînes décimales

                // CHECK UTXO (sauf si fait en async par net_adapter)
                if !policy.skip_utxo_checks {
                    utxo_no_double_spend(dag, tx)?;
                    utxo_sufficient_funds(dag, tx)?;
                }
            }
            PlainPayload::Milestone {
                approved: _,
                distribute_node_rewards: _,
            } => {
                // Milestone Validation
                if let Some(coord_pk) = &policy.coordinator_public_key {
                    if let Some(spk) = &b.signer_pk {
                        if spk != coord_pk {
                            return Err(ValidationError::InvalidSignature(format!(
                                "Milestone signed by unauthorized key: {}. Expected: {}",
                                spk, coord_pk
                            )));
                        }
                    } else {
                        return Err(ValidationError::InvalidSignature(
                            "Milestone block must be signed".into(),
                        ));
                    }
                } else {
                    return Err(ValidationError::Other(
                        "Milestones not enabled (no coordinator_public_key)",
                    ));
                }
            }
            PlainPayload::Nft(_action) => {
                // ═══════════════════════════════════════════════════════════
                // NFT VALIDATION : Vérification basique dans le flow sync
                // ═══════════════════════════════════════════════════════════
                //
                // La validation complète (ownership, existence via store) est
                // effectuée dans net_adapter.rs lors du persist_block, où on
                // a accès au NftStorage (RocksDB).
                //
                // Ici on ne fait que les checks basiques sans accès au store :
                // - Le bloc doit être signé (toute action NFT requiert une signature)
                //
                // Voir chapitre 4.2 du Rust Book : Références et Emprunt
                // pour comprendre pourquoi on ne peut pas facilement passer
                // un trait object `&dyn NftStorage` ici.

                if b.signer_pk.is_none() {
                    return Err(ValidationError::InvalidSignature(
                        "NFT action block must be signed".into(),
                    ));
                }
            }
            PlainPayload::ConfigUpdate(_update) => {
                // ConfigUpdate : seulement le Coordinator peut modifier la config
                if let Some(coord_pk) = &policy.coordinator_public_key {
                    if let Some(spk) = &b.signer_pk {
                        if spk != coord_pk {
                            return Err(ValidationError::InvalidSignature(format!(
                                "ConfigUpdate signed by unauthorized key: {}. Expected: {}",
                                spk, coord_pk
                            )));
                        }
                    } else {
                        return Err(ValidationError::InvalidSignature(
                            "ConfigUpdate block must be signed".into(),
                        ));
                    }
                } else {
                    return Err(ValidationError::Other(
                        "ConfigUpdate not enabled (no coordinator_public_key)",
                    ));
                }
            }
            PlainPayload::Reward { .. } => {
                // SECURITY: Only Coordinator can create Reward blocks
                // This prevents malicious nodes from minting tokens via fake rewards
                if let Some(coord_pk) = &policy.coordinator_public_key {
                    if let Some(spk) = &b.signer_pk {
                        if spk != coord_pk {
                            return Err(ValidationError::InvalidSignature(format!(
                                "Reward signed by unauthorized key: {}. Expected Coordinator: {}",
                                spk, coord_pk
                            )));
                        }
                    } else {
                        return Err(ValidationError::InvalidSignature(
                            "Reward block must be signed by Coordinator".into(),
                        ));
                    }
                } else {
                    return Err(ValidationError::Other(
                        "Reward not enabled (no coordinator_public_key configured)",
                    ));
                }
            }
            PlainPayload::EncryptedReward { .. } => {
                // SECURITY: Same rules as Reward - only Coordinator can create
                // EncryptedReward contains encrypted outputs for privacy
                if let Some(coord_pk) = &policy.coordinator_public_key {
                    if let Some(spk) = &b.signer_pk {
                        if spk != coord_pk {
                            return Err(ValidationError::InvalidSignature(format!(
                                "EncryptedReward signed by unauthorized key: {}. Expected Coordinator: {}",
                                spk, coord_pk
                            )));
                        }
                    } else {
                        return Err(ValidationError::InvalidSignature(
                            "EncryptedReward block must be signed by Coordinator".into(),
                        ));
                    }
                } else {
                    return Err(ValidationError::Other(
                        "EncryptedReward not enabled (no coordinator_public_key configured)",
                    ));
                }
            }
            PlainPayload::TokenCreate(_) => {
                // SECURITY: Only Coordinator can register new tokens
                if let Some(coord_pk) = &policy.coordinator_public_key {
                    if let Some(spk) = &b.signer_pk {
                        if spk != coord_pk {
                            return Err(ValidationError::InvalidSignature(format!(
                                "TokenCreate signed by unauthorized key: {}. Expected Coordinator: {}",
                                spk, coord_pk
                            )));
                        }
                    } else {
                        return Err(ValidationError::InvalidSignature(
                            "TokenCreate block must be signed by Coordinator".into(),
                        ));
                    }
                } else {
                    return Err(ValidationError::Other(
                        "TokenCreate not enabled (no coordinator_public_key configured)",
                    ));
                }
            }
        },
        Some(PayloadEnvelope::Encrypted(_ep)) => {
            // MVP privé : on ne peut pas valider le contenu → on se limite à la structure.
            // Plus tard : validations « header-only » (quota, taille, destinataires cachés, etc.).
        }
    }

    Ok(())
}

/// Limite simple sur la taille de payload (anti-flood).
fn payload_size_limit(b: &Block, policy: &ValidatePolicy) -> Result<(), ValidationError> {
    if let Some(pe) = &b.payload {
        let est = serde_json::to_vec(pe)
            .map(|v| v.len())
            .unwrap_or(policy.max_payload_bytes + 1);
        if est > policy.max_payload_bytes {
            return Err(ValidationError::PayloadTooLarge);
        }
    }
    Ok(())
}

/// Règles spécifiques genesis (déjà couvertes dans `parent_count`, doublon minimal pour clarté).
fn genesis_rules(_dag: &Dag, b: &Block) -> Result<(), ValidationError> {
    if !b.parents.is_empty() {
        return Err(ValidationError::InvalidGenesis(
            "genesis sans parents".into(),
        ));
    }
    Ok(())
}
