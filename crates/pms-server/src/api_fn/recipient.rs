//! Validation fail-fast d'une **adresse-destinataire** avant l'écriture d'un
//! output (mint, faucet, send, on-ramp).
//!
//! ## Pourquoi
//!
//! L'index d'adresse est keyé par le string-propriétaire **exact** écrit à la
//! création de l'output. Un `to` qui n'est aucune forme d'adresse dépensable
//! (typo de checksum bech32, pubkey secp tronquée / hors-courbe, garbage) crée
//! un UTXO que **personne** ne peut dépenser — des fonds « piégés » définitifs.
//!
//! La coin-selection multi-forme ([`crate::api_fn::tx_helpers::select_utxos_multi`])
//! récupère les fonds mintés vers une forme *valide* mais non-canonique (pubkey
//! hex minuscule). Ce garde-fou attrape l'autre moitié du problème : un `to`
//! qui n'est **pas** une forme valide du tout — rejeté AVANT de forger le bloc
//! (`ApiError::InvalidAddress`, code `2010`).
//!
//! ## Formes acceptées
//!
//! - **bech32m single-key** — checksum + payload 52 octets valides
//!   ([`pms_wallet::decode_address`]) ;
//! - **multisig canonique** — `msig1` + 40 hex minuscules (adresse M-of-N,
//!   [`pms_core::validations::conditions::multisig_address`]) ;
//! - **pubkey secp256k1 hex minuscule** — 33 (compressée) / 65 (non-compressée)
//!   octets, point valide sur la courbe, préfixe `0x` optionnel.
//!
//! La casse **minuscule** est imposée pour la forme hex : la coin-selection
//! indexe/retrouve la forme minuscule (cf. [`pms_wallet::spend_address_forms`]) —
//! accepter une casse mixte re-piégerait les fonds. bech32m et le hash20
//! multisig sont déjà minuscules par construction.

use crate::api_error::ApiError;
use axum::Json;
use http::StatusCode;
use pms_core::validations::conditions::MULTISIG_ADDR_PREFIX;
use serde_json::{Value, json};

/// Valide qu'une adresse-destinataire est une forme dépensable connue.
/// Renvoie `Err(ApiError::InvalidAddress)` (code `2010`) sinon.
///
/// # Examples
///
/// ```
/// use pms_server::api_fn::recipient::validate_recipient_address;
/// use pms_wallet::{SignerBackend, Wallet};
///
/// let w = Wallet::from_seed(&[2u8; 32], None).unwrap();
/// // Forme canonique bech32m : OK.
/// assert!(validate_recipient_address(&w.get_address("8e")).is_ok());
/// // Forme SDK (pubkey hex minuscule) : OK.
/// assert!(validate_recipient_address(&w.public_key_hex).is_ok());
/// // Typo / garbage : rejeté.
/// assert!(validate_recipient_address("8e1zzzznot-an-address").is_err());
/// assert!(validate_recipient_address("").is_err());
/// ```
pub fn validate_recipient_address(to: &str) -> Result<(), ApiError> {
    let t = to.trim();
    if t.is_empty() {
        return Err(ApiError::InvalidAddress { addr: to.to_string() });
    }

    // 1) bech32m single-key (checksum + payload 52 octets validés).
    if pms_wallet::decode_address(t).is_ok() {
        return Ok(());
    }

    // 2) Multisig canonique : `msig1` + 40 hex minuscules (20 octets).
    if let Some(rest) = t.strip_prefix(MULTISIG_ADDR_PREFIX) {
        if rest.len() == 40 && is_lower_hex(rest) {
            return Ok(());
        }
    }

    // 3) Pubkey secp256k1 hex minuscule (33/65 octets), point valide.
    let stripped = t.strip_prefix("0x").unwrap_or(t);
    if is_lower_hex(stripped) && pms_wallet::is_valid_secp_pubkey_hex(stripped) {
        return Ok(());
    }

    Err(ApiError::InvalidAddress { addr: to.to_string() })
}

