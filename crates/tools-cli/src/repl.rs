use crate::block_submission::{action_make_mint, action_send_tokens};
use crate::helpers::wait_enter;
use crate::history_actions::{
    action_encrypted_history, action_stream_blocks, action_wallet_history,
};
use crate::wallet_actions::{
    action_create_wallet, action_list_wallets, action_select_wallet, action_show_current,
    action_wallet_balance,
};
use crate::wallet_mnemonic::{action_wallet_export_mnemonic, action_wallet_import_mnemonic};
use anyhow::Result;
use dialoguer::{Input, Select, theme::ColorfulTheme};
use owo_colors::OwoColorize;
use pms_config::load_config;
use pms_core::dag::Dag;
use pms_core::{CoreAdapter, MAX_TIPS_CAP};
use pms_interface::NetDagAdapter;
use pms_storage::DagStorage;
use pms_storage::rocks_store::store::RocksStore;
use pms_types_block::Block;
use pms_utils::{compute_block_id, print_block_full, submit_block_http};
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::{WireBlock, WireMeta};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, sleep};
use pms_wallet::signing_wire::canonical_wireblock_message;

/// Alias pratique
pub type DagRef = Arc<Mutex<Dag>>;
pub type StoreRef = Arc<dyn DagStorage>;

pub struct CliState {
    pub wallets: Vec<Wallet>,
    pub current: Option<usize>,
}

impl CliState {
    pub fn new() -> Self {
        Self {
            wallets: Vec::new(),
            current: None,
        }
    }
    pub fn current_wallet(&self) -> Option<&Wallet> {
        self.current.and_then(|i| self.wallets.get(i))
    }
}

