// NOTE: Ces allow sont temporaires pour le pré-déploiement.
// Les warnings peuvent être refactorisés après le déploiement.
#![allow(clippy::collapsible_if)]
#![allow(clippy::needless_borrows_for_generic_args)]
#![allow(clippy::needless_borrow)]
#![allow(clippy::borrow_deref_ref)]
#![allow(clippy::unnecessary_to_owned)]
#![allow(unused_imports)]
#![allow(unused_variables)]
extern crate core;

use anyhow::Result;

mod block_submission;
mod helpers;
mod history_actions;
pub mod keygen;
mod repl;
mod utils;
mod wallet_actions;
mod wallet_mnemonic;

#[tokio::main]
async fn main() -> Result<()> {
    // Simple argument parsing to support scripting
    let args: Vec<String> = std::env::args().collect();

    if args.len() > 1 && args[1] == "gen-coordinator" {
        // Simple manual parsing to support optional flag anywhere
        let mut clean_args: Vec<String> = Vec::new();
        let mut force = false;

        for arg in &args {
            if arg == "--force" {
                force = true;
            } else {
                clean_args.push(arg.clone());
            }
        }

        if clean_args.len() < 4 || clean_args.len() > 5 {
            eprintln!(
                "Usage: tools-cli gen-coordinator <key_file> <json_file> [config_file] [--force]"
            );
            eprintln!(
                "  config_file: optionnel, met à jour coordinator_public_key automatiquement"
            );
            eprintln!("  --force: écrase les fichiers existants sans confirmation");
            std::process::exit(1);
        }
        let key_path = &clean_args[2];
        let json_path = &clean_args[3];
        let config_path = clean_args.get(4).map(|s| s.as_str());
        keygen::generate_and_save(key_path, json_path, config_path, force)?;
        return Ok(());
    }

    // Commande pour dériver les clés coordinateur d'une clé privée existante et mettre à jour le config
    if args.len() > 1 && args[1] == "derive-coordinator" {
        if args.len() != 4 {
            eprintln!("Usage: tools-cli derive-coordinator <private_key_hex> <config_file>");
            eprintln!(
                "  Dérive les clés publiques (secp256k1 et x25519) et met à jour le fichier de config."
            );
            std::process::exit(1);
        }
        let priv_hex = &args[2];
        let config_path = &args[3];
        keygen::derive_and_update_config(priv_hex, config_path)?;
        return Ok(());
    }

    // Commande pour vérifier si ce nœud est le coordinateur
    if args.len() > 1 && args[1] == "check-coordinator" {
        if args.len() != 4 {
            eprintln!("Usage: tools-cli check-coordinator <key_file> <config_file>");
            eprintln!("  Vérifie si la clé privée correspond au coordinator_public_key du config");
            std::process::exit(1);
        }
        let key_path = &args[2];
        let config_path = &args[3];
        keygen::check_is_coordinator(key_path, config_path)?;
        return Ok(());
    }

    // Commande pour signer la liste des wallets treasury
    if args.len() > 1 && args[1] == "treasury-sign" {
        if args.len() != 4 {
            eprintln!("Usage: tools-cli treasury-sign <coordinator_key_file> <treasury_json_file>");
            eprintln!("  Signs the treasury wallets list with the coordinator's private key");
            eprintln!("");
            eprintln!("  The JSON file should have format:");
            eprintln!("  {{");
            eprintln!("    \"wallets\": [\"8e1addr1...\", \"8e1addr2...\"],");
            eprintln!("    \"signature\": \"\"");
            eprintln!("  }}");
            eprintln!("");
            eprintln!("  The signature field will be updated with the coordinator's signature.");
            std::process::exit(1);
        }
        let key_path = &args[2];
        let json_path = &args[3];

        // Load coordinator private key
        let key_hex = std::fs::read_to_string(key_path)
            .map_err(|e| anyhow::anyhow!("Failed to read key file: {}", e))?
            .trim()
            .to_string();

        // Load treasury JSON
        let json_content = std::fs::read_to_string(json_path)
            .map_err(|e| anyhow::anyhow!("Failed to read treasury JSON: {}", e))?;

        let mut treasury: serde_json::Value = serde_json::from_str(&json_content)
            .map_err(|e| anyhow::anyhow!("Invalid JSON: {}", e))?;

        // Extract wallets
        let wallets: Vec<String> = treasury["wallets"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Missing 'wallets' array in JSON"))?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();

        if wallets.is_empty() {
            eprintln!("Error: No wallets found in JSON");
            std::process::exit(1);
        }

        // Sign
        let signature = pms_config::sign_treasury_wallets(&wallets, &key_hex)
            .map_err(|e| anyhow::anyhow!("Signing failed: {}", e))?;

        // Update JSON
        treasury["signature"] = serde_json::Value::String(signature.clone());

        // Write back
        let updated_json = serde_json::to_string_pretty(&treasury)?;
        std::fs::write(json_path, updated_json)?;

        println!("✅ Treasury wallets signed successfully!");
        println!("   Wallets: {:?}", wallets);
        println!("   Signature: {}...", &signature[..32.min(signature.len())]);
        println!("   File updated: {}", json_path);

        return Ok(());
    }

    // Commande pour générer des wallets treasury avec de vraies adresses PMS
    if args.len() > 1 && args[1] == "treasury-generate" {
        let num_wallets: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3);
        let output_dir = args
            .get(3)
            .map(|s| s.as_str())
            .unwrap_or("etc/pms/treasury-keys");
        let json_path = args
            .get(4)
            .map(|s| s.as_str())
            .unwrap_or("etc/pms/treasury-wallets.json");

        println!("═══════════════════════════════════════════════════════════════");
        println!("🏦 Treasury Wallet Generator (Real PMS Addresses)");
        println!("═══════════════════════════════════════════════════════════════");
        println!("");

        // Create output directory
        std::fs::create_dir_all(output_dir)?;

        let mut addresses = Vec::new();
        let mut mnemonics_output = String::new();

        for i in 1..=num_wallets {
            let wallet = pms_wallet::Wallet::generate();
            let address = wallet.get_address("8e");
            let mnemonic_words = wallet.mnemonic_words.clone().unwrap_or_default();
            let mnemonic = mnemonic_words.join(" ");

            // Save individual wallet
            let wallet_json = serde_json::json!({
                "private_key_b64": wallet.private_key_b64,
                "public_key_hex": wallet.public_key_hex,
                "x25519_pub_hex": wallet.x25519_pub_hex,
                "address": address,
                "mnemonic": mnemonic
            });

            let wallet_path = format!("{}/treasury-{}.json", output_dir, i);
            std::fs::write(&wallet_path, serde_json::to_string_pretty(&wallet_json)?)?;

            // Secure permissions on Unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&wallet_path, std::fs::Permissions::from_mode(0o600)).ok();
            }

            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
            println!("📍 Treasury Wallet #{}", i);
            println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
            println!("   Address: {}", address);
            println!("   Saved to: {}", wallet_path);
            println!("");
            println!("   ⚠️  MNEMONIC (SAVE THIS SECURELY):");
            println!("   ────────────────────────────────────");
            println!("   {}", mnemonic);
            println!("");

            mnemonics_output.push_str(&format!(
                "\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n\
                Treasury Wallet #{}\n\
                Address: {}\n\
                Mnemonic: {}\n\
                ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n",
                i, address, mnemonic
            ));

            addresses.push(address);
        }

        // Create treasury-wallets.json
        let treasury_json = serde_json::json!({
            "wallets": addresses,
            "signature": ""
        });

        std::fs::write(json_path, serde_json::to_string_pretty(&treasury_json)?)?;

        println!("═══════════════════════════════════════════════════════════════");
        println!("📝 Treasury JSON created: {}", json_path);
        println!("   Total wallets: {}", addresses.len());
        println!("═══════════════════════════════════════════════════════════════");
        println!("");
        println!("📋 SUMMARY - SAVE THESE MNEMONICS!");
        println!("{}", mnemonics_output);
        println!("");
        println!("⚡ NEXT STEPS:");
        println!("1. Save the mnemonics above in a SECURE location");
        println!("2. Sign the treasury list:");
        println!(
            "   cargo run -p tools-cli -- treasury-sign <coordinator.key> {}",
            json_path
        );
        println!("3. Distribute {} to all nodes", json_path);

        return Ok(());
    }

    repl::run().await
}
