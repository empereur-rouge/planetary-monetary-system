//! Burn Refund Logic - Cube NFT to Token Conversion
//!
//! Ce module gère la logique de remboursement lors du burn de NFTs "Cube".
//! Seuls les cubes avec une signature valide de la Game Authority sont éligibles.
//!
//! ## Processus de vérification :
//! 1. Extraire les métadonnées du NFT brûlé
//! 2. Vérifier que `nft_type == "cube"`
//! 3. Extraire la signature du champ `extra`
//! 4. Vérifier la signature contre la clé publique Authority
//! 5. Calculer le remboursement basé sur les attributs
//!
//! ## Voir aussi
//! - Chapitre 9 du Rust Book : Error Handling
//!   https://doc.rust-lang.org/book/ch09-00-error-handling.html

use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose;
use hex::FromHex;
use k256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use pms_types_nft::NftMetadata;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Structure attendue dans le champ `extra` des cubes authentiques
#[derive(Debug, Serialize, Deserialize)]
pub struct CubeExtra {
    /// Rareté du cube (Unique, Legendary, Rare, etc.)
    pub rarity: String,
    /// Attributs du cube
    pub attributes: CubeAttributes,
    /// Roll original (1-10M)
    pub roll: u64,
    /// Signature (Base64 DER) des attributs par la Game Authority
    #[serde(default)]
    pub signature: Option<String>,
}

/// Attributs d'un cube utilisés pour le calcul du remboursement
#[derive(Debug, Serialize, Deserialize)]
pub struct CubeAttributes {
    /// Poids en grammes (ex: 500 à 5000g pour 0.5-5kg)
    pub weight: u32,
    /// Taille en mm (ex: 20 à 80mm pour 2-8cm)
    pub size: u32,
    /// Densité x100 (ex: 20 à 100 pour 0.2-1.0)
    pub density: u32,
}

/// Résultat du calcul de remboursement
#[derive(Debug)]
pub struct BurnRefundResult {
    /// Adresse à créditer (le burner)
    pub recipient: String,
    /// Montant à rembourser
    pub amount: Decimal,
    /// Token ID du cube brûlé
    pub token_id: String,
}

