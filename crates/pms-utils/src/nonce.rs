pub fn hash_meets_difficulty(id_hex: &str, difficulty: u32) -> bool {
    if difficulty == 0 {
        return true;
    }

    let needed = difficulty as usize;
    // approche simple : "difficulty" = nombre de zéros hex en tête
    id_hex.chars().take(needed).all(|c| c == '0')
}