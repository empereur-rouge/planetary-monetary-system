use std::fs;
use std::path::Path;
use k256::ecdsa::{SigningKey, VerifyingKey, Signature, signature::{Signer, Verifier}};
use base64::{engine::general_purpose, Engine as _};
use bip39::{Language, Mnemonic};
use bip39::rand_core::OsRng;
use sha2::{Digest, Sha256};
use hex;
use k256::{FieldBytes};
use serde::{Deserialize, Serialize};
use crate::types::{SignError, SignerBackend, VerifyError};
use crate::wallet::Wallet;

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
}
