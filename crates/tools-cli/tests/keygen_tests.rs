//! Tests pour le générateur de clés coordinateur
//!
//! Voir `src/keygen.rs` pour l'implémentation.

use std::fs;

/// Test que la clé privée générée fait exactement 64 caractères hex
///
/// ## Explication
/// - Le nœud PMS attend une clé privée en format hex (64 caractères)
/// - 64 caractères hex = 32 bytes = 256 bits (taille standard secp256k1)
/// - Ce test garantit que le keygen produit le bon format
#[test]
fn test_generate_and_save_produces_64_char_hex_key() {
    let key_path = "/tmp/test_keygen_node.key";
    let json_path = "/tmp/test_keygen_admin.json";

    // Générer les clés via la commande CLI
    let output = std::process::Command::new("cargo")
        .args([
            "run",
            "-p",
            "tools-cli",
            "--",
            "gen-coordinator",
            key_path,
            json_path,
        ])
        .output()
        .expect("Failed to run gen-coordinator command");

    assert!(
        output.status.success(),
        "gen-coordinator should succeed: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Lire le fichier de clé
    let key_content = fs::read_to_string(key_path).expect("Should read key file");

    // Vérifier la longueur (64 caractères hex = 32 bytes)
    assert_eq!(
        key_content.len(),
        64,
        "Private key should be exactly 64 hex characters, got {}",
        key_content.len()
    );

    // Vérifier que c'est bien du hex valide
    assert!(
        hex::decode(&key_content).is_ok(),
        "Private key should be valid hex"
    );

    // Vérifier que le JSON est valide
    let json_content = fs::read_to_string(json_path).expect("Should read json file");
    let parsed: serde_json::Value =
        serde_json::from_str(&json_content).expect("JSON should be valid");

    // Vérifier que le JSON contient les champs attendus
    assert!(
        parsed.get("private_key").is_some(),
        "JSON should have private_key"
    );
    assert!(
        parsed.get("public_key").is_some(),
        "JSON should have public_key"
    );
    assert!(parsed.get("address").is_some(), "JSON should have address");

    // Cleanup
    let _ = fs::remove_file(key_path);
    let _ = fs::remove_file(json_path);
}
