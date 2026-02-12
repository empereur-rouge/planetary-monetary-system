use crate::helpers::wait_enter;
use crate::repl::{CliState, DagRef};
use crate::utils::sync::sync_after_submit;
use anyhow::Result;
use dialoguer::Input;
use dialoguer::theme::ColorfulTheme;
use owo_colors::OwoColorize;
use pms_config::{NetworkMode, load_config};
use pms_core::{ConcurrentDag, to_wire};
use pms_interface::NetDagAdapter;
use pms_storage::DagStorage;
use pms_storage::rocks_store::store::RocksStore;
use pms_types::{
    EncryptedPayload, OutputId, PayloadEnvelope, PlainPayload, Transaction, TxInput, TxOutput,
    Unlock,
};
use pms_types_block::Block;
use pms_utils::{compute_block_id, submit_block_http};
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wallet::utxo_store::{gather_wallet_utxos_dec, select_utxos_dec};
use pms_wallet::{SignerBackend, Wallet, decode_address, make_address};
use rust_decimal::Decimal;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Forge un bloc (sans modifier le DAG) + affiche un log standardisé.
/// `kind` sert juste au log ("Mint", "Tx", …).
pub async fn forge_block_with(
    dag: &DagRef,
    payload: Option<PayloadEnvelope>,
    _label: &str,
) -> anyhow::Result<Block> {
    let settings = load_config()?;
    let difficulty = match settings.network.mode {
        NetworkMode::Dev => 0,
        NetworkMode::Testnet => 1,
        NetworkMode::Mainnet => 2, // à ajuster
    };

    let b = dag.forge_block(payload, difficulty, compute_block_id)?;
    Ok(b)
}

pub async fn reload_dag_after_submit(dag: &Arc<ConcurrentDag>, store: &Arc<RocksStore>) {
    // Reload logic for ConcurrentDag (skipped or clear-and-insert)
    // For now we assume no-op or just log warning as in repl.rs
    // To truely reload, we'd need to clear 'dag' and re-feed it.
    // dag.blocks.clear(); ...
    // let fresh = ConcurrentDag::bootstrap_from_store(store).await...
    // But since 'dag' is Arc, we can't replace it easily in caller unless RwLock.
    // We'll leave it as no-op/log for now.
    println!(
        "{}",
        "⚠️  Reload DAG inplace not supported with ConcurrentDag yet.".yellow()
    );
}

