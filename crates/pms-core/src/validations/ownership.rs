//! Binding cryptographique unlock ↔ propriétaire de l'UTXO (audit C-1).
//!
//! Une signature de transaction valide ne prouve que la possession de la clé
//! contenue dans l'`Unlock` — clé fournie par l'émetteur lui-même. Sans lien
//! entre cette clé et l'`address` de l'UTXO dépensé, n'importe quel signataire
//! peut dépenser les fonds de n'importe qui. Ce module fournit la comparaison
//! manquante.
//!
//! Deux formes d'adresse coexistent en production :
//! - **Pubkey hex brute** (SDK TypeScript) : l'adresse EST la clé publique
//!   secp256k1 non-compressée en hex (130 chars), avec ou sans préfixe `0x`.
//! - **Bech32m** (wallets Rust, `make_address`) : payload 52 octets =
//!   `SHA256(ecdsa_pub)[..20] || x25519_pub`. Seuls les 20 premiers octets
//!   dépendent de la clé ECDSA — c'est la partie comparée ici (la clé X25519
//!   est une clé de chiffrement, pas d'autorisation).

use sha2::{Digest, Sha256};

/// Renvoie `true` si la clé publique ECDSA d'un `Unlock` autorise la dépense
/// d'un UTXO appartenant à `address`.
///
/// Accepte les deux formes d'adresse de production :
/// 1. `address == pubkey_hex` (forme brute, insensible à la casse et au `0x`) ;
/// 2. `address` bech32m dont le hash20 == `SHA256(pubkey_bytes)[..20]`.
///
/// La comparaison utilise l'encodage de clé tel que fourni (non-compressé en
/// pratique, côté SDK comme côté wallets Rust) — le même que celui utilisé
/// pour dériver l'adresse à la création de l'output.
///
/// # Examples
///
/// ```
/// use pms_core::validations::ownership::unlock_matches_address;
///
/// // Forme brute : l'adresse est la pubkey elle-même.
/// let pk = "04abcdef";
/// assert!(unlock_matches_address(pk, "04ABCDEF"));
/// assert!(unlock_matches_address(pk, "0x04abcdef"));
/// assert!(!unlock_matches_address(pk, "04abcde0"));
/// ```
pub fn unlock_matches_address(pubkey_hex: &str, address: &str) -> bool {
    let pk_norm = pubkey_hex.trim().trim_start_matches("0x");
    let addr = address.trim();

    // Forme 1 : adresse = pubkey hex brute.
    if addr
        .trim_start_matches("0x")
        .eq_ignore_ascii_case(pk_norm)
    {
        return true;
    }

    // Forme 2 : adresse bech32m — hash20 == SHA256(pubkey)[..20].
    if let Ok((h20_hex, _x25519_hex)) = pms_wallet::decode_address(addr) {
        if let Ok(pk_bytes) = hex::decode(pk_norm) {
            let hash = Sha256::digest(&pk_bytes);
            return hex::encode(&hash[..20]).eq_ignore_ascii_case(&h20_hex);
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use pms_wallet::{SignerBackend, Wallet};

    #[test]
    fn bech32m_address_matches_its_own_pubkey() {
        let wallet = Wallet::from_seed(&[1u8; 32], None).expect("wallet");
        let addr = wallet.get_address("8e");
        println!("addr={addr} pubkey={}", wallet.public_key_hex);
        assert!(unlock_matches_address(&wallet.public_key_hex, &addr));
    }

    #[test]
    fn bech32m_address_rejects_foreign_pubkey() {
        let owner = Wallet::from_seed(&[1u8; 32], None).expect("wallet");
        let attacker = Wallet::from_seed(&[2u8; 32], None).expect("wallet");
        let addr = owner.get_address("8e");
        println!(
            "owner addr={addr}, attacker pubkey={}",
            attacker.public_key_hex
        );
        assert!(!unlock_matches_address(&attacker.public_key_hex, &addr));
    }

    #[test]
    fn raw_pubkey_address_matches_case_insensitive_and_0x() {
        let wallet = Wallet::from_seed(&[3u8; 32], None).expect("wallet");
        let pk = wallet.public_key_hex.clone();
        println!("raw pubkey address={pk}");
        assert!(unlock_matches_address(&pk, &pk));
        assert!(unlock_matches_address(&pk, &pk.to_uppercase()));
        assert!(unlock_matches_address(&pk, &format!("0x{pk}")));
    }

    #[test]
    fn raw_pubkey_address_rejects_foreign_pubkey() {
        let owner = Wallet::from_seed(&[4u8; 32], None).expect("wallet");
        let attacker = Wallet::from_seed(&[5u8; 32], None).expect("wallet");
        println!(
            "owner pk={} attacker pk={}",
            owner.public_key_hex, attacker.public_key_hex
        );
        assert!(!unlock_matches_address(
            &attacker.public_key_hex,
            &owner.public_key_hex
        ));
    }

    #[test]
    fn garbage_address_never_matches() {
        let wallet = Wallet::from_seed(&[6u8; 32], None).expect("wallet");
        assert!(!unlock_matches_address(&wallet.public_key_hex, ""));
        assert!(!unlock_matches_address(&wallet.public_key_hex, "not-an-address"));
        assert!(!unlock_matches_address("zz-not-hex", "8e1qqqq"));
    }
}
