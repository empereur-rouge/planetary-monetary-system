//! Générateur de clés pour le Coordinateur (Master Node).
//!
//! Ce module génère une paire de clés ECDSA (secp256k1) utilisée pour :
//! - Signer les blocs Milestone (avec la clé privée)
//! - Vérifier l'authenticité des Milestones (avec la clé publique)
//!
//! ## Utilisation
//!
//! ```bash
//! cargo run -p tools-cli -- keygen
//! ```
//!
//! ## Sortie
//!
//! Le programme affiche :
//! - La clé publique (à mettre dans `pms-consensus/src/lib.rs`)
//! - La clé privée (à stocker dans un fichier sécurisé sur le serveur Coordinateur)
//!
//! ## Sécurité
//!
//! ⚠️ La clé privée doit être gardée secrète. Elle donne le pouvoir
//! d'émettre des Milestones et donc de contrôler la finalité du réseau.

use k256::ecdsa::SigningKey;
use k256::elliptic_curve::rand_core::OsRng;

/// Génère une nouvelle paire de clés Coordinateur et l'affiche.
///
/// # Explication du code (Chapitre 10 - Generic Types, Traits, Lifetimes)
///
/// - `SigningKey::random(&mut OsRng)` : Génère une clé privée aléatoire
///   en utilisant le générateur de nombres aléatoires du système d'exploitation.
///   C'est la source d'entropie la plus sécurisée disponible.
///
/// - `signing_key.verifying_key()` : Dérive la clé publique à partir de
///   la clé privée. C'est une opération mathématique sur courbe elliptique
///   (secp256k1, la même que Bitcoin).
///
/// - `to_sec1_bytes()` : Sérialise la clé au format SEC1 (le standard
///   pour les clés de courbe elliptique). La clé publique compressée
///   fait 33 bytes et commence par `02` ou `03`.
pub fn run_keygen() {
    println!("╔═══════════════════════════════════════════════════════════════╗");
    println!("║       PMS Coordinator Key Generator (Master Node)             ║");
    println!("╚═══════════════════════════════════════════════════════════════╝");
    println!();

    // 1) Générer une clé privée ECDSA secp256k1 aléatoire
    //    OsRng = utilise /dev/urandom (Unix) ou CryptGenRandom (Windows)
    let signing_key = SigningKey::random(&mut OsRng);

    // 2) Dériver la clé publique correspondante
    let verifying_key = signing_key.verifying_key();

    // 3) Sérialiser en hex pour stockage/affichage
    //    - Clé privée : 32 bytes (256 bits)
    //    - Clé publique compressée : 33 bytes (commence par 02 ou 03)
    let private_key_hex = hex::encode(signing_key.to_bytes());
    let public_key_hex = hex::encode(verifying_key.to_sec1_bytes());

    // 4) Affichage avec instructions
    println!("🔑 CLÉS GÉNÉRÉES AVEC SUCCÈS");
    println!();
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📜 CLÉ PUBLIQUE (à mettre dans pms-consensus/src/lib.rs) :");
    println!();
    println!("   {}", public_key_hex);
    println!();
    println!("   Exemple d'utilisation :");
    println!(
        "   pub const COORDINATOR_PUBLIC_KEY_MAINNET: &str = \"{}\";",
        public_key_hex
    );
    println!();
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("🔐 CLÉ PRIVÉE (à stocker dans un fichier sécurisé) :");
    println!();
    println!("   {}", private_key_hex);
    println!();
    println!("   ⚠️  ATTENTION : Ne partagez JAMAIS cette clé !");
    println!("   ⚠️  Celui qui possède cette clé contrôle les Milestones.");
    println!();
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📝 PROCHAINES ÉTAPES :");
    println!();
    println!("   1. Copiez la CLÉ PUBLIQUE dans :");
    println!("      crates/pms-consensus/src/lib.rs");
    println!();
    println!("   2. Sauvegardez la CLÉ PRIVÉE dans un fichier sécurisé :");
    println!(
        "      echo '{}' > /secure/path/coordinator.key",
        private_key_hex
    );
    println!("      chmod 600 /secure/path/coordinator.key");
    println!();
    println!("   3. Configurez le nœud Coordinateur pour utiliser cette clé.");
    println!();
}

use anyhow::Result;
use pms_wallet::{SignerBackend, Wallet};
use serde_json::json;
use std::fs;
use std::path::Path;

/// Génère une clé, la sauvegarde dans un fichier brut et exporte un JSON complet.
pub fn generate_and_save(key_path: &str, json_path: &str) -> Result<()> {
    println!("🔑 Generation des clés coordinateur...");

    // 1. Générer via pms-wallet (compatible secp256k1 + adresse bech32)
    let wallet = Wallet::generate();

    let priv_hex = wallet.encoded_private_key();
    let pub_hex = wallet.encoded_public_key();
    let address = wallet.get_address("8e"); // HRP par défaut

    // 2. Sauvegarder la clé privée brute (pour le nœud)
    // Le nœud attend souvent 64 chars hex sans newline, ou binaire.
    // pms-node charge souvent hex string.
    fs::write(key_path, &priv_hex)?;
    println!("✅ Clé privée sauvegardée dans : {}", key_path);

    // 3. Sauvegarder le JSON (pour l'admin)
    let info = json!({
        "private_key": priv_hex,
        "public_key": pub_hex,
        "address": address,
        "type": "coordinator",
        "readme": "KEEP PRIVATE_KEY SECRET! PubKey is for config.toml."
    });

    fs::write(json_path, serde_json::to_string_pretty(&info)?)?;
    println!("✅ Infos Wallet sauvegardées dans : {}", json_path);

    println!();
    println!("📋 INFORMATION PUBLIQUE (à mettre dans config.toml) :");
    println!("coordinator_public_key = \"{}\"", pub_hex);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keygen_runs_without_panic() {
        // Vérifie simplement que la génération ne plante pas
        // (on ne vérifie pas la sortie stdout dans ce test)
        run_keygen();
    }
}