/// Forge un bloc avec un payload chiffré, signe le WireBlock avec le `wallet`,
/// l'envoie au nœud HTTP, puis:
///   - synchronise le store secondaire (refresh_from_primary + sync_after_submit)
///   - recharge le DAG local
///
/// Paramètres:
/// - `dag`: DAG en RAM (Arc<Mutex<Dag>>), utilisé pour choisir les parents + forger le bloc.
/// - `store`: RocksStore local (peut être secondaire).
/// - `wallet`: wallet secp256k1 qui signe le bloc (signer_pk_hex + signature).
/// - `payload`: payload déjà prêt.
/// - `label`: juste un tag lisible pour les logs ("Mint", "TxUtxo", etc.).
/// - `network_id`, `protocol_version`: issus de ta config (settings.network.*).
async fn submit_block_from_cli(
    dag: &DagRef,
    store: &Arc<RocksStore>,
    wallet: &Wallet,
    payload: PayloadEnvelope,
    label: &str,
    network_id: &str,
    protocol_version: u16,
) -> anyhow::Result<()> {
    // 1) Forge le bloc en RAM:
    //    - choisit les parents dans le DAG
    //    - applique éventuellement du PoW (ici difficulté=0 en pratique)
    //    - ne persiste rien en local, c’est juste une “maquette” de bloc.
    let forged = forge_block_with(dag, Some(payload), label).await?;
    println!(
        "[CLI][FORGED][{}] id={} parents={:?} nonce={}",
        label, forged.id, forged.parents, forged.nonce
    );

    // 2) Conversion Block → WireBlock:
    //    - on sérialise le payload en JSON
    //    - on garde id/parents/nonce
    //    - les champs réseau + signature sont ajoutés juste après.
    let mut wb = to_wire(&forged);

    // 3) Injecte les métadonnées réseau (ce que le nœud attend pour filtrer).
    wb.network_id = network_id.to_string();
    wb.protocol_version = protocol_version;

    // 4) Clé publique du signataire (secp256k1, encodée en hex)
    wb.signer_pk_hex = wallet.encoded_public_key();

    // 5) Message canonique:
    //    On prend *tous* les champs structurants (id, parents, payload_json,
    //    nonce, network_id, protocol_version, signer_pk_hex) et on génère
    //    une string *canonique* (ordre stable, sans champs inutiles).
    //    C’est ce message exact qui est signé et vérifié côté nœud.
    let msg = canonical_wireblock_message(&wb);

    // 6) Signature ECDSA via ton Wallet:
    //    - `wallet.sign(&msg)` produit une signature base64.
    //    - cette signature sera vérifiée avec `verify_block_signature`
    //      dans `persist_block`.
    let sig_b64 = wallet
        .sign(&msg)
        .map_err(|e| anyhow::anyhow!("sign error: {e:?}"))?;
    wb.signature_hex = sig_b64;

    // 7) Envoi HTTP au nœud:
    //    - `/submit/block` va:
    //        * vérifier réseau (network_id/protocol_version)
    //        * vérifier la signature ECDSA
    //        * persister dans Rocks
    //        * mettre à jour DAG + finalité côté serveur
    println!(
        "[CLI][HTTP][OUT][{}] POST /submit/block id={} parents={:?}",
        label, wb.id, wb.parents
    );
    let (status, id_opt) = submit_block_http(&wb).await?;

    // 8) Si tu as un store secondaire, on essaye de le resynchroniser:
    if let Err(e) = store.refresh_from_primary() {
        eprintln!("[CLI][REFRESH_BEFORE][ERR] {e:#}");
    } else {
        let cnt = store.all_block_ids().await.map(|v| v.len()).unwrap_or(0);
        println!("[CLI][REFRESH_BEFORE][OK] store_count={}", cnt);
    }

    // 9) Sync fine (en fonction du status + id retourné par le nœud)
    sync_after_submit(store, id_opt.as_deref(), status).await?;

    // 10) Rechargement complet du DAG local depuis le store:
    //     - toutes les nouvelles arêtes/enfants/finalités sont reflétées dans le CLI.
    reload_dag_after_submit(dag, store).await;
    {
        let tips = dag.find_tips();
        println!(
            "[CLI][AFTER_RELOAD][{}] blocks_ram={}, tips_ram={:?}",
            label,
            dag.len(),
            tips
        );
    }

    // 11) Feedback lisible pour l’utilisateur final
    match status {
        s if s == reqwest::StatusCode::CREATED => {
            let id = id_opt.as_deref().unwrap_or(&wb.id);
            println!("✅ [{}] 201 Created (id={})", label, id);
        }
        s if s == reqwest::StatusCode::ACCEPTED => {
            let id = id_opt.as_deref().unwrap_or(&wb.id);
            println!("⚠️  [{}] 202 Accepted (async, id={})", label, id);
        }
        s if s == reqwest::StatusCode::CONFLICT => {
            println!("ℹ️  [{}] 409 Conflict (déjà présent)", label);
        }
        other => {
            eprintln!("❌ [{}] Statut inattendu: {}", label, other);
        }
    }

    Ok(())
}

pub async fn action_make_mint(
    state: &Arc<Mutex<CliState>>,
    dag: &DagRef,
    store: &Arc<RocksStore>,
    _adapter: &Arc<dyn NetDagAdapter>,
) -> anyhow::Result<()> {
    // 1) Wallet courant
    let w = match state.lock().await.current_wallet() {
        Some(w) => w.clone(),
        None => {
            eprintln!("{}", "Aucun wallet sélectionné".red());
            wait_enter();
            return Ok(());
        }
    };

    // 2) Config (HRP + réseau + admin)
    let settings = load_config()?;
    let hrp = settings.address.hrp.clone();
    // let admin_xpk = pick_admin_recipient(&settings.admin.wallet_addresses).await?; // Unused since Mint is transparent

    // 3) Montant
    let amount: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Montant (string décimale)")
        .default("1000".into())
        .interact_text()?;

    // 4) Payload Mint → TRANSPARENT (pas de chiffrement)
    let plain = PlainPayload::Mint {
        outputs: vec![TxOutput {
            address: w.get_address(&hrp),
            amount: amount.clone(),
            asset_id: None,
        }],
    };

    // Modification: on passe directement en Plain
    submit_block_from_cli(
        dag,
        store,
        &w,
        PayloadEnvelope::Plain(plain),
        "Mint",
        &settings.network.network_id,
        settings.network.protocol_version as u16,
    )
    .await?;

    wait_enter();
    Ok(())
}

