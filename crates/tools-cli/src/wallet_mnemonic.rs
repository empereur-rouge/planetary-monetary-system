use anyhow::Result;
use dialoguer::{Input, Confirm, theme::ColorfulTheme, Select};
use owo_colors::OwoColorize;
use std::fs;
use std::sync::Arc;
use serde_json::Value;
use tokio::sync::Mutex;
use pms_wallet::SignerBackend;
use crate::helpers::{default_export_path, in_docker, read_stdin_all};
use crate::repl::CliState;

pub async fn action_wallet_export_mnemonic(state: &Arc<Mutex<CliState>>, hrp: &str) -> anyhow::Result<()> {
    // --- copie des données nécessaires hors du lock ---
    let (address, pub_hex, words): (String, String, Vec<String>) = {
        let st = state.lock().await;
        let w = match st.current_wallet() {
            Some(w) => w,
            None => { eprintln!("{}", "Aucun wallet sélectionné".red()); return Ok(()); }
        };
        let words = match w.mnemonic_words() {
            Some(v) => v.into_iter().map(|s| s.to_string()).collect(),
            None => { eprintln!("{}", "Ce wallet n’a pas de mnemonic".red()); return Ok(()); }
        };
        (w.get_address(hrp), w.encoded_public_key(), words)
    }; // lock libéré ici

    println!("{}", "Mnemonic (24 mots):".bright_blue().bold());
    for (i, ww) in words.iter().enumerate() { println!("{:>2}. {}", i+1, ww.yellow()); }

    let how = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Exporter le mnemonic")
        .items(&["Fichier", "STDOUT (copier/coller)"])
        .default(0).interact()?;

    let payload = serde_json::json!({
        "address": address,
        "public_key_hex": pub_hex,
        "mnemonic_words": words,
    });

    match how {
        1 => {
            println!("{}", serde_json::to_string_pretty(&payload)?);
            if in_docker() { println!("{}", "Astuce: redirige > wallet.json".bright_black()); }
        }
        _ => {
            let def = default_export_path(&format!("wallet-seed-{}.json", payload["address"].as_str().unwrap_or("wallet")));
            let path: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("Chemin du fichier").default(def).interact_text()?;
            std::fs::write(&path, serde_json::to_string_pretty(&payload)?)?;
            println!("{} {}", "✅ Exporté:".green().bold(), path.cyan());
            if in_docker() && path.starts_with("/exports/") {
                println!("{} {}", "📥 Sur l’hôte:".bright_black(), path.replace("/exports/", "exports/"));
            }
        }
    }
    Ok(())
}

pub async fn action_wallet_import_mnemonic(state: &Arc<Mutex<CliState>>, hrp: &str) -> Result<()> {
    // si STDIN est pipé, on le privilégie
    let preload = read_stdin_all();

    let mode = if preload.is_some() {
        1 // STDIN
    } else {
        Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Source du mnemonic")
            .items(&["Depuis un fichier JSON", "Depuis STDIN", "Saisir les 24 mots"])
            .default(0).interact()?
    };

    let words_vec: Vec<String> = match mode {
        0 => { // fichier
            let def = if in_docker() { "/exports/wallet-seed.json".to_string() } else { "wallet-seed.json".to_string() };
            let path: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("Chemin du fichier JSON").default(def).interact_text()?;
            let txt = fs::read_to_string(&path)?;

            // si c’est un fichier wallet complet, tente un import direct
            if txt.contains("\"private_key_b64\"") && txt.contains("\"public_key_hex\"") {
                match pms_wallet::Wallet::load_from_file(&path) {
                    Ok(w) => {
                        let mut st = state.lock().await;
                        let idx = st.wallets.len();
                        st.wallets.push(w);
                        if st.current.is_none() { st.current = Some(idx); }
                        println!("{}", "✅ Wallet importé depuis fichier wallet".green().bold());
                        return Ok(());
                    }
                    Err(e) => { eprintln!("{} {}", "Import wallet JSON échoué:".red(), e); return Ok(()); }
                }
            }

            let v: Value = serde_json::from_str(&txt)?;
            if let Some(arr) = v.get("mnemonic_words").and_then(|x| x.as_array()) {
                arr.iter().map(|x| x.as_str().unwrap_or_default().to_string()).collect()
            } else if let Some(s) = v.get("mnemonic").and_then(|x| x.as_str()) {
                s.split_whitespace().map(|w| w.to_string()).collect()
            } else {
                anyhow::bail!("JSON inconnu: fournissez `mnemonic_words` ou `mnemonic`");
            }
        }
        1 => { // STDIN
            let txt = preload.unwrap_or_else(|| {
                println!("{}", "STDIN vide. Exemple: cat seed.json | tools-cli".red());
                String::new()
            });
            if txt.is_empty() { return Ok(()); }
            let v: Value = serde_json::from_str(&txt)?;
            if let Some(arr) = v.get("mnemonic_words").and_then(|x| x.as_array()) {
                arr.iter().map(|x| x.as_str().unwrap_or_default().to_string()).collect()
            } else if let Some(s) = v.get("mnemonic").and_then(|x| x.as_str()) {
                s.split_whitespace().map(|w| w.to_string()).collect()
            } else {
                anyhow::bail!("STDIN JSON inconnu: `mnemonic_words` ou `mnemonic` requis");
            }
        }
        _ => { // saisie
            let phrase: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("Saisis les 24 mots (séparés par des espaces)").interact_text()?;
            phrase.split_whitespace().map(|w| w.to_string()).collect()
        }
    };

    if words_vec.len() != 24 {
        eprintln!("{} {}", "Il faut exactement 24 mots. Reçu:".red(), words_vec.len());
        return Ok(());
    }

    let words_ref: Vec<&str> = words_vec.iter().map(|s| s.as_str()).collect();
    let w = match pms_wallet::Wallet::from_word_list(&words_ref) {
        Ok(w) => w,
        Err(e) => { eprintln!("{} {}", "Mnemonic invalide:".red(), e); return Ok(()); }
    };

    {
        let mut st = state.lock().await;
        let idx = st.wallets.len();
        st.wallets.push(w);
        if st.current.is_none() { st.current = Some(idx); }
        println!("{}", "✅ Wallet importé et ajouté à la liste".green().bold());
        println!("Adresse  : {}", st.wallets[idx].get_address(&hrp).cyan());
        println!("Public   : {}", st.wallets[idx].encoded_public_key().yellow());
    }

    // option sauvegarde du wallet complet
    if Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Sauvegarder le wallet complet en JSON ?")
        .default(false).interact()?
    {
        // clone hors lock
        let (w_clone, def) = {
            let st = state.lock().await;
            let w = st.wallets.last().cloned().unwrap(); // Wallet: Clone
            let def = default_export_path(&format!("wallet-{}.json", w.get_address(&hrp)));
            (w, def)
        };

        let path: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Chemin du fichier").default(def).interact_text()?;

        match w_clone.save_to_file(&path) {
            Ok(_) => {
                println!("{} {}", "💾 Sauvegardé:".green(), path.cyan());
                if in_docker() && path.starts_with("/exports/") {
                    println!("{} {}", "📥 Sur l’hôte:".bright_black(), path.replace("/exports/", "exports/"));
                }
            }
            Err(e) => eprintln!("{} {}", "Échec sauvegarde:".red(), e),
        }
    }

    Ok(())
}