pub async fn run() -> Result<()> {
    let state: Arc<Mutex<CliState>> = Arc::new(Mutex::new(CliState::new()));
    let settings = load_config()?;
    let mode = settings.network.mode;

    let path = &settings.rocks.path;
    let prefix = &settings.rocks.prefix;
    let tip_limit = MAX_TIPS_CAP;
    let secondary_dir = format!("{}/cli-view", &settings.rocks.path);

    eprintln!("🔌 CLI -> RocksDB path='{}' ns='{}'", path, prefix);
    // Pour lecture seule pendant que le nœud tourne :
    let store = Arc::new(
        RocksStore::open_secondary(
            &settings.rocks.path,
            &secondary_dir,
            tip_limit,
            &settings.rocks.prefix,
        )
        .await?,
    );

    // DAG
    let dag = match Dag::bootstrap_from_store::<RocksStore>(&*store).await {
        Ok(d) if !d.blocks.is_empty() => d,
        _ => {
            eprintln!(
                "🟡 DB vide → création du genesis (mémoire, persist via submit/block côté nœud)."
            );
            Dag::new_with_genesis(Block::genesis(compute_block_id))
        }
    };
    let dag = Arc::new(Mutex::new(dag));

    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone());

    // ---- Menu robuste (labels + enum) ----
    #[derive(Clone, Copy)]
    enum Action {
        Status,
        Tips,
        ReloadDag,
        WalletCreate,
        WalletList,
        WalletSelect,
        WalletShow,
        ListIds,
        ListRange,
        MakeMint,
        MakeTx,
        MineAndSubmit,
        BroadcastLast,
        WalletExportMnemonic,
        WalletImportMnemonic,
        StreamBlocks,
        EncryptedHistory,
        WalletHistory,
        WalletBalance,
        Quit,
    }

    let mut menu: Vec<(&'static str, Action)> = vec![
        ("1. Statut DAG", Action::Status),
        ("2. Lister les tips", Action::Tips),
    ];

    if mode.is_non_prod() {
        // dev/testnet uniquement
        menu.push(("3. Miner & soumettre au nœud", Action::MineAndSubmit));
        menu.push(("11. Créer un Mint (encrypté)", Action::MakeMint));
        menu.push(("12. Créer une Transaction (encryptée)", Action::MakeTx));
        menu.push(("4. Diffuser le dernier bloc", Action::BroadcastLast));
        menu.push(("7. Wallet: créer", Action::WalletCreate));
        menu.push(("8. Wallet: lister", Action::WalletList));
        menu.push(("9. Wallet: sélectionner", Action::WalletSelect));
        menu.push(("10. Wallet: courant", Action::WalletShow));
        menu.push((
            "13. Wallet: exporter mnemonic",
            Action::WalletExportMnemonic,
        ));
        menu.push((
            "14. Wallet: importer par mnemonic",
            Action::WalletImportMnemonic,
        ));
    }

    // commun (lecture seule)
    menu.extend_from_slice(&[
        ("5. Lister tous les IDs", Action::ListIds),
        ("6. Recharger le DAG", Action::ReloadDag),
        ("3. Lister blocs 0..n", Action::ListRange),
        ("7. Stream: derniers blocs", Action::StreamBlocks),
        ("8. History chiffré (page)", Action::EncryptedHistory),
        ("15. Wallet: historique (décrypté)", Action::WalletHistory),
        ("16. Wallet: balance", Action::WalletBalance),
        ("0. Quitter", Action::Quit),
    ]);

    let labels: Vec<&str> = menu.iter().map(|(label, _)| *label).collect();

    println!("\n{}", "PMS CLI".bright_blue().bold());
    if mode.is_prod() {
        println!(
            "{}",
            "Mode production: actions sensibles désactivées (minage/diffusion locale).".yellow()
        );
    } else {
        println!(
            "{}",
            "Mode non-prod: toutes les actions de test sont disponibles.".green()
        );
    }

    loop {
        let idx = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Choisis une action")
            .items(&labels)
            .default(0)
            .interact()?;

        let action = menu[idx].1;

        macro_rules! run_result {
            ($fut:expr) => {{
                if let Err(e) = $fut.await {
                    eprintln!("{} {}", "❌ Action échouée:".red().bold(), e);
                    wait_enter();
                }
            }};
        }

        macro_rules! run_unit {
            ($fut:expr) => {{
                $fut.await;
            }};
        }

        match action {
            Action::Status => run_unit!(action_status(&dag)),
            Action::Tips => run_unit!(action_list_tips(&dag)),

            Action::MineAndSubmit => {
                if mode.is_prod() {
                    eprintln!("{}", "⛔ Désactivé en production.".red().bold());
                } else {
                    // ta fonction existante (ne doit jamais paniquer).
                    action_mine_block(&state, &dag, &store, &adapter).await;
                }
            }
            Action::ListIds => run_unit!(action_list_ids(&store)),
            Action::ReloadDag => run_unit!(action_reload_dag(&dag, &store)),
            Action::WalletCreate => {
                run_result!(action_create_wallet(&state, &settings.address.hrp))
            }
            Action::WalletList => run_result!(action_list_wallets(&state, &settings.address.hrp)),
            Action::WalletSelect => {
                run_result!(action_select_wallet(&state, &settings.address.hrp))
            }
            Action::WalletShow => run_result!(action_show_current(&state, &settings.address.hrp)),
            Action::MakeMint => run_result!(action_make_mint(&state, &dag, &store, &adapter)),
            Action::MakeTx => run_result!(action_send_tokens(&state, &dag, &store)),

            Action::ListRange => {
                // paramètres interactifs
                let start: usize = Input::with_theme(&ColorfulTheme::default())
                    .with_prompt("Offset (start)")
                    .default(0)
                    .interact_text()?;
                let count: usize = Input::with_theme(&ColorfulTheme::default())
                    .with_prompt("Combien de blocs")
                    .default(10)
                    .interact_text()?;
                action_list_blocks(&dag, start, count).await;
            }

            Action::StreamBlocks => run_result!(action_stream_blocks(&store)),
            Action::EncryptedHistory => run_result!(action_encrypted_history(&store)),
            Action::WalletHistory => run_result!(action_wallet_history(&state, &store)),
            Action::WalletBalance => run_result!(action_wallet_balance(&state, &store)),

            Action::BroadcastLast => {}
            Action::WalletExportMnemonic => {
                run_result!(action_wallet_export_mnemonic(&state, &settings.address.hrp))
            }
            Action::WalletImportMnemonic => {
                run_result!(action_wallet_import_mnemonic(&state, &settings.address.hrp))
            }

            Action::Quit => {
                println!("{}", "👋 Bye".bright_black());
                break;
            }
        }

        sleep(Duration::from_millis(120)).await;
    }

    Ok(())
}

