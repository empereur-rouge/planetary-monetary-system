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
use pms_storage::DagStorage;

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
            eprintln!();
            eprintln!("  The JSON file should have format:");
            eprintln!("  {{");
            eprintln!("    \"wallets\": [\"8e1addr1...\", \"8e1addr2...\"],");
            eprintln!("    \"signature\": \"\"");
            eprintln!("  }}");
            eprintln!();
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
        println!();

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
            println!();
            println!("   ⚠️  MNEMONIC (SAVE THIS SECURELY):");
            println!("   ────────────────────────────────────");
            println!("   {}", mnemonic);
            println!();

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
        println!();
        println!("📋 SUMMARY - SAVE THESE MNEMONICS!");
        println!("{}", mnemonics_output);
        println!();
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

    // Commande pour générer un wallet avec mnemonic (output JSON)
    if args.len() > 1 && args[1] == "wallet-generate" {
        let prefix = args.get(2).map(|s| s.as_str()).unwrap_or("pms");
        let wallet = pms_wallet::Wallet::generate();
        let address = wallet.get_address(prefix);
        let mnemonic = wallet.mnemonic_words.clone().unwrap_or_default().join(" ");

        // Get private key as hex
        let priv_b64 = &wallet.private_key_b64;
        let priv_bytes =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, priv_b64)
                .unwrap_or_default();
        let priv_hex = hex::encode(&priv_bytes);

        let output = serde_json::json!({
            "private_key_hex": priv_hex,
            "private_key_b64": wallet.private_key_b64,
            "public_key_hex": wallet.public_key_hex,
            "x25519_pub_hex": wallet.x25519_pub_hex,
            "address": address,
            "mnemonic": mnemonic
        });
        println!("{}", serde_json::to_string_pretty(&output).unwrap());
        return Ok(());
    }

    // Commande utilitaire pour dériver l'adresse wallet depuis une clé privée hex
    if args.len() > 1 && args[1] == "key-to-wallet" {
        if args.len() != 3 {
            eprintln!("Usage: tools-cli key-to-wallet <private_key_hex>");
            std::process::exit(1);
        }
        let priv_hex = &args[2];
        let wallet = pms_wallet::Wallet::from_hex(priv_hex)
            .map_err(|e| anyhow::anyhow!("Invalid key: {}", e))?;
        println!("{}", wallet.get_address("pms"));
        return Ok(());
    }

    // Commande utilitaire pour obtenir la PubKey HEX (04...) depuis une clé privée
    if args.len() > 1 && args[1] == "priv-to-pub" {
        if args.len() != 3 {
            eprintln!("Usage: tools-cli priv-to-pub <private_key_hex>");
            std::process::exit(1);
        }
        let priv_hex = &args[2];

        // Note: pms-node uses compressed keys (03/02...) by default via to_sec1_bytes()
        // So we must output compressed for the Registry to match the Block signer.
        let priv_bytes =
            hex::decode(priv_hex).map_err(|e| anyhow::anyhow!("Invalid hex: {}", e))?;
        let signing_key = k256::ecdsa::SigningKey::from_slice(&priv_bytes)
            .map_err(|e| anyhow::anyhow!("Invalid key: {}", e))?;
        let verify_key = signing_key.verifying_key();
        println!(
            "{}",
            hex::encode(verify_key.to_encoded_point(true).as_bytes())
        );
        return Ok(());
    }

    // Commande pour afficher les versions (DAG, schema, logicielle)
    // Ouvre RocksDB en mode secondary (lecture seule, ne bloque pas le serveur)
    if args.len() > 1 && args[1] == "version" {
        let settings = pms_config::load_config()?;
        let tip_limit = pms_core::MAX_TIPS_CAP;
        let secondary_dir = format!("{}/cli-version-view", &settings.rocks.path);

        println!("PMS Version Info");
        println!("  Software:  {}", env!("CARGO_PKG_VERSION"));
        println!(
            "  Protocol:  {}",
            settings.network.protocol_version
        );
        println!("  Network:   {}", settings.network.network_id);
        println!("  Mode:      {:?}", settings.network.mode);
        println!();

        let store_res = pms_storage::rocks_store::store::RocksStore::open_secondary(
            &settings.rocks.path,
            &secondary_dir,
            tip_limit,
            &settings.rocks.prefix,
        )
        .await;

        match store_res {
            Ok(store) => {
                let dag_version = store.get_dag_version().await.unwrap_or_else(|_| "1.0.0".into());
                let schema_version = store.get_version().await.unwrap_or(0);
                let block_count = store.block_count().await.unwrap_or(0);

                println!("Ledger (prefix '{}'):", settings.rocks.prefix);
                println!("  DAG version:    {}", dag_version);
                println!("  Schema version: {}", schema_version);
                println!("  Blocks:         {}", block_count);
            }
            Err(e) => {
                eprintln!("Could not open RocksDB ({}): {}", settings.rocks.path, e);
                eprintln!("DAG/Schema versions unavailable (DB not found or locked).");
            }
        }

        return Ok(());
    }

    // Commande headless pour envoyer des tokens sans interaction (pour scripts)
    if args.len() > 1 && args[1] == "tx" {
        if args.len() < 5 || args.len() > 6 {
            eprintln!("Usage: tools-cli tx <priv_key_hex> <dest_addr> <amount> [fee]");
            std::process::exit(1);
        }
        let priv_hex = &args[2];
        let dest_addr = &args[3];
        let amount_str = &args[4];
        let fee_str = args.get(5).map(|s| s.as_str()).unwrap_or("0");

        // Bootstrapping minimal pour DAG/Store en mode secondaire
        let settings = pms_config::load_config()?;
        let tip_limit = pms_core::MAX_TIPS_CAP;
        let secondary_dir = format!("{}/cli-tx-view", &settings.rocks.path);

        eprintln!(
            "🔌 CLI (Headless) -> RocksDB path='{}'",
            settings.rocks.path
        );

        let store_res = pms_storage::rocks_store::store::RocksStore::open_secondary(
            &settings.rocks.path,
            &secondary_dir,
            tip_limit,
            &settings.rocks.prefix,
        )
        .await;

        let (store, dag) = match store_res {
            Ok(s) => {
                let store = std::sync::Arc::new(s);
                let dag = pms_core::ConcurrentDag::bootstrap_from_store::<
                    pms_storage::rocks_store::store::RocksStore,
                >(&*store)
                .await?;
                (store, std::sync::Arc::new(dag))
            }
            Err(e) => {
                eprintln!("❌ Failed to open RocksDB: {}", e);
                std::process::exit(1);
            }
        };

        crate::block_submission::action_send_tokens_headless(
            &dag, &store, priv_hex, dest_addr, amount_str, fee_str,
        )
        .await?;

        return Ok(());
    }

    // Commande headless pour mint (pour scripts)
    if args.len() > 1 && args[1] == "mint" {
        if args.len() != 4 {
            eprintln!("Usage: tools-cli mint <priv_key_hex> <amount>");
            std::process::exit(1);
        }
        let priv_hex = &args[2];
        let amount_str = &args[3];

        // Bootstrapping minimal pour DAG/Store en mode secondaire
        let settings = pms_config::load_config()?;
        let tip_limit = pms_core::MAX_TIPS_CAP;
        let secondary_dir = format!("{}/cli-mint-view", &settings.rocks.path);

        eprintln!(
            "🔌 CLI (Headless Mint) -> RocksDB path='{}'",
            settings.rocks.path
        );

        let store_res = pms_storage::rocks_store::store::RocksStore::open_secondary(
            &settings.rocks.path,
            &secondary_dir,
            tip_limit,
            &settings.rocks.prefix,
        )
        .await;

        let (store, dag) = match store_res {
            Ok(s) => {
                let store = std::sync::Arc::new(s);
                let dag = pms_core::ConcurrentDag::bootstrap_from_store::<
                    pms_storage::rocks_store::store::RocksStore,
                >(&*store)
                .await?;
                (store, std::sync::Arc::new(dag))
            }
            Err(e) => {
                eprintln!("❌ Failed to open RocksDB: {}", e);
                std::process::exit(1);
            }
        };

        crate::block_submission::action_make_mint_headless(&dag, &store, priv_hex, amount_str)
            .await?;

        return Ok(());
    }

    // Commande pour chiffrer une clé coordinator existante (AES-256-GCM + argon2id)
    // — voir pms-wallet::key_encryption et audit finding H-key (v0.7.4).
    if args.len() > 1 && args[1] == "encrypt-coordinator-key" {
        if args.len() != 4 {
            eprintln!(
                "Usage: tools-cli encrypt-coordinator-key <in_plain_key_path> <out_enc_path>"
            );
            eprintln!();
            eprintln!("  in_plain_key_path : 64-hex coordinator key (same format as gen-coordinator output)");
            eprintln!("  out_enc_path      : destination JSON envelope (e.g. /opt/pms/etc/pms/node.key.enc)");
            eprintln!();
            eprintln!("  Passphrase source, in order:");
            eprintln!("    1. env var PMS_COORDINATOR_KEY_PASSPHRASE");
            eprintln!("    2. interactive prompt (stdin, no echo when stdin is a TTY)");
            std::process::exit(1);
        }
        let in_path = &args[2];
        let out_path = &args[3];

        // Read the plain key file — same parsing as Wallet::load_from_node_key_file
        let raw = std::fs::read(in_path)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", in_path))?;
        let trimmed = String::from_utf8(raw.clone())
            .unwrap_or_default()
            .trim()
            .to_string();
        let priv_bytes: [u8; 32] = if trimmed.len() == 64 {
            let v = hex::decode(&trimmed)
                .map_err(|e| anyhow::anyhow!("parse hex key: {e}"))?;
            v.as_slice()
                .try_into()
                .map_err(|_| anyhow::anyhow!("decoded key must be 32 bytes"))?
        } else if raw.len() == 32 {
            let mut a = [0u8; 32];
            a.copy_from_slice(&raw);
            a
        } else {
            anyhow::bail!(
                "unsupported plain key format (expected 64-hex or 32-raw-bytes, got {} bytes)",
                raw.len()
            );
        };

        // Passphrase: env var first, then interactive prompt (double entry for confirm).
        let mut passphrase = match std::env::var("PMS_COORDINATOR_KEY_PASSPHRASE") {
            Ok(p) if !p.is_empty() => {
                eprintln!("[encrypt] using passphrase from PMS_COORDINATOR_KEY_PASSPHRASE");
                p
            }
            _ => {
                let p1 = rpassword::prompt_password("Passphrase (min 12 chars): ")
                    .map_err(|e| anyhow::anyhow!("read passphrase: {e}"))?;
                if p1.len() < 12 {
                    anyhow::bail!("passphrase must be at least 12 chars");
                }
                let p2 = rpassword::prompt_password("Confirm passphrase: ")
                    .map_err(|e| anyhow::anyhow!("read passphrase: {e}"))?;
                if p1 != p2 {
                    anyhow::bail!("passphrases do not match");
                }
                p1
            }
        };

        let env = pms_wallet::key_encryption::encrypt_key(&priv_bytes, &mut passphrase)
            .map_err(|e| anyhow::anyhow!("encrypt: {e}"))?;

        let json = serde_json::to_string_pretty(&env)?;
        std::fs::write(out_path, json)
            .map_err(|e| anyhow::anyhow!("write {}: {e}", out_path))?;

        // 0600 on unix — same hardening as gen-coordinator
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(out_path, std::fs::Permissions::from_mode(0o600));
        }

        println!("✅ Wrote encrypted coordinator key to {}", out_path);
        println!();
        println!("Deployment checklist:");
        println!("  1. Move {} out of source control. Keep an offline backup.", in_path);
        println!("  2. On the server, set PMS_COORDINATOR_KEY_PASSPHRASE via systemd EnvironmentFile,");
        println!("     Docker secret, or a sourced shell script — never inline in config.toml.");
        println!("  3. Update [secrets].node_identity_key_encrypted_path = \"{}\" in config.", out_path);
        println!("  4. When the engine boots, it will prefer the encrypted file over the plain one.");
        return Ok(());
    }

    // Forge a CoordinatorKeyRotate block offline and print the signed
    // WireBlock JSON to stdout (or write it to --out). The operator
    // submits it to the running engine via the existing
    // `POST /v1/submit/block` endpoint. Audit item 8 (v0.7.4).
    if args.len() > 1 && args[1] == "rotate-coordinator-key" {
        return run_rotate_coordinator_key(&args[2..]);
    }

    // Interactive REPL fallback
    repl::run().await
}