pub async fn action_send_tokens(
    state: &Arc<Mutex<CliState>>,
    dag: &DagRef,
    store: &Arc<RocksStore>,
) -> Result<()> {
    // 1) Wallet courant + HRP + X25519 SK + settings
    let (w, w_pub, w_xpk, w_xsk, hrp, settings) = {
        let st = state.lock().await;
        let wallet = st
            .current_wallet()
            .ok_or_else(|| anyhow::anyhow!("Aucun wallet sélectionné"))?;
        let settings = load_config()?;
        let hrp = settings.address.hrp.clone();
        let xsk = wallet
            .x25519_sk_hex()
            .ok_or_else(|| anyhow::anyhow!("Wallet sans mnemonic → pas de X25519 SK"))?;
        (
            wallet.clone(),
            wallet.public_key_hex.clone(),
            wallet.x25519_pub_hex.clone(),
            xsk,
            hrp,
            settings,
        )
    };

    // 2) Saisie destinataire + montant + frais
    let dest_addr: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Adresse destinataire (Bech32m)")
        .interact_text()?;

    let amount_str: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Montant (string décimale)")
        .interact_text()?;

    let fee_str: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Frais (string décimale) — Entrée pour 0")
        .default("0".into())
        .interact_text()?;

    // 3) Parse montants
    let want =
        Decimal::from_str_exact(&amount_str).map_err(|_| anyhow::anyhow!("Montant invalide"))?;
    let fee = Decimal::from_str_exact(&fee_str).map_err(|_| anyhow::anyhow!("Frais invalides"))?;

    // 4) UTXO du wallet
    let utxos = gather_wallet_utxos_dec(&*store, &w_pub, &w_xpk, &w_xsk, &hrp, 5_000).await?;
    if utxos.is_empty() {
        eprintln!("{}", "Aucun UTXO disponible".red());
        wait_enter();
        return Ok(());
    }

    // 5) Sélection gloutonne
    let need = want + fee;
    let (picked, change) = select_utxos_dec(utxos, need)?;

    // 6) Construire la transaction (outputs: destinataire [+ change])
    let mut outputs = vec![TxOutput {
        address: dest_addr.clone(),
        amount: amount_str.clone(),
        asset_id: None,
    }];
    if change > Decimal::ZERO {
        let change_addr = make_address(&hrp, &w_pub, &w_xpk);
        outputs.push(TxOutput {
            address: change_addr,
            amount: change.to_string(),
            asset_id: None,
        });
    }

    let inputs: Vec<TxInput> = picked
        .into_iter()
        .map(|u| TxInput {
            out: OutputId {
                txid: u.txid,
                index: u.index,
            },
        })
        .collect();

    let mut tx = Transaction {
        inputs,
        outputs,
        fee: fee_str.clone(),
        unlocks: vec![],
    };

    // 7) Signature de la Tx UTXO (niveau “transaction”)
    {
        let msg = tx.signing_message()?;
        let sig_b64 = w
            .sign(&msg)
            .map_err(|_| anyhow::anyhow!("Failed to sign transaction"))?;
        tx.unlocks = vec![Unlock {
            pubkey_hex: w.public_key_hex.clone(),
            signature_b64: sig_b64,
        }];
    }

    // 8) Chiffrement du payload (destinataires = nous + destinataire)
    let (_h20, dest_xpk) = decode_address(&dest_addr)
        .map_err(|e| anyhow::anyhow!("Adresse destinataire invalide: {e}"))?;

    let plain = PlainPayload::TxUtxo(tx);
    let enc = EncryptedPayload::encrypt_for_plain(&plain, &[w_xpk.clone(), dest_xpk])
        .map_err(|e| anyhow::anyhow!(e))?;

    // 9) Forge + sign + submit + reload via helper unifié
    submit_block_from_cli(
        dag,
        store,
        &w,
        PayloadEnvelope::Encrypted(enc),
        "TxUtxo",
        &settings.network.network_id,
        settings.network.protocol_version as u16,
    )
    .await?;

    println!(
        "{} {}",
        "✅ Transaction soumise, bloc id:".green().bold(),
        "voir logs ci-dessus".cyan()
    );
    wait_enter();
    Ok(())
}
// ... (existing code)