/// `true` si `s` est non vide et composé uniquement de `[0-9a-f]` (hex
/// canonique minuscule — aucune majuscule tolérée, cf. contrainte de casse).
fn is_lower_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Variante « tuple » de [`validate_recipient_address`] pour les handlers legacy
/// qui renvoient `impl IntoResponse` (pas encore migrés à `ApiError` + `?`).
///
/// Renvoie `Some(rejet)` si `to` n'est pas une forme dépensable, `None` sinon.
/// Le status/message/code sont **dérivés de `ApiError::InvalidAddress`** → une
/// seule source de vérité : pas de `2010` codé en dur ni de message divergent
/// avec les handlers migrés (règle du repo : toute erreur passe par `ApiError`).
pub fn reject_bad_recipient(to: &str) -> Option<(StatusCode, Json<Value>)> {
    validate_recipient_address(to).err().map(|e| {
        (
            e.http_status(),
            Json(json!({ "error": e.public_message(), "code": e.code() })),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pms_core::validations::conditions::multisig_address;
    use pms_wallet::{SignerBackend, Wallet};

    #[test]
    fn accepts_bech32m_canonical() {
        let w = Wallet::from_seed(&[3u8; 32], None).unwrap();
        let addr = w.get_address("8e");
        println!("bech32m={addr}");
        assert!(validate_recipient_address(&addr).is_ok());
    }

    #[test]
    fn accepts_raw_secp_pubkey_lowercase_and_0x() {
        let w = Wallet::from_seed(&[4u8; 32], None).unwrap();
        let pk = w.public_key_hex.clone();
        println!("pubkey={pk}");
        assert!(validate_recipient_address(&pk).is_ok(), "bare lowercase pubkey");
        assert!(validate_recipient_address(&format!("0x{pk}")).is_ok(), "0x-prefixed");
    }

    #[test]
    fn accepts_multisig_address() {
        let a = multisig_address(2, &["04aa".into(), "04bb".into()]);
        println!("multisig={a}");
        assert!(validate_recipient_address(&a).is_ok());
    }

    #[test]
    fn rejects_uppercase_pubkey_hex() {
        // Casse non canonique : re-piégerait les fonds (index case-sensitive,
        // spend_address_forms produit la forme minuscule) → rejet fail-fast.
        let w = Wallet::from_seed(&[5u8; 32], None).unwrap();
        let upper = w.public_key_hex.to_uppercase();
        println!("UPPER pubkey={upper}");
        let r = validate_recipient_address(&upper);
        println!("→ {r:?}");
        assert!(r.is_err(), "uppercase hex must be rejected (non-canonical case)");
    }

    #[test]
    fn rejects_bech32m_with_bad_checksum() {
        let w = Wallet::from_seed(&[6u8; 32], None).unwrap();
        let mut addr = w.get_address("8e");
        // Corrompt le dernier caractère (checksum bech32m) → typo réaliste.
        let last = addr.pop().unwrap();
        addr.push(if last == 'q' { 'p' } else { 'q' });
        println!("corrupted bech32m={addr}");
        assert!(validate_recipient_address(&addr).is_err(), "bad checksum must be rejected");
    }

    #[test]
    fn rejects_truncated_pubkey_and_garbage() {
        assert!(validate_recipient_address("04deadbeef").is_err(), "trop court pour une pubkey");
        assert!(validate_recipient_address("not-an-address").is_err());
        assert!(validate_recipient_address("").is_err());
        assert!(validate_recipient_address("   ").is_err());
        // 65 octets hex mais point hors-courbe (04 + 128 zéros) → rejeté.
        let off_curve = format!("04{}", "0".repeat(128));
        println!("off-curve len={}", off_curve.len());
        assert!(validate_recipient_address(&off_curve).is_err(), "point hors-courbe rejeté");
    }
}