/// Forge a `CoordinatorKeyRotate` payload, sign it with the current
/// coordinator key, and print the resulting `WireBlock` JSON.
///
/// Args (positional + named, parsed by hand to avoid pulling clap into a
/// sub-binary that's mostly a REPL):
///   tools-cli rotate-coordinator-key
///       --old-key <path>          plain 64-hex coordinator key (current)
///       --new-key <path>          plain 64-hex new coordinator key
///       --parent <block_id>       parent tip to attach the rotation block to
///       [--grace <seconds>]       grace window for old_pk; default 60
///       [--network-id <id>]       wire network id (default "pms-mainnet-v1")
///       [--protocol-version <n>]  wire protocol version (default 1)
///       [--out <file>]            write JSON here (default: stdout)
fn run_rotate_coordinator_key(args: &[String]) -> Result<()> {
    use pms_types::{PayloadEnvelope, PlainPayload};
    use pms_utils::compute_block_id;
    use pms_wire::WireBlock;
    use std::collections::HashMap;

    // Tiny named-arg parser — keeps the dependency surface flat.
    let mut named: HashMap<String, String> = HashMap::new();
    let mut i = 0usize;
    while i < args.len() {
        let key = &args[i];
        if !key.starts_with("--") {
            anyhow::bail!("expected --flag, got {}", key);
        }
        let value = args
            .get(i + 1)
            .ok_or_else(|| anyhow::anyhow!("flag {} missing value", key))?;
        named.insert(key.trim_start_matches("--").to_string(), value.clone());
        i += 2;
    }

    let old_key_path = named
        .get("old-key")
        .ok_or_else(|| anyhow::anyhow!("missing --old-key"))?
        .clone();
    let new_key_path = named
        .get("new-key")
        .ok_or_else(|| anyhow::anyhow!("missing --new-key"))?
        .clone();
    let parent = named
        .get("parent")
        .ok_or_else(|| anyhow::anyhow!("missing --parent"))?
        .clone();
    let grace_window_seconds: u64 = named
        .get("grace")
        .map(|s| s.parse::<u64>())
        .transpose()
        .map_err(|e| anyhow::anyhow!("--grace must be u64: {e}"))?
        .unwrap_or(60);
    let network_id = named
        .get("network-id")
        .cloned()
        .unwrap_or_else(|| "pms-mainnet-v1".into());
    let protocol_version: u16 = named
        .get("protocol-version")
        .map(|s| s.parse::<u16>())
        .transpose()
        .map_err(|e| anyhow::anyhow!("--protocol-version must be u16: {e}"))?
        .unwrap_or(1);
    let out_path = named.get("out").cloned();

    // Helper: read 64-hex plain key file, return a Wallet.
    let load_wallet = |path: &str| -> Result<pms_wallet::Wallet> {
        pms_wallet::Wallet::load_from_node_key_file(path)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", path))
    };

    let old_wallet = load_wallet(&old_key_path)?;
    let new_wallet = load_wallet(&new_key_path)?;
    let old_pk = old_wallet.encoded_public_key();
    let new_pk = new_wallet.encoded_public_key();
    if old_pk.eq_ignore_ascii_case(&new_pk) {
        anyhow::bail!(
            "old and new keys derive the same public key — refusing to forge a no-op rotation"
        );
    }

    let payload = PlainPayload::CoordinatorKeyRotate {
        old_pk: old_pk.clone(),
        new_pk: new_pk.clone(),
        grace_window_seconds,
    };
    let envelope = PayloadEnvelope::Plain(payload);

    // Mirror the block construction the API does for any signed plain
    // block: parents[parent], nonce 0, payload-then-id, sign with the
    // OLD key (current coordinator).
    let parents = vec![parent.clone()];
    let nonce: u64 = 0;
    let id = compute_block_id(&parents, &Some(envelope.clone()), nonce);

    let payload_json = serde_json::to_string(&envelope)
        .map_err(|e| anyhow::anyhow!("serialize payload: {e}"))?;

    let mut wire = WireBlock {
        id: id.clone(),
        parents: parents.clone(),
        payload_json: Some(payload_json),
        nonce,
        network_id,
        protocol_version,
        signer_pk_hex: old_pk.clone(),
        signature_hex: String::new(),
        metadata: None,
    };
    let canonical = pms_wallet::signing_wire::canonical_wireblock_message(&wire);
    use pms_wallet::SignerBackend;
    wire.signature_hex = old_wallet
        .sign(&canonical)
        .map_err(|e| anyhow::anyhow!("sign: {e}"))?;

    let json = serde_json::to_string_pretty(&wire)
        .map_err(|e| anyhow::anyhow!("serialize wire block: {e}"))?;
    if let Some(p) = out_path {
        std::fs::write(&p, &json).map_err(|e| anyhow::anyhow!("write {}: {e}", p))?;
        eprintln!("✅ Wrote signed CoordinatorKeyRotate block to {p}");
    } else {
        println!("{json}");
    }
    eprintln!();
    eprintln!("Submit it to the running engine with:");
    eprintln!(
        "  curl -sS -X POST -H 'Content-Type: application/json' \\\n       --data-binary @- <SUBMIT_URL>/v1/submit/block <<<'{json}'"
    );
    eprintln!();
    eprintln!("Block ID: {id}");
    eprintln!("Old PK  : {old_pk}");
    eprintln!("New PK  : {new_pk}");
    eprintln!("Grace   : {grace_window_seconds}s");
    Ok(())
}