/// Calcule le remboursement pour un cube brûlé.
///
/// # Arguments
/// * `token_id` - ID du NFT brûlé
/// * `burner` - Adresse du propriétaire qui brûle
/// * `metadata` - Métadonnées déchiffrées du NFT (None si pas disponibles)
/// * `authority_pks` - Clés publiques des Authority (hex)
///
/// # Returns
/// * `Some(BurnRefundResult)` si le cube est authentique et éligible
/// * `None` si pas de remboursement (pas un cube, signature invalide, etc.)
///
/// # Privacy Note
/// Le caller (coordinateur) doit déchiffrer les métadonnées depuis le bloc DAG
/// avant d'appeler cette fonction. Les métadonnées ne sont jamais stockées en clair.
pub fn calculate_burn_refund(
    token_id: &str,
    burner: &str,
    metadata: Option<&NftMetadata>,
    authority_pks: &[String],
) -> Result<Option<BurnRefundResult>> {
    // 1. Vérifier qu'on a au moins une Authority key configurée
    if authority_pks.is_empty() {
        tracing::debug!("No authority_public_keys configured, no burn refund");
        return Ok(None);
    }

    // 2. Vérifier qu'on a des métadonnées
    let metadata = match metadata {
        Some(m) => m,
        None => {
            tracing::debug!("No metadata for token {}, no burn refund", token_id);
            return Ok(None);
        }
    };

    // 3. Vérifier que c'est un cube
    if metadata.nft_type.as_deref() != Some("cube") {
        tracing::debug!(
            "Token {} is not a cube ({:?}), no burn refund",
            token_id,
            metadata.nft_type
        );
        return Ok(None);
    }

    // 4. Parser le champ extra
    let extra_str = match &metadata.extra {
        Some(e) => e,
        None => {
            tracing::debug!("No extra field for cube {}, no burn refund", token_id);
            return Ok(None);
        }
    };

    let cube_extra: CubeExtra = match serde_json::from_str(extra_str) {
        Ok(e) => e,
        Err(err) => {
            tracing::warn!("Failed to parse cube extra for {}: {}", token_id, err);
            return Ok(None);
        }
    };

    // 5. Vérifier la signature
    let signature_b64 = match &cube_extra.signature {
        Some(s) => s,
        None => {
            tracing::debug!("No signature in cube {}, no burn refund", token_id);
            return Ok(None);
        }
    };

    // Construire le message signé : canonical JSON des attributs
    let signed_message = attributes_to_signed_message(&cube_extra.attributes);

    // Vérifier contre TOUTES les clés Authority (match ANY)
    let is_valid = authority_pks
        .iter()
        .any(|pk| verify_authority_signature(&signed_message, signature_b64, pk));

    if !is_valid {
        tracing::warn!(
            "Invalid signature for cube {} (tried {} keys), no burn refund",
            token_id,
            authority_pks.len()
        );
        return Ok(None);
    }

    // 6. Calculer le remboursement
    let refund = calculate_refund_amount(&cube_extra.attributes);

    tracing::info!(
        "🎮 Cube burn refund: {} -> {} PMS (w={}, s={}, d={})",
        token_id,
        refund,
        cube_extra.attributes.weight,
        cube_extra.attributes.size,
        cube_extra.attributes.density
    );

    Ok(Some(BurnRefundResult {
        recipient: burner.to_string(),
        amount: refund,
        token_id: token_id.to_string(),
    }))
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
pub fn verify_authority_signature(
    message: &str,
    signature_b64: &str,
    authority_pk_hex: &str,
) -> bool {
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

/// Construit le message canonique à signer pour les attributs.
/// Format: "weight:X,size:Y,density:Z"
pub fn attributes_to_signed_message(attrs: &CubeAttributes) -> String {
    format!(
        "weight:{},size:{},density:{}",
        attrs.weight, attrs.size, attrs.density
    )
}

/// Calcule le montant de remboursement basé sur les attributs.
/// Formule: (weight * size * density) / 10000
///
/// Min: 1 * 1 * 1 / 10000 = 0.0001 PMS
/// Max: 100 * 100 * 100 / 10000 = 100 PMS
/// Calcule le montant de remboursement basé sur les attributs.
/// Formule: (weight_g * size_mm * density_x100) / 19_300_000_000
///
/// Target moyenne hardcore: ~277 PMS/mois
fn calculate_refund_amount(attrs: &CubeAttributes) -> Decimal {
    let product = u64::from(attrs.weight) * u64::from(attrs.size) * u64::from(attrs.density);
    // Diviseur calibré pour ~0.0004275 PMS par cube moyen (Hardcore: 277 PMS/mois)
    let refund = (Decimal::from(product) / Decimal::from(19_300_000_000u64)).round_dp(8);

    // Safety clamp: max 1000 PMS
    let max_refund = Decimal::from(1000);
    if refund > max_refund {
        max_refund
    } else {
        refund
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // ═══════════════════════════════════════════════════════════════════════
    // Tests de calcul de remboursement
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_calculate_refund_min() {
        // Minimum possible: 0.5kg (500g), 2cm (20mm), 0.2 density (20)
        let attrs = CubeAttributes {
            weight: 500,
            size: 20,
            density: 20,
        };
        let refund = calculate_refund_amount(&attrs);
        // 500 * 20 * 20 = 200,000
        // 200,000 / 19,300,000,000 = 0.00001036269...
        // Round dp 8 -> 0.00001036
        assert_eq!(refund.to_string(), "0.00001036");
    }

    #[test]
    fn test_calculate_refund_mid() {
        // Moyen: ~2.75kg (2750g), ~5cm (50mm), ~0.6 density (60)
        let attrs = CubeAttributes {
            weight: 2750,
            size: 50,
            density: 60,
        };
        let refund = calculate_refund_amount(&attrs);
        // 2750 * 50 * 60 = 8,250,000
        // 8,250,000 / 19,300,000,000 = 0.0004274611...
        // Round dp 8 -> 0.00042746
        assert_eq!(refund.to_string(), "0.00042746");
    }

    #[test]
    fn test_calculate_refund_max() {
        // Maximum: 5kg (5000g), 8cm (80mm), 1.0 density (100)
        let attrs = CubeAttributes {
            weight: 5000,
            size: 80,
            density: 100,
        };
        let refund = calculate_refund_amount(&attrs);
        // 5000 * 80 * 100 = 40,000,000
        // 40,000,000 / 19,300,000,000 = 0.002072538...
        // Round dp 8 -> 0.00207254
        assert_eq!(refund.to_string(), "0.00207254");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Tests du format de message
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_attributes_to_signed_message() {
        let attrs = CubeAttributes {
            weight: 2750,
            size: 50,
            density: 60,
        };
        let msg = attributes_to_signed_message(&attrs);
        assert_eq!(msg, "weight:2750,size:50,density:60");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Tests de vérification de signature
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_verify_authority_signature_invalid_pk_format() {
        // Clé publique invalide (pas du hex)
        let result = verify_authority_signature("test", "c2lnbmF0dXJl", "not_hex!");
        assert!(!result);
    }

    #[test]
    fn test_verify_authority_signature_invalid_pk_length() {
        // Clé publique trop courte
        let result = verify_authority_signature("test", "c2lnbmF0dXJl", "abcd");
        assert!(!result);
    }

    #[test]
    fn test_verify_authority_signature_invalid_sig_base64() {
        // Signature pas du base64 valide
        let fake_pk = "04".to_string() + &"00".repeat(64);
        let result = verify_authority_signature("test", "!!!invalid base64!!!", &fake_pk);
        assert!(!result);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Tests de parsing CubeExtra
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_cube_extra_parsing_with_signature() {
        let json = r#"{
            "rarity": "Rare",
            "attributes": {"weight": 2750, "size": 50, "density": 60},
            "roll": 1234567,
            "signature": "MEUCIQDtest..."
        }"#;

        let extra: CubeExtra = serde_json::from_str(json).unwrap();
        assert_eq!(extra.rarity, "Rare");
        assert_eq!(extra.attributes.weight, 2750);
        assert!(extra.signature.is_some());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Tests d'intégration avec la nouvelle API (metadata en paramètre)
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_calculate_burn_refund_no_authority_configured() {
        let result = calculate_burn_refund("token123", "burner_addr", None, &[]).unwrap();

        // Sans authority configurée, pas de remboursement
        assert!(result.is_none());
    }

    #[test]
    fn test_calculate_burn_refund_no_metadata() {
        let fake_pk = "04".to_string() + &"00".repeat(64);

        let result = calculate_burn_refund("nonexistent", "burner", None, &[fake_pk]).unwrap();

        // Pas de métadonnées, pas de remboursement
        assert!(result.is_none());
    }

    #[test]
    fn test_calculate_burn_refund_not_a_cube() {
        use pms_types_nft::NftMetadata;

        let fake_pk = "04".to_string() + &"00".repeat(64);

        // Créer des métadonnées pour un NFT qui n'est pas un cube
        let metadata = NftMetadata {
            name: Some("Not a Cube".to_string()),
            description: None,
            uri: None,
            nft_type: Some("collectible".to_string()), // NOT "cube"
            extra: None,
        };

        let result =
            calculate_burn_refund("token123", "owner", Some(&metadata), &[fake_pk]).unwrap();

        // Pas un cube, pas de remboursement
        assert!(result.is_none());
    }

    #[test]
    fn test_calculate_burn_refund_cube_no_signature() {
        use pms_types_nft::NftMetadata;

        let fake_pk = "04".to_string() + &"00".repeat(64);

        // Cube sans signature
        let extra = serde_json::json!({
            "rarity": "Common",
            "attributes": {"weight": 500, "size": 20, "density": 20},
            "roll": 1234
            // Pas de "signature"
        });

        let metadata = NftMetadata {
            name: Some("Fake Cube".to_string()),
            description: None,
            uri: None,
            nft_type: Some("cube".to_string()),
            extra: Some(extra.to_string()),
        };

        let result =
            calculate_burn_refund("cube123", "owner", Some(&metadata), &[fake_pk]).unwrap();

        // Cube sans signature, pas de remboursement
        assert!(result.is_none());
    }
}
