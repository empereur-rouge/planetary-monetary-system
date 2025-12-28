/// Vérifie qu'un hash hex commence par `leading_zero_bits` bits à zéro.
/// Exemple:
/// - 0  → toujours vrai
/// - 4  → premier nibble = 0x0..0x0
/// - 8  → premier octet = 0x00
pub fn check_pow_leading_zero_bits(id_hex: &str, leading_zero_bits: u8) -> bool {
    if leading_zero_bits == 0 {
        return true;
    }

    // On parse l'hex en bytes
    let bytes = match hex::decode(id_hex) {
        Ok(b) => b,
        Err(_) => return false,
    };

    let full_bytes = (leading_zero_bits / 8) as usize;
    let rem_bits   = (leading_zero_bits % 8) as u8;

    // 1) Bytes complets = 0
    for i in 0..full_bytes {
        if bytes.get(i).copied().unwrap_or(0) != 0 {
            return false;
        }
    }

    if rem_bits == 0 {
        return true;
    }

    // 2) Bits de tête du byte suivant
    let next = match bytes.get(full_bytes).copied() {
        Some(b) => b,
        None => return false,
    };

    // On garde les `rem_bits` bits de poids fort
    // Exemple: rem_bits = 4 → mask = 0b11110000
    let mask: u8 = 0xFF << (8 - rem_bits);
    (next & mask) == 0
}