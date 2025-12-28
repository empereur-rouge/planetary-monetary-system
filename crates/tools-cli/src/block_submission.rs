use std::sync::Arc;
use dialoguer::{Confirm, Input, Select};
use dialoguer::theme::ColorfulTheme;
use owo_colors::OwoColorize;
use tokio::sync::Mutex;
use pms_types::{EncryptedPayload, OutputId, PayloadEnvelope, PlainPayload, Transaction, TxInput, TxOutput, Unlock};
use crate::helpers::{pubkey_of_local_addr, wait_enter};
use anyhow::{Result};
use serde_json::json;
use pms_interface::NetDagAdapter;
use pms_storage::RedisStore;
use pms_wallet::SignerBackend;
use crate::repl::{CliState, DagRef};

async fn maybe_submit(mined: &pms_types_block::Block) -> Result<()> {
    if !Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Soumettre au nœud maintenant ?")
        .default(true)
        .interact()?
    {
        return Ok(());
    }

    let node = std::env::var("ORACLE_URL")
        .ok()
        .unwrap_or_else(|| "http://127.0.0.1:8080".into());

    let wb = pms_wire::WireBlock {
        id: mined.id.clone(),
        parents: mined.parents.clone(),
        payload_json: serde_json::to_string(&mined.payload).ok(),
        nonce: mined.nonce,
    };

    let resp = reqwest::Client::new()
        .post(format!("{}/submit/block", node))
        .json(&wb)
        .send()
        .await?;

    println!("📣 Soumission: {}", resp.status());
    Ok(())
}

pub async fn action_make_reward(
    state: &Arc<Mutex<CliState>>,
    dag: &DagRef,
    _store: &Arc<RedisStore>,
    adapter: &Arc<dyn NetDagAdapter>,
) -> Result<()> {
    // bénéficiaire = wallet courant
    let w = {
        let st = state.lock().await;
        match st.current_wallet() {
            Some(w) => w.clone(),
            None => {
                eprintln!("{}", "Aucun wallet sélectionné".red());
                wait_enter();
                return Ok(());
            }
        }
    };

    // montant
    let amount: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Montant (string décimale)")
        .default("1000".into())
        .interact_text()?;

    // payload clair
    let plain = PlainPayload::Reward {
        outputs: vec![TxOutput { address: w.get_address(), amount: amount.clone() }],
    };

    // destinataires chiffrement
    let admin_pk_hex = std::env::var("PMS_ADMIN_PUBKEY_HEX").unwrap_or_else(|_| {
        Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Admin public key hex")
            .interact_text()
            .unwrap()
    });
    let recipients = vec![w.encoded_public_key(), admin_pk_hex];

    // chiffrement
    let enc = match EncryptedPayload::encrypt_for_plain(&plain, &recipients) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{} {}", "❌ Chiffrement échoué:".red(), e);
            wait_enter();
            return Ok(());
        }
    };

    // minage RAM
    let mined = {
        let mut d = dag.lock().await;
        match d.add_payload_auto_parents_mined(
            Some(PayloadEnvelope::Encrypted(enc)),
            0,
            pms_utils::compute_block_id,
        ) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{} {}", "❌ Minage échoué:".red(), e);
                wait_enter();
                return Ok(());
            }
        }
    };

    println!(
        "{} {}  {} {}  {} {}",
        "✅ Reward miné:".green().bold(),
        mined.id.yellow(),
        "dest:".bright_black(),
        w.get_address().cyan(),
        "amount:".bright_black(),
        amount
    );

    // wire
    let wb = pms_wire::WireBlock {
        id: mined.id.clone(),
        parents: mined.parents.clone(),
        payload_json: serde_json::to_string(&mined.payload).ok(),
        nonce: mined.nonce,
    };

    // persist + broadcast
    match adapter.persist_block(&wb).await {
        Ok(pms_storage::PutResult::Inserted) => {
            println!("{}", "✅ Persisté dans le store".green());
            match adapter.broadcast_block(&wb).await {
                Ok(_)  => println!("{}", "📣 Bloc broadcasté".yellow()),
                Err(e) => eprintln!("{} {}", "❌ Broadcast échoué:".red(), e),
            }
        }
        Ok(pms_storage::PutResult::AlreadyExists) => {
            println!("{}", "ℹ️ Bloc déjà présent (dup)".bright_black());
        }
        Ok(other) => {
            eprintln!("{} {:?}", "❌ PutResult inattendu:".red(), other);
        }
        Err(e) => {
            eprintln!("{} {}", "❌ Persistance échouée:".red(), e);
        }
    }

    wait_enter();
    Ok(())
}