pub async fn action_send_tokens_headless(
    dag: &DagRef,
    store: &Arc<RocksStore>,
    wallet_private_key: &str,
    dest_addr: &str,
    amount_str: &str,
    fee_str: &str,
) -> Result<()> {
    // 1) Load Config & Wallet
    let settings = load_config()?;
    let hrp = settings.address.hrp.clone();

    let w = Wallet::from_hex(wallet_private_key).map_err(|e| anyhow::anyhow!(e))?;
    let w_pub = w.public_key_hex.clone();
    let w_xpk = w.x25519_pub_hex.clone();
    let w_xsk = w
        .x25519_sk_hex()
        .ok_or_else(|| anyhow::anyhow!("Wallet needs private key"))?;

    // 2) Parse amounts
    let want =
        Decimal::from_str_exact(amount_str).map_err(|_| anyhow::anyhow!("Invalid amount"))?;
    let fee = Decimal::from_str_exact(fee_str).map_err(|_| anyhow::anyhow!("Invalid fee"))?;

    // 3) UTXO Selection
    let utxos = gather_wallet_utxos_dec(&*store, &w_pub, &w_xpk, &w_xsk, &hrp, 5_000).await?;
    if utxos.is_empty() {
        return Err(anyhow::anyhow!("No UTXOs available for this wallet"));
    }

    let need = want + fee;
    let (picked, change) = select_utxos_dec(utxos, need)?;

    // 4) Build Transaction
    let mut outputs = vec![TxOutput {
        address: dest_addr.to_string(),
        amount: amount_str.to_string(),
        asset_id: None,
    }];
    if change > Decimal::ZERO {
        let change_addr = make_address(&hrp, &w_pub, &w_xpk);
        outputs.push(TxOutput {
            address: change_addr,
            amount: change.to_string(),
            asset_id: None,
        });
    }

    let inputs: Vec<TxInput> = picked
        .into_iter()
        .map(|u| TxInput {
            out: OutputId {
                txid: u.txid,
                index: u.index,
            },
        })
        .collect();

    let mut tx = Transaction {
        inputs,
        outputs,
        fee: fee_str.to_string(),
        unlocks: vec![],
    };

    // 5) Sign Transaction
    {
        let msg = tx.signing_message()?;
        let sig_b64 = w
            .sign(&msg)
            .map_err(|_| anyhow::anyhow!("Failed to sign"))?;
        tx.unlocks = vec![Unlock {
            pubkey_hex: w.public_key_hex.clone(),
            signature_b64: sig_b64,
        }];
    }

    // 6) Encrypt Payload
    let (_h20, dest_xpk_decoded) =
        decode_address(dest_addr).map_err(|e| anyhow::anyhow!("Invalid dest addr: {e}"))?;

    let plain = PlainPayload::TxUtxo(tx);
    let enc = EncryptedPayload::encrypt_for_plain(&plain, &[w_xpk.clone(), dest_xpk_decoded])
        .map_err(|e| anyhow::anyhow!(e))?;

    // 7) Submit
    submit_block_from_cli(
        dag,
        store,
        &w,
        PayloadEnvelope::Encrypted(enc),
        "TxUtxo-Headless",
        &settings.network.network_id,
        settings.network.protocol_version as u16,
    )
    .await?;

    println!("✅ Headless Transaction Submitted Successfully");
    Ok(())
}

pub async fn action_make_mint_headless(
    dag: &DagRef,
    store: &Arc<RocksStore>,
    wallet_private_key: &str,
    amount_str: &str,
) -> Result<()> {
    let settings = load_config()?;
    let hrp = settings.address.hrp.clone();

    let w = Wallet::from_hex(wallet_private_key).map_err(|e| anyhow::anyhow!(e))?;

    // Validate amount
    if Decimal::from_str_exact(amount_str).is_err() {
        return Err(anyhow::anyhow!("Invalid amount"));
    }

    let plain = PlainPayload::Mint {
        outputs: vec![TxOutput {
            address: w.get_address(&hrp),
            amount: amount_str.to_string(),
            asset_id: None,
        }],
    };

    submit_block_from_cli(
        dag,
        store,
        &w,
        PayloadEnvelope::Plain(plain),
        "Mint-Headless",
        &settings.network.network_id,
        settings.network.protocol_version as u16,
    )
    .await?;

    println!("✅ Headless Mint Submitted Successfully");
    Ok(())
}
