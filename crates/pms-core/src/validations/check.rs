//! Validation d’un bloc : un SEUL point d’entrée qui orchestre
//! de PETITES fonctions spécialisées, rapides, pures et testables.

use crate::Dag;
use crate::validations::amount::{amounts_positive_outputs, tx_amounts_valid};
use crate::validations::fees::validate_fee_recipient_output;
use crate::validations::parents::{no_cycle, parent_count, parents_exist};
use crate::validations::signature::verify_tx_signatures;
use crate::validations::transactions::{utxo_no_double_spend, utxo_sufficient_funds};
use pms_config::{ValidationSettings, load_config};
use pms_errors::ValidationError;
use pms_types::{Block, PayloadEnvelope, PlainPayload};
use rust_decimal::Decimal;

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
        }
    }

    /// Optionnel si tu veux une helper globale
    pub fn from_global_config() -> Self {
        let settings = load_config().expect("config invalide");
        Self::from_settings(&settings.validation)
    }
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
    if policy.enforce_parent_existence {
        parents_exist(dag, b)?;
    }
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
                utxo_no_double_spend(dag, tx)?;
                utxo_sufficient_funds(dag, tx)?;
            }
            PlainPayload::Milestone { approved: _ } => {
                // MVP: rien de spécial ici
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
