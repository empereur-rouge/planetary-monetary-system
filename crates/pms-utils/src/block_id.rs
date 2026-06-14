use pms_types::{BlockId, EncryptedPayload, PayloadEnvelope, PlainPayload};
use pms_wire::WireBlock;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Serialize)]
struct EnvelopeHeader {
    payload_type: String, // "None" | "Genesis" | "Mint" | "Transaction" | "Milestone" | "Encrypted"
    commitment: String,   // hex(sha256(payload clair)) OU encrypted.commitment
    len_hint: u32,        // taille approx. (clair ou chiffré)
    key_version: u32,     // 0 si Plain/None, sinon version de clé pour l'encrypt
}

#[derive(Serialize)]
struct BlockHeaderForId<'a> {
    parents: &'a [String],
    nonce: u64,
    envelope: EnvelopeHeader,
}

/// Calcule l'identifiant canonique d'un block:
/// id = hex(sha256( json({ parents, nonce, envelope_header }) ))
pub fn compute_block_id(
    parents: &[String],
    payload: &Option<PayloadEnvelope>,
    nonce: u64,
) -> BlockId {
    // -- construire l'en-tête public de l'enveloppe (sans dévoiler le contenu) --
    let (payload_type, commitment, len_hint, key_version) = match payload {
        None => (
            "None".to_string(),
            // 32 octets de zéros en hex pour "pas de payload"
            "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            0u32,
            0u32,
        ),
        Some(PayloadEnvelope::Plain(plain)) => {
            // Déterminer le type lisible
            let ptype = match plain {
                PlainPayload::Genesis => "Genesis",
                PlainPayload::Mint { .. } => "Mint",
                PlainPayload::TxUtxo(_) => "Transaction",
                PlainPayload::TokenBurn { .. } => "TokenBurn",
                PlainPayload::Milestone { .. } => "Milestone",
                PlainPayload::Nft(_) => "Nft",
                PlainPayload::ConfigUpdate(_) => "ConfigUpdate",
                PlainPayload::GovernanceProposal { .. } => "GovernanceProposal",
                PlainPayload::GovernanceEnact { .. } => "GovernanceEnact",
                PlainPayload::GovernanceCancel { .. } => "GovernanceCancel",
                PlainPayload::Reward { .. } => "Reward",
                PlainPayload::EncryptedReward { .. } => "EncryptedReward",
                PlainPayload::TokenCreate(_) => "TokenCreate",
                PlainPayload::BridgeLock { .. } => "BridgeLock",
                PlainPayload::BridgeMint { .. } => "BridgeMint",
                PlainPayload::Freeze { .. } => "Freeze",
                PlainPayload::Unfreeze { .. } => "Unfreeze",
                PlainPayload::Seize { .. } => "Seize",
                PlainPayload::Reverse { .. } => "Reverse",
                PlainPayload::ContractRegister(_) => "ContractRegister",
                PlainPayload::ContractUpdate { .. } => "ContractUpdate",
                PlainPayload::LedgerOwnershipTransfer { .. } => "LedgerOwnershipTransfer",
                PlainPayload::CoordinatorKeyRotate { .. } => "CoordinatorKeyRotate",
                PlainPayload::ReserveSnapshot { .. } => "ReserveSnapshot",
            }
            .to_string();

            // commitment = hash du payload clair (liaison forte sans chiffrer)
            let bytes = serde_json::to_vec(plain).expect("serialize plain payload");
            let hash = Sha256::digest(&bytes);
            let commitment_hex = hex::encode(hash);

            (ptype, commitment_hex, bytes.len() as u32, 0u32)
        }
        Some(PayloadEnvelope::Encrypted(EncryptedPayload {
            scheme: _,
            key_version: kv,
            aad: _,
            commitment,
            ciphertext_b64,
            recipients: _,
            nonce_b64: _,
        })) => {
            // On n'inclut pas le ciphertext dans l'ID; on lie via 'commitment'
            (
                "Encrypted".to_string(),
                commitment.clone(),
                ciphertext_b64.len() as u32, // indice public de taille
                *kv,
            )
        }
    };

    let env = EnvelopeHeader {
        payload_type,
        commitment,
        len_hint,
        key_version,
    };

    // En-tête du block utilisé pour l'ID (parents + nonce + méta publique de l'enveloppe)
    let head = BlockHeaderForId {
        parents,
        nonce,
        envelope: env,
    };

    // Sérialisation déterministe (ordre des champs fixé par la struct)
    let bytes = serde_json::to_vec(&head).expect("serialize block header");

    // Hash SHA-256 puis encodage hex → BlockId
    let id = Sha256::digest(&bytes);
    hex::encode(id)
}

pub fn compute_block_id_sorted(
    parents: &[String],
    payload: &Option<PayloadEnvelope>,
    nonce: u64,
) -> BlockId {
    let mut ps = parents.to_vec();
    ps.sort(); // canonique
    compute_block_id(&ps, payload, nonce) // ta version existante
}

pub fn compute_id_adapter(wb: &WireBlock) -> String {
    let payload = wb
        .payload_json
        .as_ref()
        .and_then(|s| serde_json::from_str(s).ok());

    compute_block_id(&wb.parents, &payload, wb.nonce)
}
