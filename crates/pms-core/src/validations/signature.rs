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

    // Fast path : 1 seul unlock sans cosigners (cas dominant) — zéro allocation.
    if tx.unlocks.len() == 1 && tx.unlocks[0].cosigners.is_empty() {
        return verify_single_signature(
            msg_bytes,
            &tx.unlocks[0].pubkey_hex,
            &tx.unlocks[0].signature_b64,
            0,
        );
    }

    // 2. Dedupe identical (pubkey, signature) pairs before the expensive
    // ECDSA verify. Couvre la signature principale ET les cosignatures
    // MultiSig (protocole 2.2) — chaque cosignature porte sur le même
    // message canonique et DOIT être cryptographiquement valide, sinon un
    // attaquant remplirait le quorum avec des signatures bidon.
    let mut seen = std::collections::HashSet::new();
    let mut unique_sigs: Vec<(usize, &str, &str)> = Vec::with_capacity(tx.unlocks.len());
    for (i, u) in tx.unlocks.iter().enumerate() {
        if seen.insert((u.pubkey_hex.as_str(), u.signature_b64.as_str())) {
            unique_sigs.push((i, &u.pubkey_hex, &u.signature_b64));
        }
        for co in &u.cosigners {
            if seen.insert((co.pubkey_hex.as_str(), co.signature_b64.as_str())) {
                unique_sigs.push((i, &co.pubkey_hex, &co.signature_b64));
            }
        }
    }

    // 3. Hybrid verification strategy
    // Parallelism has overhead. For small transaction (1-3 inputs), sequential is faster.
    // Benchmark showed 4400 TPS (seq) vs 3300 TPS (par) for 1-input txs.
    if unique_sigs.len() < 4 {
        for (i, pk, sig) in &unique_sigs {
            verify_single_signature(msg_bytes, pk, sig, *i)?;
        }
    } else {
        unique_sigs
            .par_iter()
            .try_for_each(|(i, pk, sig)| verify_single_signature(msg_bytes, pk, sig, *i))?;
    }

    Ok(())
}

/// Vérifie une seule signature ECDSA (appelée en parallèle par rayon).
#[inline]
fn verify_single_signature(
    msg_bytes: &[u8],
    pubkey_hex: &str,
    signature_b64: &str,
    index: usize,
) -> Result<(), ValidationError> {
    // Decode pubkey from hex
    let vk_bytes = hex::decode(pubkey_hex)
        .map_err(|_| ValidationError::InvalidSignature("invalid pubkey hex".into()))?;
    let vk = VerifyingKey::from_sec1_bytes(&vk_bytes)
        .map_err(|_| ValidationError::InvalidSignature("invalid sec1 pubkey".into()))?;

    // Decode signature from base64
    use base64::prelude::*;
    let sig_bytes = BASE64_STANDARD
        .decode(signature_b64)
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
