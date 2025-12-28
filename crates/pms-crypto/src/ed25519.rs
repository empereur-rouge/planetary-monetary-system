use ed25519_dalek::{SigningKey, VerifyingKey, Signature, Signer, Verifier};

pub fn sign_input(sk: &SigningKey, txid: &[u8], input_index: u32) -> Vec<u8> {
    let mut msg = Vec::with_capacity(txid.len()+4);
    msg.extend_from_slice(txid);
    msg.extend_from_slice(&input_index.to_le_bytes());
    sk.sign(&msg).to_bytes().to_vec()
}

pub fn verify_input(vk: &VerifyingKey, txid: &[u8], input_index: u32, sig_bytes: &[u8]) -> bool {
    // message = txid || LE(input_index)
    let mut msg = Vec::with_capacity(txid.len() + 4);
    msg.extend_from_slice(txid);
    msg.extend_from_slice(&input_index.to_le_bytes());

    // parse signature (64 octets)
    let sig = match Signature::try_from(sig_bytes) {
        Ok(s) => s,
        Err(_) => return false,
    };

    vk.verify(&msg, &sig).is_ok()
}
