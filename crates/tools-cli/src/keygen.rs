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
///
/// ## Format de sortie
/// - `key_path` : Clé privée en hex (64 caractères) - format attendu par pms-node
/// - `json_path` : Fichier JSON complet avec les informations du wallet
/// - `config_path` : Optionnel - met à jour automatiquement coordinator_public_key dans le config
pub fn generate_and_save(key_path: &str, json_path: &str, config_path: Option<&str>) -> Result<()> {
    use dialoguer::Confirm;

    println!("🔑 Generation des clés coordinateur...");
    println!();

    // Vérifier si des fichiers existants seront écrasés
    let mut files_to_overwrite = Vec::new();

    // Vérifier node.key
    if Path::new(key_path).exists() {
        if let Ok(content) = fs::read_to_string(key_path) {
            if !content.trim().is_empty() {
                files_to_overwrite.push(format!("🔑 {} (clé privée existante)", key_path));
            }
        }
    }

    // Vérifier admin-wallet.json
    if Path::new(json_path).exists() {
        if let Ok(content) = fs::read_to_string(json_path) {
            // Considérer non-vide si ce n'est pas juste "{}" ou vide
            let trimmed = content.trim();
            if !trimmed.is_empty() && trimmed != "{}" {
                files_to_overwrite.push(format!("👛 {} (wallet existant)", json_path));
            }
        }
    }

    // Vérifier coordinator_public_key dans le config
    if let Some(cfg_path) = config_path {
        if Path::new(cfg_path).exists() {
            if let Ok(content) = fs::read_to_string(cfg_path) {
                // Chercher coordinator_public_key = "xxx" où xxx n'est pas vide
                let re = regex::Regex::new(r#"coordinator_public_key\s*=\s*"([^"]*)""#).ok();
                if let Some(re) = re {
                    if let Some(caps) = re.captures(&content) {
                        if let Some(key) = caps.get(1) {
                            if !key.as_str().is_empty() {
                                files_to_overwrite.push(format!(
                                    "📋 {} (coordinator_public_key déjà défini)",
                                    cfg_path
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    // Demander confirmation si des fichiers seront écrasés
    if !files_to_overwrite.is_empty() {
        println!("⚠️  ATTENTION: Les fichiers suivants seront écrasés:");
        for f in &files_to_overwrite {
            println!("   {}", f);
        }
        println!();

        let confirm = Confirm::new()
            .with_prompt("Voulez-vous continuer et écraser ces fichiers ?")
            .default(false)
            .interact()
            .unwrap_or(false);

        if !confirm {
            println!("❌ Génération annulée.");
            return Ok(());
        }
        println!();
    }
    let wallet = Wallet::generate();

    // 2. La clé privée dans le wallet est en base64, on la convertit en hex
    //    Le nœud attend 64 caractères hex (32 bytes en hex)
    let priv_b64 = wallet.encoded_private_key();
    let priv_bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &priv_b64)
        .map_err(|e| anyhow::anyhow!("Failed to decode base64: {}", e))?;

    let priv_hex = hex::encode(&priv_bytes);
    let pub_hex = wallet.encoded_public_key();
    let address = wallet.get_address("8e"); // HRP par défaut

    // 3. Sauvegarder la clé privée en HEX (64 chars, pas de newline)
    //    C'est le format attendu par pms-node
    fs::write(key_path, &priv_hex)?;
    println!(
        "✅ Clé privée sauvegardée dans : {} ({} chars hex)",
        key_path,
        priv_hex.len()
    );

    // 4. Sauvegarder le JSON (pour l'admin)
    let info = json!({
        "private_key": priv_hex,
        "public_key": pub_hex,
        "address": address,
        "type": "coordinator",
        "readme": "KEEP PRIVATE_KEY SECRET! PubKey is for config.toml."
    });

    fs::write(json_path, serde_json::to_string_pretty(&info)?)?;
    println!("✅ Infos Wallet sauvegardées dans : {}", json_path);

    // 5. Optionnel: mettre à jour le fichier config avec la clé publique
    if let Some(cfg_path) = config_path {
        update_config_coordinator_key(cfg_path, &pub_hex)?;
        println!("✅ Config mise à jour : {}", cfg_path);
    }

    println!();
    println!("📋 INFORMATION PUBLIQUE :");
    println!("coordinator_public_key = \"{}\"", pub_hex);

    Ok(())
}

/// Met à jour la valeur de coordinator_public_key dans un fichier TOML
fn update_config_coordinator_key(config_path: &str, public_key: &str) -> Result<()> {
    let content = fs::read_to_string(config_path)
        .map_err(|e| anyhow::anyhow!("Impossible de lire {}: {}", config_path, e))?;

    // Utiliser une regex pour remplacer la valeur de coordinator_public_key
    // Pattern: coordinator_public_key = "..." (avec guillemets)
    let re = regex::Regex::new(r#"coordinator_public_key\s*=\s*"[^"]*""#)
        .map_err(|e| anyhow::anyhow!("Regex error: {}", e))?;

    let new_content = re
        .replace(
            &content,
            format!("coordinator_public_key = \"{}\"", public_key),
        )
        .to_string();

    fs::write(config_path, new_content)
        .map_err(|e| anyhow::anyhow!("Impossible d'écrire {}: {}", config_path, e))?;

    Ok(())
}
