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
            let h20 = pms_wallet::pubkey_hash20(&pk_bytes);
            return hex::encode(h20).eq_ignore_ascii_case(&h20_hex);
        }
    }

    false
}

/// Canonicalise une adresse-propriétaire vers son **identité** stable, de sorte
/// que les formes équivalentes d'une même clé collapsent sur une seule entrée.
///
/// C'est le pendant « clé de comptabilité » de [`unlock_matches_address`] (même
/// relation forme↔hash20) : partout où un flux/propriété doit être attribué à
/// une partie **indépendamment de l'encodage** de l'adresse stockée
/// (comptabilité de settlement, conservation d'un burn), keyer par
/// `address_identity` plutôt que par la string brute. Les deux formes d'un même
/// wallet (pubkey secp hex « forme SDK » et bech32m « forme nœud ») ont la même
/// identité — c'est ce qui débloque les fonds mintés vers la forme hex.
///
/// - **pubkey secp hex brute** (33 ou 65 octets, préfixe `0x`/`0X` optionnel) →
///   `sha256(pubkey)[..20]` hex, exactement le hash20 que le bech32m embarque ;
/// - **bech32m** → son hash20 (20 premiers octets du payload) ;
/// - **autre forme** (multisig `msig1…`, adresse inconnue) → `"nonkey:"` + le
///   string en minuscules (aucun collapse — déjà canonique, pas d'équivalent
///   hex). Le préfixe `nonkey:` est **essentiel** : sans lui, une string de 40
///   hex (= un hash20 nu, non-pubkey non-bech32m) collapserait sur l'identité
///   d'une partie réelle (`sha256(pubkey)[..20]` fait aussi 40 hex) → un output
///   de settlement adressé à un hash20 nu passerait la comptabilité tout en
///   étant **indépensable** (aucune clé ne déverrouille un hash20 nu →
///   « black-hole » de la royalty). Le préfixe rend ce collapse impossible.
///
/// # Examples
///
/// ```
/// use pms_core::validations::ownership::address_identity;
/// // Une pubkey secp (ici 33 octets = 66 hex) : `0x` et la casse sont
/// // normalisés vers la même identité (sha256(pubkey)[..20]).
/// let pk = String::from("02") + &"ab".repeat(32); // 33 octets
/// assert_eq!(
///     address_identity(&pk),
///     address_identity(&format!("0x{}", pk.to_uppercase())),
/// );
/// // Une forme inconnue est keyée par elle-même, préfixée `nonkey:`.
/// assert_eq!(address_identity("MSIG1abc"), "nonkey:msig1abc");
/// // Un hash20 nu (40 hex) NE collapse PAS sur l'identité d'une pubkey/bech32m.
/// let bare_hash20 = "ab".repeat(20); // 40 hex = 20 octets
/// assert_eq!(address_identity(&bare_hash20), format!("nonkey:{bare_hash20}"));
/// ```
pub fn address_identity(addr: &str) -> String {
    let a = addr.trim();

    // Forme pubkey secp brute (0x/0X optionnel) : identité = sha256(pubkey)[..20].
    // La contrainte de longueur (33 compressé / 65 non-compressé) évite de hasher
    // par erreur un hex arbitraire qui ne serait pas une clé.
    let stripped = a.strip_prefix("0x").or_else(|| a.strip_prefix("0X")).unwrap_or(a);
    if let Ok(pk) = hex::decode(stripped) {
        if pk.len() == 33 || pk.len() == 65 {
            // Même dérivation pubkey→hash20 que make_address / get_address.
            return hex::encode(pms_wallet::pubkey_hash20(&pk));
        }
    }

    // Forme bech32m : identité = hash20 (20 premiers octets du payload).
    if let Ok((h20_hex, _x25519)) = pms_wallet::decode_address(a) {
        return h20_hex;
    }

    // Forme inconnue/multisig : keyée par elle-même, PRÉFIXÉE pour ne jamais
    // collisionner avec un hash20 réel (cf. doc — anti « black-hole »).
    format!("nonkey:{}", a.to_lowercase())
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
