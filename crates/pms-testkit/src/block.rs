use pms_types::{PayloadEnvelope, Transaction, Unlock};
use pms_utils::compute_block_id;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::{WireBlock, WireMeta};

/// Signe `tx` avec `wallet` (le propriétaire des UTXO dépensés) et remplit un
/// `Unlock` par input — requis par la validation canonique v0.9.0
/// (`validate_transaction_full` : `unlocks.len() == inputs.len()`, la pubkey
/// de chaque unlock doit autoriser l'adresse de l'UTXO, et la signature porte
/// sur `tx.signing_message(network_id)`). Sans ça → "transaction authorization
/// invalid" / "inputs/unlocks count mismatch".
///
/// Helper partagé : tout test qui forge/poste une `TxUtxo` DOIT signer sa tx
/// avec ça (cf. CLAUDE.md § Anti-faux-tests).
pub fn sign_tx_inputs(wallet: &Wallet, tx: &Transaction, network_id: &str) -> Transaction {
    let msg = tx.signing_message(network_id).expect("signing_message");
    let sig = wallet.sign(&msg).expect("sign");
    let mut signed = tx.clone();
    signed.unlocks = tx
        .inputs
        .iter()
        .map(|_| Unlock {
            pubkey_hex: wallet.public_key_hex.clone(),
            signature_b64: sig.clone(),
        })
        .collect();
    signed
}

pub fn forge_signed_wire_block_for_test(
    parents: Vec<String>,
    meta: &WireMeta,
    wallet: &Wallet,
    nonce: u64,
    payload: Option<PayloadEnvelope>,
) -> WireBlock {
    // 1) Sérialise le payload (ou None -> "null")
    let payload_json = payload.as_ref().and_then(|p| serde_json::to_string(p).ok());

    // 2) Construit le squelette du WireBlock (sans signature)
    let mut wb = WireBlock {
        id: String::new(), // on va le remplir juste après
        parents,
        payload_json,
        nonce,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: wallet.encoded_public_key(), // clé publique en hex
        signature_hex: String::new(),               // signature à remplir après
        metadata: None,
    };

    // 3) Calculer l'ID *avant* la signature, comme en prod
    wb.id = compute_block_id(
        &wb.parents,
        &wb.payload_json
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok()),
        wb.nonce,
    );

    // 4) Construire le message canonique incluant l'ID + meta réseau
    let msg = canonical_wireblock_message(&wb);

    // 5) Signer exactement ce message
    wb.signature_hex = wallet.sign(&msg).expect("sign ne doit pas fail en test");

    wb
}