// Action 1: Statut DAG (déjà OK)
pub async fn action_status(dag: &DagRef) {
    let d = dag.lock().await;
    let tips = d.find_tips();
    println!(
        "{}",
        format!("Blocs: {} | Tips: {}", d.blocks.len(), tips.len()).green()
    );
}

// Action 2: Lister tips (déjà OK)
pub async fn action_list_tips(dag: &DagRef) {
    let mut tips = dag.lock().await.find_tips();
    tips.sort();
    println!("{}", format!("Tips ({}): {:?}", tips.len(), tips).cyan());
}

// Action 3: Miner bloc vide — ne jamais crash
pub async fn action_mine_block(
    state: &Arc<Mutex<CliState>>,
    dag: &DagRef,
    _store: &Arc<RocksStore>,          // pas utilisé ici
    _adapter: &Arc<dyn NetDagAdapter>, // on passe par HTTP comme un vrai client
) -> anyhow::Result<()> {
    // 0) Wallet courant (comme dans action_make_mint)
    let w = match state.lock().await.current_wallet() {
        Some(w) => w.clone(),
        None => {
            eprintln!("{}", "Aucun wallet sélectionné".red());
            wait_enter();
            return Ok(());
        }
    };

    // 1) Config + meta réseau
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    // difficulté en fonction du mode
    let difficulty = match settings.network.mode {
        pms_config::NetworkMode::Dev => 0,
        pms_config::NetworkMode::Testnet => 1,
        pms_config::NetworkMode::Mainnet => 2,
    };

    // 2) Miner localement dans le DAG du CLI (sans persist)
    let mined = {
        let mut d = dag.lock().await;
        match d.add_payload_auto_parents_mined(
            None,               // pas de payload pour ce test
            difficulty,
            compute_block_id,
        ) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("❌ Minage RAM: {e:#}");
                wait_enter();
                return Ok(());
            }
        }
    };

    println!(
        "[CLI][MINE] id={} parents={:?} nonce={}",
        mined.id, mined.parents, mined.nonce
    );

    // 3) WireBlock unsigned
    let mut wb = WireBlock {
        id: mined.id.clone(),
        parents: mined.parents.clone(),
        payload_json: serde_json::to_string(&mined.payload).ok(),
        nonce: mined.nonce,

        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: w.encoded_public_key(),
        signature_hex: String::new(),
    };

    // 4) Message canonique + signature ECDSA via le wallet courant
    let msg = canonical_wireblock_message(&wb);
    let sig_b64 = w
        .sign(&msg)
        .map_err(|e| anyhow::anyhow!("sign error: {e:?}"))?;
    wb.signature_hex = sig_b64;

    // 5) Soumission HTTP vers le nœud (comme un vrai client)
    let (status, id_opt) = submit_block_http(&wb).await?;
    match status {
        s if s == reqwest::StatusCode::CREATED => {
            let id = id_opt.as_deref().unwrap_or(&mined.id);
            println!("✅ Soumis au nœud: 201 Created (id={})", id);
        }
        s if s == reqwest::StatusCode::ACCEPTED => {
            let id = id_opt.as_deref().unwrap_or(&mined.id);
            println!("⚠️  Soumis (async worker): 202 Accepted (id={})", id);
        }
        s if s == reqwest::StatusCode::CONFLICT => {
            println!("ℹ️  Déjà présent: 409 Conflict");
        }
        other => {
            eprintln!("❌ Statut inattendu: {}", other);
        }
    }

    wait_enter();
    Ok(())
}

