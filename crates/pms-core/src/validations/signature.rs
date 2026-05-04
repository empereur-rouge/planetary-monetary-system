use k256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use pms_errors::ValidationError;
use pms_types::Transaction;
use rayon::prelude::*;

/// Vérifie toutes les signatures d'une transaction en PARALLÈLE.
/// Utilise rayon pour distribuer la vérification sur tous les cœurs CPU.
///
/// `network_id` doit être le `network_id` de la chaîne courante. La signature
/// est calculée sur un message qui inclut `network_id` — toute TX signée pour
/// un autre réseau (testnet vs mainnet) sera rejetée ici (cross-chain replay
/// protection).
pub fn verify_tx_signatures(tx: &Transaction, network_id: &str) -> Result<(), ValidationError> {
    if tx.inputs.len() != tx.unlocks.len() {
        return Err(ValidationError::InvalidSignature(
            "inputs/unlocks mismatch".into(),
        ));
    }

    // 1. Compute signing message once (shared across all verifications)
    let msg_hex = tx
        .signing_message(network_id)
        .map_err(|_| ValidationError::Other("Serialization in signing_message failed"))?;
    let msg_bytes = msg_hex.as_bytes();

    // 2. Hybrid verification strategy
    // Parallelism has overhead. For small transaction (1-3 inputs), sequential is faster.
    // Benchmark showed 4400 TPS (seq) vs 3300 TPS (par) for 1-input txs.
    if tx.unlocks.len() < 4 {
        for (i, unlock) in tx.unlocks.iter().enumerate() {
            verify_single_signature(msg_bytes, unlock, i)?;
        }
    } else {
        tx.unlocks
            .par_iter()
            .enumerate()
            .try_for_each(|(i, unlock)| verify_single_signature(msg_bytes, unlock, i))?;
    }

    Ok(())
}

/// Vérifie une seule signature (appelée en parallèle par rayon).
#[inline]
fn verify_single_signature(
    msg_bytes: &[u8],
    unlock: &pms_types::Unlock,
    index: usize,
) -> Result<(), ValidationError> {
    // Decode pubkey from hex
    let vk_bytes = hex::decode(&unlock.pubkey_hex)
        .map_err(|_| ValidationError::InvalidSignature("invalid pubkey hex".into()))?;
    let vk = VerifyingKey::from_sec1_bytes(&vk_bytes)
        .map_err(|_| ValidationError::InvalidSignature("invalid sec1 pubkey".into()))?;

    // Decode signature from base64
    use base64::prelude::*;
    let sig_bytes = BASE64_STANDARD
        .decode(&unlock.signature_b64)
        .map_err(|_| ValidationError::InvalidSignature("invalid b64 signature".into()))?;

    // Parse k256 Signature
    let signature = Signature::from_der(&sig_bytes)
        .map_err(|_| ValidationError::InvalidSignature("invalid der signature".into()))?;

    // Verify ECDSA signature
    vk.verify(msg_bytes, &signature).map_err(|_| {
        ValidationError::InvalidSignature(format!("signature mismatch input {}", index))
    })?;

    Ok(())
}
