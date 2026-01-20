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
        // We calculate x25519_pub_hex from the wallet
        let x25519_hex = wallet.x25519_pub_hex();
        update_config_coordinator_keys(cfg_path, &pub_hex, &x25519_hex)?;
        println!("✅ Config mise à jour : {}", cfg_path);
    }

    println!();
    println!("📋 INFORMATION PUBLIQUE :");
    println!("coordinator_public_key = \"{}\"", pub_hex);

    Ok(())
}

/// Dérive les clés publiques depuis une clé privée hex et met à jour le config.
pub fn derive_and_update_config(priv_hex: &str, config_path: &str) -> Result<()> {
    // 1. Décode et dérive
    // Wallet::from_private_key n'existe pas, on reconstruit le wallet manuellement
    // On utilise une logique similaire à load_from_node_key_file mais en mémoire
    let priv_bytes = hex::decode(priv_hex).map_err(|e| anyhow::anyhow!("Invalid hex: {}", e))?;

    // On utilise k256 pour dériver la pubkey
    let signing_key = k256::ecdsa::SigningKey::from_slice(&priv_bytes)
        .map_err(|e| anyhow::anyhow!("Invalid ECDSA private key: {}", e))?;
    let verify_key = signing_key.verifying_key();
    let pub_hex = hex::encode(verify_key.to_sec1_bytes());

    // On crée une instance temporaire juste pour dériver x25519
    // On doit encoder la clé privée en base64 pour le constructeur Wallet
    let priv_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &priv_bytes);

    let mut wallet = Wallet {
        private_key_b64: priv_b64,
        public_key_hex: pub_hex.clone(),
        x25519_pub_hex: String::new(),
        mnemonic_words: None,
    };

    // Dériver x25519
    if let Some((_, pk)) = wallet.derive_x25519_pair_from_private_key_b64() {
        wallet.x25519_pub_hex = pk;
    }

    let x25519_hex = wallet.x25519_pub_hex();

    println!("🔑 Clés dérivées :");
    println!("   Secp256k1 : {}", pub_hex);
    println!("   X25519    : {}", x25519_hex);

    // 2. Mise à jour config
    update_config_coordinator_keys(config_path, &pub_hex, &x25519_hex)?;
    println!("✅ Fichier config mis à jour : {}", config_path);

    Ok(())
}

/// Met à jour coordinator_public_key et coordinator_x25519_public_key dans un fichier TOML
fn update_config_coordinator_keys(
    config_path: &str,
    public_key: &str,
    x25519_key: &str,
) -> Result<()> {
    let content = fs::read_to_string(config_path)
        .map_err(|e| anyhow::anyhow!("Impossible de lire {}: {}", config_path, e))?;

    // 1. Update coordinator_public_key
    let re_pub = regex::Regex::new(r#"coordinator_public_key\s*=\s*"[^"]*""#)
        .map_err(|e| anyhow::anyhow!("Regex error: {}", e))?;

    let mut new_content = re_pub
        .replace(
            &content,
            format!("coordinator_public_key = \"{}\"", public_key),
        )
        .to_string();

    // 2. Update coordinator_x25519_public_key
    // Si la ligne existe, on remplace
    if new_content.contains("coordinator_x25519_public_key") {
        let re_x255 = regex::Regex::new(r#"coordinator_x25519_public_key\s*=\s*"[^"]*""#)
            .map_err(|e| anyhow::anyhow!("Regex error: {}", e))?;
        new_content = re_x255
            .replace(
                &new_content,
                format!("coordinator_x25519_public_key = \"{}\"", x25519_key),
            )
            .to_string();
    } else {
        // Sinon on l'ajoute juste après coordinator_public_key
        new_content = new_content.replace(
            &format!("coordinator_public_key = \"{}\"", public_key),
            &format!(
                "coordinator_public_key = \"{}\"\ncoordinator_x25519_public_key = \"{}\"",
                public_key, x25519_key
            ),
        );
    }

    fs::write(config_path, new_content)
        .map_err(|e| anyhow::anyhow!("Impossible d'écrire {}: {}", config_path, e))?;

    Ok(())
}

/// Vérifie si la clé privée du nœud correspond au coordinator_public_key du config.
///
/// ## Explication
/// Pour être coordinateur, la clé publique dérivée de la clé privée du nœud
/// doit correspondre exactement à la `coordinator_public_key` définie dans le config.
///
/// ## Sortie
/// Affiche un message clair indiquant si le nœud est coordinateur ou non.
pub fn check_is_coordinator(key_path: &str, config_path: &str) -> Result<()> {
    use k256::ecdsa::SigningKey;
    use k256::elliptic_curve::sec1::ToEncodedPoint;

    println!("🔍 Vérification du statut coordinateur...");
    println!();

    // 1. Lire la clé privée du nœud
    let priv_hex = fs::read_to_string(key_path)
        .map_err(|e| anyhow::anyhow!("Impossible de lire {}: {}", key_path, e))?;
    let priv_hex = priv_hex.trim();

    if priv_hex.len() != 64 {
        anyhow::bail!(
            "Format de clé invalide dans {}. Attendu: 64 chars hex, trouvé: {}",
            key_path,
            priv_hex.len()
        );
    }

    // 2. Décoder la clé privée hex
    let priv_bytes =
        hex::decode(priv_hex).map_err(|e| anyhow::anyhow!("Clé privée hex invalide: {}", e))?;

    // 3. Créer la clé de signature et dériver la clé publique
    let signing_key = SigningKey::from_slice(&priv_bytes)
        .map_err(|e| anyhow::anyhow!("Clé privée invalide: {}", e))?;
    let verifying_key = signing_key.verifying_key();

    // Clé publique non-compressée (65 bytes, commence par 04)
    let node_pubkey = hex::encode(verifying_key.to_encoded_point(false).as_bytes());

    // 4. Lire le coordinator_public_key du config
    let config_content = fs::read_to_string(config_path)
        .map_err(|e| anyhow::anyhow!("Impossible de lire {}: {}", config_path, e))?;

    let re = regex::Regex::new(r#"coordinator_public_key\s*=\s*"([^"]*)""#)
        .map_err(|e| anyhow::anyhow!("Regex error: {}", e))?;

    let config_pubkey = match re.captures(&config_content) {
        Some(caps) => caps
            .get(1)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default(),
        None => {
            println!(
                "⚠️  Aucun coordinator_public_key trouvé dans {}",
                config_path
            );
            println!("   Ce nœud ne peut pas être coordinateur.");
            return Ok(());
        }
    };

    if config_pubkey.is_empty() {
        println!("⚠️  coordinator_public_key est vide dans {}", config_path);
        println!("   Ce nœud ne peut pas être coordinateur.");
        return Ok(());
    }

    // 5. Comparer les clés
    println!("📋 Clé publique du nœud   : {}", &node_pubkey[..20]);
    println!(
        "📋 Clé coordinateur config: {}",
        &config_pubkey[..20.min(config_pubkey.len())]
    );
    println!();

    if node_pubkey == config_pubkey {
        println!("✅ CE NŒUD EST LE COORDINATEUR !");
        println!("   Il a le pouvoir de minter des tokens.");
    } else {
        println!("❌ Ce nœud N'EST PAS le coordinateur.");
        println!("   Les clés ne correspondent pas.");
    }

    Ok(())
}