// Action 4: Diffuser dernier bloc — ne jamais crash
pub async fn action_broadcast_last_block(
    dag: &DagRef,
    store: &Arc<StoreRef>,
    adapter: &Arc<dyn NetDagAdapter>,
) {
    // On essaye d'abord via les tips (plus “récent” logique), sinon dernier id du store trié.
    let last_id = {
        let d = dag.lock().await;
        let mut tips = d.find_tips();
        tips.sort();
        tips.pop()
    };

    let last_id = match last_id {
        Some(id) => Some(id),
        None => match store.all_block_ids().await {
            Ok(mut ids) if !ids.is_empty() => {
                ids.sort();
                ids.last().cloned()
            }
            Ok(_) => None,
            Err(e) => {
                eprintln!(
                    "{} {}",
                    "❌ Impossible de lister les IDs du store:".red().bold(),
                    format!("{e:#}").bright_black()
                );
                None
            }
        },
    };

    if let Some(id) = last_id {
        match store.get_block(&id).await {
            Ok(Some(sb)) => {
                let wb = WireBlock {
                    id: sb.id,
                    parents: sb.parents,
                    payload_json: sb.payload_json,
                    nonce: sb.nonce,
                    network_id: sb.network_id,
                    protocol_version: sb.protocol_version,
                    signer_pk_hex: sb.signer_pk_hex,
                    signature_hex: sb.signature_hex,
                };
                if let Err(e) = adapter.broadcast_block(&wb).await {
                    eprintln!(
                        "{} {}",
                        "❌ Diffusion échouée:".red().bold(),
                        format!("{e:#}").bright_black()
                    );
                } else {
                    println!("{}", "📣 Bloc diffusé sur le réseau".yellow());
                }
            }
            Ok(None) => {
                eprintln!("{}", "❌ Bloc introuvable dans le store".red().bold());
            }
            Err(e) => {
                eprintln!(
                    "{} {}",
                    "❌ Lecture du bloc dans le store échouée:".red().bold(),
                    format!("{e:#}").bright_black()
                );
            }
        }
    } else {
        eprintln!("{}", "❌ Aucun bloc disponible à diffuser".red().bold());
    }
}

// Action 5: Lister IDs store — ne jamais crash
pub async fn action_list_ids(store: &Arc<RocksStore>) {
    match store.all_block_ids().await {
        Ok(mut ids) => {
            ids.sort();
            println!("{}", format!("IDs ({}): {:?}", ids.len(), ids).blue());
        }
        Err(e) => {
            eprintln!(
                "{} {}",
                "❌ Impossible de lister les IDs:".red().bold(),
                format!("{e:#}").bright_black()
            );
        }
    }
}

// Action 6: Reload DAG — ne jamais crash
pub async fn action_reload_dag<S>(dag: &DagRef, store: &Arc<S>)
where
    S: DagStorage + Send + Sync + 'static,
{
    let new = Dag::bootstrap_from_store::<S>(&**store)
        .await
        .unwrap_or_else(|e| {
            eprintln!(
                "{} {}",
                "⚠️  Store vide/invalide, recréation genesis:"
                    .yellow()
                    .bold(),
                format!("{e:#}").bright_black()
            );
            Dag::new_with_genesis(Block::genesis(compute_block_id))
        });

    *dag.lock().await = new;
    println!("{}", "🔄 DAG rechargé depuis le store".green());
}

pub async fn action_list_blocks(dag: &DagRef, start: usize, count: usize) {
    let d = dag.lock().await;

    let mut ids: Vec<String> = d.blocks.keys().cloned().collect();
    ids.sort();

    if ids.is_empty() {
        println!("{}", "Aucun bloc".bright_black());
        return;
    }
    if start >= ids.len() {
        println!("{} {}", "Offset hors borne. Taille DAG =".red(), ids.len());
        return;
    }

    let end = start.saturating_add(count).min(ids.len());
    println!(
        "{}",
        format!("Blocs [{}..{}) / total {}", start, end, ids.len())
            .bright_blue()
            .bold()
    );

    for id in &ids[start..end] {
        if let Some(b) = d.blocks.get(id) {
            println!(
                "{}",
                "---------------- BLOCK ----------------".bright_black()
            );
            print_block_full(b);
        }
    }
}
