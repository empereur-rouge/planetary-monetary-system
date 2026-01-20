//! Validation de la signature Authority pour les NFTs Cube.
//!
//! Ce module vérifie que les Cubes sont signés par l'Authority configurée.
//! Sans signature valide, le mint de Cube est rejeté.
//!
//! ## Processus de vérification :
//! 1. Parser le champ `extra` des métadonnées
//! 2. Extraire la signature et les attributs
//! 3. Vérifier la signature contre la clé publique Authority
//!
//! ## Voir aussi
//! - Chapitre 9 du Rust Book : Error Handling
//!   https://doc.rust-lang.org/book/ch09-00-error-handling.html

use anyhow::{Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose;
use hex::FromHex;
use k256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use pms_types_nft::NftMetadata;
use serde::Deserialize;

/// Structure attendue dans le champ `extra` des cubes authentiques.
/// Dupliquée ici depuis pms-server pour éviter la dépendance circulaire.
#[derive(Debug, Deserialize)]
pub struct CubeExtra {
    /// Attributs du cube
    pub attributes: CubeAttributes,
    /// Signature (Base64 DER) des attributs par l'Authority
    #[serde(default)]
    pub signature: Option<String>,
}

/// Attributs d'un cube utilisés pour la vérification.
#[derive(Debug, Deserialize)]
pub struct CubeAttributes {
    pub weight: u32,
    pub size: u32,
    pub density: u32,
}

/// Erreur de validation Cube.
#[derive(Debug, Clone)]
pub enum CubeValidationError {
    /// Le champ extra est manquant
    MissingExtra,
    /// Le champ extra n'est pas un JSON valide
    InvalidExtra(String),
    /// La signature est manquante dans le champ extra
    MissingSignature,
    /// La signature est invalide
    InvalidSignature,
}

impl std::fmt::Display for CubeValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CubeValidationError::MissingExtra => {
                write!(f, "Cube metadata missing 'extra' field")
            }
            CubeValidationError::InvalidExtra(e) => {
                write!(f, "Cube 'extra' field is not valid JSON: {}", e)
            }
            CubeValidationError::MissingSignature => {
                write!(f, "Cube 'extra' missing Authority signature")
            }
            CubeValidationError::InvalidSignature => {
                write!(f, "Cube Authority signature is invalid")
            }
        }
    }
}

impl std::error::Error for CubeValidationError {}

/// Valide qu'un NFT Cube a une signature Authority valide.
///
/// Cette fonction est appelée lors du Mint d'un NFT de type "cube".
/// Elle vérifie que le champ `extra` contient une signature valide des attributs.
///
/// # Arguments
/// * `metadata` - Les métadonnées du NFT
/// * `authority_pks` - Liste des clés publiques Authority autorisées (hex, SEC1)
///
/// # Returns
/// * `Ok(())` si la signature est valide avec au moins une des clés
/// * `Err` si la validation échoue
pub fn validate_cube_authority_signature(
    metadata: &NftMetadata,
    authority_pks: &[String],
) -> Result<()> {
    // 1. Extraire le champ extra
    let extra_str = metadata
        .extra
        .as_ref()
        .ok_or_else(|| anyhow!(CubeValidationError::MissingExtra))?;

    // 2. Parser en CubeExtra
    let cube_extra: CubeExtra = serde_json::from_str(extra_str)
        .map_err(|e| anyhow!(CubeValidationError::InvalidExtra(e.to_string())))?;

    // 3. Vérifier la présence de la signature
    let signature_b64 = cube_extra
        .signature
        .as_ref()
        .ok_or_else(|| anyhow!(CubeValidationError::MissingSignature))?;

    // 4. Construire le message canonique
    let message = format!(
        "weight:{},size:{},density:{}",
        cube_extra.attributes.weight, cube_extra.attributes.size, cube_extra.attributes.density
    );

    // 5. Vérifier la signature contre TOUTES les clés Authority (match ANY)
    let is_valid = authority_pks
        .iter()
        .any(|pk| verify_authority_signature(&message, signature_b64, pk));

    if !is_valid {
        return Err(anyhow!(CubeValidationError::InvalidSignature));
    }

    Ok(())
}

/// Vérifie la signature de l'Authority sur les attributs.
///
/// # Arguments
/// * `message` - Le message canonique ("weight:X,size:Y,density:Z")
/// * `signature_b64` - Signature en Base64 DER
/// * `authority_pk_hex` - Clé publique Authority en hex (SEC1)
///
/// # Returns
/// `true` si la signature est valide, `false` sinon.
fn verify_authority_signature(message: &str, signature_b64: &str, authority_pk_hex: &str) -> bool {
    // Décoder la clé publique
    let pk_bytes = match <Vec<u8>>::from_hex(authority_pk_hex) {
        Ok(b) => b,
        Err(_) => return false,
    };

    let verify_key = match VerifyingKey::from_sec1_bytes(&pk_bytes) {
        Ok(k) => k,
        Err(_) => return false,
    };

    // Décoder la signature (Base64 DER)
    let sig_bytes = match general_purpose::STANDARD.decode(signature_b64) {
        Ok(b) => b,
        Err(_) => return false,
    };

    let signature = match Signature::from_der(&sig_bytes) {
        Ok(s) => s,
        Err(_) => return false,
    };

    // Vérifier
    verify_key.verify(message.as_bytes(), &signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_missing_extra_rejected() {
        let metadata = NftMetadata {
            name: Some("Test Cube".into()),
            nft_type: Some("cube".into()),
            extra: None, // Missing!
            ..Default::default()
        };

        let result = validate_cube_authority_signature(&metadata, &["04abcd".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn test_missing_signature_rejected() {
        let metadata = NftMetadata {
            name: Some("Test Cube".into()),
            nft_type: Some("cube".into()),
            extra: Some(r#"{"rarity":"Common","attributes":{"weight":50,"size":50,"density":50},"roll":123}"#.into()),
            ..Default::default()
        };

        let result = validate_cube_authority_signature(&metadata, &["04abcd".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn test_invalid_json_rejected() {
        let metadata = NftMetadata {
            name: Some("Test Cube".into()),
            nft_type: Some("cube".into()),
            extra: Some("not valid json".into()),
            ..Default::default()
        };

        let result = validate_cube_authority_signature(&metadata, &["04abcd".to_string()]);
        assert!(result.is_err());
    }
}