// TX MAKING
pub async fn action_make_tx(
    state: &Arc<Mutex<CliState>>,
    dag: &DagRef,
    store: &Arc<RedisStore>,
    adapter: &Arc<dyn NetDagAdapter>,
) -> Result<()> {
    // --- sélection expéditeur + destinataire (inchangé) ---
    let (from_w, to_addr) = {
        let st = state.lock().await;
        if st.wallets.is_empty() { anyhow::bail!("Crée d'abord un wallet"); }
        let items: Vec<String> = st.wallets.iter().map(|w| w.get_address()).collect();
        drop(st);

        let from_idx = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Wallet expéditeur").items(&items).default(0).interact()?;

        let dest_mode = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Choisir le destinataire")
            .items(&["Adresse saisie", "Parmi mes wallets"]).default(0).interact()?;

        let to_addr = if dest_mode==1 {
            let st2 = state.lock().await;
            let def = if st2.wallets.len()>1 { 1 } else { 0 };
            let idx = Select::with_theme(&ColorfulTheme::default())
                .with_prompt("Wallet destinataire")
                .items(&items).default(def).interact()?;
            st2.wallets[idx].get_address()
        } else {
            Input::with_theme(&ColorfulTheme::default())
                .with_prompt("Adresse destinataire").interact_text()?
        };

        let st3 = state.lock().await;
        (st3.wallets[from_idx].clone(), to_addr)
    };

    // --- sélection UTXO (ici on demande un input simple; remplace par auto-UTXO si dispo) ---
    let prev_txid: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Input: txid").interact_text()?;
    let prev_index: u32 = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Input: index").default(0).interact_text()?;

    let amount_out: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Montant à envoyer (string décimale)").default("100".into()).interact_text()?;

    // calcule la fee automatiquement si tu as une FeePolicy accessible ; ici on laisse le prompt optionnel
    let fee: String = {
        // remarque: remplace par FeePolicy::compute_fee(&amount_out) si tu as la policy ici.
        Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Frais (string décimale) — tu peux appuyer Entrée pour auto")
            .allow_empty(true)
            .interact_text()?
    };
    let fee = if fee.trim().is_empty() {
        // fallback minimal si pas de policy: "0"
        "0".to_string()
    } else {
        fee
    };

    // --- construit la tx, signe ---
    let inputs = vec![TxInput { out: OutputId { txid: prev_txid.clone(), index: prev_index } }];
    let outputs = vec![TxOutput { address: to_addr.clone(), amount: amount_out.clone() }];
    let canon = json!({ "inputs": inputs, "outputs": outputs, "fee": fee });

    let canon_str = serde_json::to_string(&canon)?;
    let sig_b64 = from_w
        .sign(&canon_str)
        .map_err(|e| anyhow::anyhow!("signature failed: {:?}", e))?;

    let unlocks = vec![Unlock {
        pubkey_hex: from_w.encoded_public_key(),
        signature_b64: sig_b64,
    }];

    let tx = Transaction { inputs, outputs, fee: fee.clone(), unlocks };
    let plain = PlainPayload::TxUtxo(tx);

    // --- destinataires pour le chiffrement ---
    let admin_pk_hex = std::env::var("PMS_ADMIN_PUBKEY_HEX")
        .unwrap_or_else(|_| Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Admin public key hex").interact_text().unwrap());

    let mut recipients = vec![from_w.encoded_public_key(), admin_pk_hex];
    if let Some(pk) = pubkey_of_local_addr(state, &to_addr).await {
        recipients.push(pk);
    } else if Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Ajouter la pubkey hex du destinataire pour qu'il puisse déchiffrer ?")
        .default(true).interact()?
    {
        let to_pk_hex: String = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Dest pubkey hex").interact_text()?;
        recipients.push(to_pk_hex);
    }

    // --- chiffrement ---
    let enc = EncryptedPayload::encrypt_for_plain(&plain, &recipients)
        .map_err(|e| anyhow::anyhow!("encrypt failed: {e}"))?;

    // --- minage RAM -> bloc complet ---
    let mined = {
        let mut d = dag.lock().await;
        d.add_payload_auto_parents_mined(
            Some(PayloadEnvelope::Encrypted(enc)),
            0,
            pms_utils::compute_block_id,
        )?
    };

    println!("{} {}", "✅ Tx minée (RAM):".green().bold(), mined.id.yellow());

    // --- transforme en WireBlock ---
    let wb = pms_wire::WireBlock {
        id: mined.id.clone(),
        parents: mined.parents.clone(),
        payload_json: serde_json::to_string(&mined.payload).ok(),
        nonce: mined.nonce,
    };

    // --- persist localement via l'adapter/store ---
    match adapter.persist_block(&wb).await {
        Ok(pms_storage::PutResult::Inserted) => {
            println!("{}", "✅ Persisté dans le store".green());
            // broadcast si persist ok
            match adapter.broadcast_block(&wb).await {
                Ok(_) => println!("{}", "📣 Bloc broadcasté".yellow()),
                Err(e) => eprintln!("{} {}", "❌ Erreur broadcast:".red(), e),
            }
        }
        Ok(pms_storage::PutResult::AlreadyExists) => {
            println!("{}", "ℹ️  Bloc déjà présent (dup)".bright_black());
        }
        Err(e) => {
            eprintln!("{} {}", "❌ Persist failed:".red(), e);
        }
    }

    Ok(())
}