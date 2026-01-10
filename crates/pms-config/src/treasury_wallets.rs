// Treasury Wallets - Signed by Coordinator
// ═══════════════════════════════════════════════════════════════════════════════
//
// This module handles the loading and verification of treasury wallet addresses.
// The list must be signed by the Coordinator's private key.
//
// File format (JSON):
// {
//   "wallets": ["8e1abc...", "8e1def..."],
//   "signature": "hex_signature"
// }
//
// Message format for signature:
// "PMS_TREASURY_v1:<wallet1>,<wallet2>,..."
//
// ═══════════════════════════════════════════════════════════════════════════════

use k256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

/// Treasury wallets file structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreasuryWalletsFile {
    /// List of Bech32m addresses for treasury
    pub wallets: Vec<String>,
    /// Hex-encoded signature by the Coordinator
    pub signature: String,
}

/// Verified treasury wallets (loaded and verified at startup)
#[derive(Debug, Clone)]
pub struct TreasuryWallets {
    /// Set of valid treasury addresses (for O(1) lookup)
    pub addresses: HashSet<String>,
    /// Original list order preserved
    pub list: Vec<String>,
}

impl TreasuryWallets {
    /// Create empty treasury (fallback for dev mode)
    pub fn empty() -> Self {
        Self {
            addresses: HashSet::new(),
            list: Vec::new(),
        }
    }

    /// Check if an address is in the treasury list
    pub fn contains(&self, addr: &str) -> bool {
        self.addresses.contains(addr)
    }

    /// Get first treasury address (for fee distribution)
    pub fn first(&self) -> Option<&String> {
        self.list.first()
    }

    /// Number of treasury wallets
    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

/// Load and verify treasury wallets from JSON file
///
/// # Arguments
/// * `file_path` - Path to the JSON file
/// * `coordinator_public_key` - Hex-encoded coordinator public key for verification
///
/// # Returns
/// * `Ok(TreasuryWallets)` if loaded and signature verified
/// * `Err(String)` if file not found, invalid, or signature mismatch
pub fn load_treasury_wallets(
    file_path: &str,
    coordinator_public_key: &str,
) -> Result<TreasuryWallets, String> {
    // Read file
    let path = Path::new(file_path);
    if !path.exists() {
        return Err(format!("Treasury wallets file not found: {}", file_path));
    }

    let content =
        fs::read_to_string(path).map_err(|e| format!("Failed to read treasury file: {}", e))?;

    let file: TreasuryWalletsFile =
        serde_json::from_str(&content).map_err(|e| format!("Invalid treasury JSON: {}", e))?;

    // Verify signature
    verify_treasury_signature(&file.wallets, &file.signature, coordinator_public_key)?;

    // Build verified result
    let addresses: HashSet<String> = file.wallets.iter().cloned().collect();

    Ok(TreasuryWallets {
        addresses,
        list: file.wallets,
    })
}

/// Verify the coordinator's signature on the wallet list
fn verify_treasury_signature(
    wallets: &[String],
    signature_hex: &str,
    coordinator_pk_hex: &str,
) -> Result<(), String> {
    // Build message: "PMS_TREASURY_v1:<wallet1>,<wallet2>,..."
    let message = format!("PMS_TREASURY_v1:{}", wallets.join(","));

    // Hash the message (SHA256)
    let mut hasher = Sha256::new();
    hasher.update(message.as_bytes());
    let hash = hasher.finalize();

    // Parse coordinator public key
    let pk_bytes = hex::decode(coordinator_pk_hex)
        .map_err(|e| format!("Invalid coordinator public key hex: {}", e))?;
    let verifying_key = VerifyingKey::from_sec1_bytes(&pk_bytes)
        .map_err(|e| format!("Invalid coordinator public key: {}", e))?;

    // Parse signature
    let sig_bytes =
        hex::decode(signature_hex).map_err(|e| format!("Invalid signature hex: {}", e))?;
    let signature =
        Signature::from_der(&sig_bytes).map_err(|e| format!("Invalid DER signature: {}", e))?;

    // Verify
    verifying_key.verify(&hash, &signature).map_err(|_| {
        "Treasury signature verification failed: signature does not match coordinator key"
            .to_string()
    })?;

    Ok(())
}

/// Sign a treasury wallet list (for CLI tool)
///
/// # Arguments
/// * `wallets` - List of wallet addresses
/// * `private_key_hex` - Coordinator's private key in hex
///
/// # Returns
/// * Hex-encoded DER signature
pub fn sign_treasury_wallets(wallets: &[String], private_key_hex: &str) -> Result<String, String> {
    use k256::ecdsa::{SigningKey, signature::Signer};

    // Build message
    let message = format!("PMS_TREASURY_v1:{}", wallets.join(","));

    // Hash the message
    let mut hasher = Sha256::new();
    hasher.update(message.as_bytes());
    let hash = hasher.finalize();

    // Parse private key
    let sk_bytes =
        hex::decode(private_key_hex).map_err(|e| format!("Invalid private key hex: {}", e))?;
    let signing_key =
        SigningKey::from_slice(&sk_bytes).map_err(|e| format!("Invalid private key: {}", e))?;

    // Sign
    let signature: Signature = signing_key.sign(&hash);

    // Return DER-encoded hex
    Ok(hex::encode(signature.to_der()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sign_and_verify_treasury() {
        // Generate test keypair
        use k256::ecdsa::SigningKey;
        use rand::rngs::OsRng;

        let sk = SigningKey::random(&mut OsRng);
        let pk = sk.verifying_key();

        let sk_hex = hex::encode(sk.to_bytes());
        let pk_hex = hex::encode(pk.to_sec1_bytes());

        let wallets = vec!["8e1abc123".to_string(), "8e1def456".to_string()];

        // Sign
        let signature = sign_treasury_wallets(&wallets, &sk_hex).unwrap();

        // Verify
        let result = verify_treasury_signature(&wallets, &signature, &pk_hex);
        assert!(result.is_ok(), "Signature should verify: {:?}", result);
    }

    #[test]
    fn test_verify_fails_with_wrong_wallets() {
        use k256::ecdsa::SigningKey;
        use rand::rngs::OsRng;

        let sk = SigningKey::random(&mut OsRng);
        let pk = sk.verifying_key();

        let sk_hex = hex::encode(sk.to_bytes());
        let pk_hex = hex::encode(pk.to_sec1_bytes());

        let original_wallets = vec!["8e1abc123".to_string()];
        let modified_wallets = vec!["8e1HACKED".to_string()];

        // Sign original
        let signature = sign_treasury_wallets(&original_wallets, &sk_hex).unwrap();

        // Verify with modified list should fail
        let result = verify_treasury_signature(&modified_wallets, &signature, &pk_hex);
        assert!(result.is_err(), "Modified wallets should fail verification");
    }
}
