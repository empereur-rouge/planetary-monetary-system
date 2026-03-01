//! Tests pour pms-utils
//!
//! Couverture:
//! - check_pow_leading_zero_bits
//! - hash_meets_difficulty
//! - compute_block_id (déterminisme)
//! - ts_ms (monotonie)

use pms_utils::{check_pow_leading_zero_bits, compute_block_id, hash_meets_difficulty};

// ============================================================================
// Tests check_pow_leading_zero_bits
// ============================================================================

#[test]
fn test_pow_zero_bits_always_passes() {
    // 0 bits requis = toujours vrai
    assert!(check_pow_leading_zero_bits("abcdef1234567890", 0));
    assert!(check_pow_leading_zero_bits("ffffffffffffffff", 0));
    assert!(check_pow_leading_zero_bits("0000000000000000", 0));
}

#[test]
fn test_pow_4_bits_single_nibble() {
    // 4 bits = premier nibble doit être 0 (0x0)
    assert!(check_pow_leading_zero_bits("0abcdef123456789", 4));
    assert!(check_pow_leading_zero_bits(
        "00000000000000000000000000000000",
        4
    ));

    // Premier nibble non-zéro = échec
    assert!(!check_pow_leading_zero_bits("1abcdef123456789", 4));
    assert!(!check_pow_leading_zero_bits("fabcdef123456789", 4));
}

#[test]
fn test_pow_8_bits_full_byte() {
    // 8 bits = premier octet doit être 0x00
    assert!(check_pow_leading_zero_bits("00abcdef12345678", 8));
    assert!(check_pow_leading_zero_bits("0000000000000000", 8));

    // Premier octet non-zéro = échec
    assert!(!check_pow_leading_zero_bits("01abcdef12345678", 8));
    assert!(!check_pow_leading_zero_bits("10abcdef12345678", 8));
}

#[test]
fn test_pow_12_bits() {
    // 12 bits = 1 byte + 4 bits = "000x" pattern
    assert!(check_pow_leading_zero_bits("0001234567890abc", 12));
    assert!(check_pow_leading_zero_bits("0000000000000000", 12));

    // Troisième nibble non-zéro = échec
    assert!(!check_pow_leading_zero_bits("0010234567890abc", 12));
    assert!(!check_pow_leading_zero_bits("00f0234567890abc", 12));
}

#[test]
fn test_pow_16_bits_two_bytes() {
    // 16 bits = 2 premiers octets doivent être 0x0000
    assert!(check_pow_leading_zero_bits("0000abcdef123456", 16));

    assert!(!check_pow_leading_zero_bits("0001abcdef123456", 16));
    assert!(!check_pow_leading_zero_bits("0100abcdef123456", 16));
}

#[test]
fn test_pow_invalid_hex() {
    // Hex invalide retourne false
    assert!(!check_pow_leading_zero_bits("not_hex!", 4));
    assert!(!check_pow_leading_zero_bits("xyz", 1));
}

#[test]
fn test_pow_short_hash() {
    // Hash "00" = 1 octet de valeur 0x00 = 8 bits tous a zero
    // Demander 8 bits sur "00" = OK
    assert!(check_pow_leading_zero_bits("00", 8));

    // Demander 9 bits sur "00" (1 octet):
    // - full_bytes = 1, rem_bits = 1
    // - bytes[0] = 0 (OK), puis bytes[1] n'existe pas -> retourne false
    assert!(!check_pow_leading_zero_bits("00", 9));
}

// ============================================================================
// Tests hash_meets_difficulty
// ============================================================================

#[test]
fn test_difficulty_zero_always_passes() {
    assert!(hash_meets_difficulty("anything", 0));
    assert!(hash_meets_difficulty("ffffffff", 0));
}

#[test]
fn test_difficulty_counts_leading_zeros() {
    // difficulty = nombre de '0' hex en tête
    assert!(hash_meets_difficulty("0abcdef", 1));
    assert!(hash_meets_difficulty("00abcdef", 2));
    assert!(hash_meets_difficulty("000abcdef", 3));
    assert!(hash_meets_difficulty("0000abcdef", 4));

    // Pas assez de zéros
    assert!(!hash_meets_difficulty("1abcdef", 1));
    assert!(!hash_meets_difficulty("0abcdef", 2));
    assert!(!hash_meets_difficulty("00abcdef", 3));
}

// ============================================================================
// Tests compute_block_id
// ============================================================================

#[test]
fn test_compute_block_id_deterministic() {
    let parents = vec!["parent1".to_string(), "parent2".to_string()];
    let nonce = 12345u64;

    // Sans payload
    let id1 = compute_block_id(&parents, &None, nonce);
    let id2 = compute_block_id(&parents, &None, nonce);

    assert_eq!(id1, id2, "compute_block_id doit etre deterministe");
    assert_eq!(id1.len(), 64, "ID doit etre un hash SHA256 hex (64 chars)");
}

#[test]
fn test_compute_block_id_different_nonce() {
    let parents = vec!["parent1".to_string()];

    let id1 = compute_block_id(&parents, &None, 1);
    let id2 = compute_block_id(&parents, &None, 2);

    assert_ne!(
        id1, id2,
        "Nonces differents doivent produire IDs differents"
    );
}

#[test]
fn test_compute_block_id_different_parents() {
    let parents1 = vec!["a".to_string()];
    let parents2 = vec!["b".to_string()];

    let id1 = compute_block_id(&parents1, &None, 0);
    let id2 = compute_block_id(&parents2, &None, 0);

    assert_ne!(
        id1, id2,
        "Parents differents doivent produire IDs differents"
    );
}

#[test]
fn test_compute_block_id_parent_order_matters() {
    let parents1 = vec!["a".to_string(), "b".to_string()];
    let parents2 = vec!["b".to_string(), "a".to_string()];

    let id1 = compute_block_id(&parents1, &None, 0);
    let id2 = compute_block_id(&parents2, &None, 0);

    // L'ordre des parents affecte l'ID (pas de tri automatique dans compute_block_id)
    assert_ne!(
        id1, id2,
        "Ordre des parents different = IDs differents (sans tri)"
    );
}

#[test]
fn test_compute_block_id_sorted_canonical() {
    use pms_utils::compute_block_id_sorted;

    let parents1 = vec!["a".to_string(), "b".to_string()];
    let parents2 = vec!["b".to_string(), "a".to_string()];

    let id1 = compute_block_id_sorted(&parents1, &None, 0);
    let id2 = compute_block_id_sorted(&parents2, &None, 0);

    // Avec _sorted, l'ordre n'importe pas
    assert_eq!(
        id1, id2,
        "compute_block_id_sorted doit produire le meme ID peu importe l'ordre"
    );
}

// ============================================================================
// Tests ts_ms
// ============================================================================

#[test]
fn test_ts_ms_monotonic() {
    use pms_utils::ts_ms;

    let t1 = ts_ms();
    std::thread::sleep(std::time::Duration::from_millis(10));
    let t2 = ts_ms();

    assert!(t2 >= t1, "ts_ms doit etre monotone croissant");
}

#[test]
fn test_ts_ms_reasonable_value() {
    use pms_utils::ts_ms;

    let now = ts_ms();
    // Timestamp doit être après 2020-01-01 (1577836800000 ms)
    assert!(
        now > 1577836800000,
        "ts_ms doit retourner un timestamp raisonnable"
    );
}
