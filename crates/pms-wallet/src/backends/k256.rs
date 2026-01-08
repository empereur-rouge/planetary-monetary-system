use crate::types::{SignError, SignerBackend, VerifyError};
use crate::wallet::Wallet;
use base64::{Engine as _, engine::general_purpose};
use k256::FieldBytes;
use k256::ecdsa::{
    Signature, SigningKey, VerifyingKey,
    signature::{Signer, Verifier},
};

impl SignerBackend for Wallet {
    fn sign(&self, message: &str) -> Result<String, SignError> {
        let priv_bytes_vec = general_purpose::STANDARD
            .decode(&self.private_key_b64)
            .map_err(|_| SignError::Base64Decode)?;

        let priv_bytes: [u8; 32] = priv_bytes_vec
            .try_into()
            .map_err(|_| SignError::InvalidLength)?;

        let signing_key = SigningKey::from_bytes(&FieldBytes::from(priv_bytes))
            .map_err(|_| SignError::SigningKey)?;

        let sig: Signature = signing_key.sign(message.as_bytes());

        Ok(general_purpose::STANDARD.encode(sig.to_der()))
    }

    fn verify(&self, message: &str, signature: &str) -> Result<bool, VerifyError> {
        let sig_bytes = general_purpose::STANDARD
            .decode(signature)
            .map_err(|_| VerifyError::Base64Decode)?;

        let sig = Signature::from_der(&sig_bytes).map_err(|_| VerifyError::SignatureFormat)?;

        let pub_bytes = hex::decode(&self.public_key_hex).map_err(|_| VerifyError::HexDecode)?;

        let verify_key =
            VerifyingKey::from_sec1_bytes(&pub_bytes).map_err(|_| VerifyError::InvalidPubKey)?;

        Ok(verify_key.verify(message.as_bytes(), &sig).is_ok())
    }

    fn from_seed(seed: &[u8], mnemonic_words: Option<Vec<String>>) -> Result<Self, String> {
        let key_bytes: [u8; 32] = seed[..32]
            .try_into()
            .map_err(|_| "Seed slice too short".to_string())?;

        let signing_key = SigningKey::from_bytes(&FieldBytes::from(key_bytes))
            .map_err(|e| format!("Invalid signing key: {e}"))?;

        let verifying_key = signing_key.verifying_key();

        let mut w = Self {
            private_key_b64: general_purpose::STANDARD.encode(signing_key.to_bytes()),
            public_key_hex: hex::encode(verifying_key.to_encoded_point(false).as_bytes()),
            x25519_pub_hex: String::new(), // rempli juste après
            mnemonic_words,
        };

        // ✅ X25519 pk dérivé de la clé privée ECDSA (source unique)
        let (_sk_hex, pk_hex) = w
            .derive_x25519_pair_from_private_key_b64()
            .ok_or_else(|| "x25519 derivation failed".to_string())?;

        w.x25519_pub_hex = pk_hex;
        Ok(w)
    }

    fn encoded_private_key(&self) -> String {
        self.private_key_b64.clone()
    }

    fn encoded_public_key(&self) -> String {
        self.public_key_hex.clone()
    }
}
