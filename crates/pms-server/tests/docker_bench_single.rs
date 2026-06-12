//! Benchmark Test: Single Docker Node, 10000 Transactions (Parallel Version)
//!
//! ## Architecture IOTA-like
//!
//! Ce test démontre le principe fondamental de l'architecture DAG où chaque
//! transaction crée son propre bloc léger. Pour maximiser le TPS, nous utilisons
//! plusieurs "workers" parallèles, chacun gérant sa propre chaîne d'UTXOs.
//!
//! ## Pourquoi le parallélisme est nécessaire ?
//!
//! Dans un modèle UTXO, chaque transaction dépend des UTXOs de la transaction
//! précédente. Si un seul client envoie des tx séquentiellement :
//!
//! ```text
//!   tx0 → tx1 → tx2 → tx3  (une seule file = TPS limité par latence réseau)
//! ```
//!
//! Avec N workers parallèles, chacun ayant sa propre source d'UTXOs :
//!
//! ```text
//!   Worker 0: tx0 → tx1 → tx2 ... (indépendant)
//!   Worker 1: tx0 → tx1 → tx2 ... (indépendant)
//!   Worker 2: tx0 → tx1 → tx2 ... (indépendant)
//!   ...
//! ```
//!
//! Le TPS total ≈ N × TPS d'un seul worker.
//!
//! ## Usage:
//!   docker compose -f docker-compose.bench.yml up -d --build
//!   RUST_LOG="warn,pms_bench=info" cargo test --release -p pms-server docker_bench_single -- --nocapture
//!   docker compose -f docker-compose.bench.yml down -v

use anyhow::{Context, Result};
use pms_types::{
    Block, OutputId, PayloadEnvelope, PlainPayload, Transaction, TxInput, TxOutput, Unlock,
};
use pms_utils::compute_block_id;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireBlock;
use reqwest::Client;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::time::sleep;

use pms_wallet::signing_wire::canonical_wireblock_message;

use chrono::Local;
use std::path::{Path, PathBuf};
use std::str::FromStr;

struct DockerGuard {
    file: String,
}

impl DockerGuard {
    fn new(file: &str) -> Self {
        // Find correct path for docker-compose file
        let path = if Path::new(file).exists() {
            file.to_string()
        } else if Path::new(&format!("../../{}", file)).exists() {
            format!("../../{}", file)
        } else {
            panic!("Could not find {}", file);
        };

        println!("🐳 Starting Docker environment using {}...", path);

        // Ensure clean state first
        stop_docker(&path);

        let status = std::process::Command::new("docker")
            .arg("compose")
            .arg("-f")
            .arg(&path)
            .arg("up")
            .arg("-d")
            .arg("--build")
            .status()
            .expect("failed to run docker compose up");

        assert!(status.success(), "docker compose up failed");

        // Give it a moment to stabilize network binding
        std::thread::sleep(Duration::from_secs(5));

        Self { file: path }
    }
}

impl Drop for DockerGuard {
    fn drop(&mut self) {
        println!("🛑 Stopping Docker environment...");
        stop_docker(&self.file);
    }
}

fn stop_docker(file: &str) {
    let _ = std::process::Command::new("docker")
        .arg("compose")
        .arg("-f")
        .arg(file)
        .arg("down")
        .arg("-v")
        .status();
}

// ============================================================================
// CONFIGURATION DU BENCHMARK
// ============================================================================

/// Nombre total de transactions à envoyer pendant le benchmark.
/// Objectif: mesurer le TPS maximum du nœud.
const TX_COUNT: usize = 10_000;

/// Nombre de workers parallèles.
///
/// Chaque worker gère sa propre chaîne UTXO indépendante. Plus il y a de workers,
/// plus le parallélisme est élevé, MAIS cela nécessite aussi plus de setup initial
/// (split des fonds) et plus de ressources serveur.
///
/// Note: Trop de workers peut créer de la contention et BAISSER le TPS.
const WORKER_COUNT: usize = 10;

/// Transactions par worker = TX_COUNT / WORKER_COUNT.
///
/// Si TX_COUNT = 10,000 et WORKER_COUNT = 10, chaque worker enverra 1,000 tx.
const TX_PER_WORKER: usize = TX_COUNT / WORKER_COUNT;

/// Montant initial à minter pour le faucet.
///
/// Doit être suffisant pour couvrir toutes les transactions + fees.
/// Calcul: (PAYMENT_AMOUNT + FEE_AMOUNT) × TX_COUNT + marge de sécurité
const INITIAL_MINT: &str = "500000.0";

/// Montant envoyé dans chaque transaction de test.
const PAYMENT_AMOUNT: &str = "1.0";

/// Frais par transaction (vont à l'adresse admin).
/// Inclut le base fee + 3.5% pour simuler un usage réel.
const FEE_AMOUNT: &str = "0.036";

/// URL du nœud Docker à tester.
const NODE_URL: &str = "https://127.0.0.1:8080";

// ============================================================================
// STRUCTURES DE DONNÉES
// ============================================================================

/// Représente une référence vers un UTXO non dépensé.
///
/// ## Ownership en Rust
///
/// Cette struct contient des données "owned" (pas de références/lifetimes).
/// Elle peut donc être clonée, déplacée entre threads, et stockée librement.
///
/// Voir: The Rust Programming Language, Chapitre 4.1 - What Is Ownership?
#[derive(Debug, Clone)]
pub struct OutputRef {
    /// ID de la transaction qui a créé cet output (= le block ID dans notre DAG)
    pub txid: String,
    /// Index de l'output dans la liste des outputs de cette transaction
    pub index: u32,
    /// Montant disponible dans cet UTXO
    pub amount: rust_decimal::Decimal,
}

// ============================================================================
// FONCTION PRINCIPALE DU BENCHMARK
// ============================================================================

#[tokio::test]
#[ignore]
async fn docker_bench_single() -> Result<()> {
    // 0) Start Docker Environment (Automated via Guard)
    let _guard = DockerGuard::new("docker-compose.bench.yml");

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!(
        "║  PMS Parallel Benchmark - {} workers × {} tx/worker   ║",
        WORKER_COUNT, TX_PER_WORKER
    );
    println!("╚══════════════════════════════════════════════════════════════╝");

    // ========================================================================
    // ÉTAPE 1: Construction du client HTTP avec connection pooling
    // ========================================================================
    //
    // Le connection pooling permet de réutiliser les connexions TCP/TLS
    // au lieu de les recréer à chaque requête. C'est CRITIQUE pour les
    // performances car le handshake TLS est coûteux (~100ms).
    //
    // pool_max_idle_per_host: Combien de connexions garder en reserve
    // pool_idle_timeout: Combien de temps garder une connexion inactive
    // ========================================================================
    let client = {
        let cert_pem = std::fs::read("secrets/tls/ca-cert.pem")
            .or_else(|_| std::fs::read("../../secrets/tls/ca-cert.pem"))
            .expect("Failed to read CA cert - ensure secrets/tls/ca-cert.pem exists");
        let cert = reqwest::Certificate::from_pem(&cert_pem)?;

        // Wait for API to be ready with a polling loop
        println!("🔌 Waiting for node API to be ready...");
        let tmp_client = Client::builder()
            .add_root_certificate(cert.clone())
            .danger_accept_invalid_certs(true)
            .build()?;

        let mut ready = false;
        for i in 0..30 {
            if let Ok(resp) = tmp_client.get("https://127.0.0.1:8080/livez").send().await {
                if resp.status().is_success() {
                    println!("✅ Node is ready!");
                    ready = true;
                    break;
                }
            }
            if i % 5 == 0 {
                println!("... waiting for node ({}/30)", i);
            }
            sleep(Duration::from_secs(1)).await;
        }
        if !ready {
            panic!("Node failed to become ready in time");
        }

        Client::builder()
            .add_root_certificate(cert)
            .danger_accept_invalid_certs(false)
            .timeout(Duration::from_secs(30))
            // === CONNECTION POOLING ===
            // Garde WORKER_COUNT × 2 connexions prêtes pour éviter les reconnexions
            .pool_max_idle_per_host(WORKER_COUNT * 2)
            // Garde les connexions 30 secondes après la dernière utilisation
            .pool_idle_timeout(Duration::from_secs(30))
            .build()?
    };

    // ========================================================================
    // ÉTAPE 2: Attente que le nœud soit prêt
    // ========================================================================
    println!("\n🔌 Waiting for node to be ready...");
    let mut attempts = 0;
    loop {
        match client.get(format!("{}/live", NODE_URL)).send().await {
            Ok(resp) if resp.status().is_success() => break,
            _ => {
                attempts += 1;
                if attempts > 60 {
                    panic!("❌ Node unreachable at {} after 60 attempts", NODE_URL);
                }
                sleep(Duration::from_secs(1)).await;
            }
        }
    }
    println!("✅ Node is UP at {}", NODE_URL);

    // ========================================================================
    // ÉTAPE 3: Création des wallets
    // ========================================================================
    //
    // - Faucet: Reçoit le mint initial et distribue aux workers
    //           IMPORTANT: En mode testnet/prod, seul le wallet admin peut minter
    //           On charge donc le wallet depuis etc/pms/admin-wallet.json
    // - Recipient: Destination des paiements de test (on s'en fiche du contenu)
    // - Admin: Reçoit les fees (hardcodée dans le protocole)
    //
    // Note: Chaque worker aura son propre wallet créé plus tard.
    // ========================================================================

    // Charger le wallet admin qui a l'autorisation de minter
    // Le chemin peut être différent selon où le test est lancé
    let admin_wallet_path = if Path::new("etc/pms/admin-wallet.json").exists() {
        "etc/pms/admin-wallet.json"
    } else if Path::new("../../etc/pms/admin-wallet.json").exists() {
        "../../etc/pms/admin-wallet.json"
    } else {
        panic!("Could not find etc/pms/admin-wallet.json - required for minting in testnet mode");
    };
    let faucet =
        Wallet::load_from_file(admin_wallet_path).expect("Failed to load admin wallet for minting");
    let faucet_addr = faucet.get_address("8e");

    let recipient = Wallet::generate();
    let recipient_addr = recipient.get_address("8e");

    // Adresse admin pour les frais (hardcodée dans le protocole)
    let admin_addr = "8e1eqy642zaz5dsyzc3cf54ul642r9259kre2d40m3mp7q8h4fq4333yw0hjkw02e4lljjm4em8hqjc67p3m4esvq774n";

    println!("\n💰 Wallets created:");
    println!("   Faucet (admin):    {}...", &faucet_addr[..20]);
    println!("   Recipient: {}...", &recipient_addr[..20]);

    // ========================================================================
    // ÉTAPE 4: Mint initial de la supply
    // ========================================================================
    //
    // Crée le bloc genesis avec tous les fonds. Ce bloc sera ensuite "splitté"
    // pour alimenter chaque worker avec sa propre reserve d'UTXOs.
    // ========================================================================
    println!("\n📦 Minting {} PMS...", INITIAL_MINT);
    // Utilise genesis deux fois puisqu'il n'y a pas d'autre tip
    let genesis_id = Block::genesis(compute_block_id).id;
    let parents = vec![genesis_id.clone()];
    let mint_id = mine_mint(
        &client,
        NODE_URL,
        &faucet,
        &faucet_addr,
        INITIAL_MINT,
        parents,
    )
    .await?;
    println!("   Mint block: {}", mint_id);

    // Petit délai pour laisser le bloc se propager
    sleep(Duration::from_secs(2)).await;

    // ========================================================================
    // ÉTAPE 5: Split des fonds vers N workers
    // ========================================================================
    //
    // ## Pourquoi ce split est nécessaire ?
    //
    // Chaque worker a besoin de sa propre "chaîne" d'UTXOs pour pouvoir
    // envoyer des transactions indépendamment des autres workers.
    //
    // Si tous les workers partageaient le même UTXO, ils devraient se
    // synchroniser entre eux, ce qui casserait le parallélisme.
    //
    // ## Diagramme:
    //
    // ```
    //              ┌────────────────────┐
    //              │  Mint (500k PMS)   │
    //              └─────────┬──────────┘
    //                        │ Split
    //        ┌───────┬───────┼───────┬───────┐
    //        ▼       ▼       ▼       ▼       ▼
    //     Worker0 Worker1 Worker2   ...   Worker9
    //     (50k)   (50k)   (50k)          (50k)
    // ```
    // ========================================================================
    println!("\n💸 Splitting funds into {} workers...", WORKER_COUNT);

    // Vec pour stocker chaque worker (wallet + son UTXO de départ)
    let mut workers: Vec<(Wallet, OutputRef)> = Vec::with_capacity(WORKER_COUNT);

    // Calcul du montant par worker: INITIAL_MINT / WORKER_COUNT
    let amount_per_worker = rust_decimal::Decimal::from_str(INITIAL_MINT).unwrap()
        / rust_decimal::Decimal::from(WORKER_COUNT as u64);

    // UTXO courant pour le split (commence par le mint, puis le change de chaque split)
    let mut current_split_utxo = OutputRef {
        txid: mint_id.clone(),
        index: 0,
        amount: rust_decimal::Decimal::from_str(INITIAL_MINT).unwrap(),
    };

    // Boucle de création des workers
    // À chaque itération, on crée une tx qui envoie `amount_per_worker` au nouveau
    // worker, et le reste (change) est utilisé pour le prochain split.
    for i in 0..WORKER_COUNT {
        // Générer un nouveau wallet pour ce worker
        let worker_wallet = Wallet::generate();
        let worker_addr = worker_wallet.get_address("8e");

        // Calculer le change restant après ce split
        let remaining = current_split_utxo.amount - amount_per_worker;

        // Créer la transaction de split
        // - Output 0: va au worker (amount_per_worker)
        // - Output 1: change retourne au faucet (remaining)

        // Utiliser [current_tx, genesis] pour avoir 2 parents pendant bootstrap
        let genesis_for_parent = Block::genesis(pms_utils::compute_block_id).id;
        let split_parents = vec![current_split_utxo.txid.clone(), genesis_for_parent];

        let (split_id, _) = send_tx(
            &client,
            NODE_URL,
            &faucet,
            &faucet_addr,
            &current_split_utxo.txid,
            current_split_utxo.index,
            &worker_addr,
            &amount_per_worker.to_string(),
            &faucet_addr,
            &remaining.to_string(),
            split_parents,
        )
        .await
        .context(format!("Failed to fund worker {}", i))?;

        // Stocker le worker avec son UTXO initial
        workers.push((
            worker_wallet,
            OutputRef {
                txid: split_id.clone(),
                index: 0, // Le worker reçoit toujours l'output 0
                amount: amount_per_worker,
            },
        ));

        // Mettre à jour l'UTXO pour le prochain split
        // Le change est en index 1 (si remaining > 0)
        current_split_utxo = OutputRef {
            txid: split_id,
            index: 1,
            amount: remaining,
        };

        println!("   ✅ Worker {} funded with {} PMS", i, amount_per_worker);
    }

    // Délai pour laisser tous les splits se propager
    sleep(Duration::from_secs(2)).await;

    // ========================================================================
    // ÉTAPE 6: LANCEMENT DU BENCHMARK PARALLÈLE
    // ========================================================================
    //
    // ## Comment ça marche ?
    //
    // 1. On transforme chaque worker en une Future (tokio::spawn)
    // 2. futures::join_all attend que TOUTES les Futures se terminent
    // 3. Le résultat est un Vec de (successful, failed) pour chaque worker
    //
    // ## Pourquoi tokio::spawn ?
    //
    // `tokio::spawn` crée une nouvelle "task" qui peut s'exécuter sur
    // n'importe quel thread du runtime Tokio. Cela permet au scheduler
    // de distribuer le travail efficacement.
    //
    // Voir: Tokio documentation - Tasks
    // ========================================================================
    println!("\n═══════════════════════════════════════════════════════════════");
    println!(
        "🚀 STARTING PARALLEL BENCHMARK: {} workers × {} tx = {} total",
        WORKER_COUNT, TX_PER_WORKER, TX_COUNT
    );
    println!("═══════════════════════════════════════════════════════════════\n");

    let benchmark_start = Instant::now();

    // Compteur de progression partagé entre tous les workers
    let progress_counter = Arc::new(AtomicUsize::new(0));
    let progress_failed = Arc::new(AtomicUsize::new(0));

    // Task de progression qui affiche le TPS en temps réel
    let progress_counter_clone = progress_counter.clone();
    let progress_failed_clone = progress_failed.clone();
    let progress_task = tokio::spawn(async move {
        let mut last_count = 0usize;
        let mut tick = 0u32;
        loop {
            sleep(Duration::from_secs(1)).await;
            tick += 1;
            let current = progress_counter_clone.load(Ordering::Relaxed);
            let failed = progress_failed_clone.load(Ordering::Relaxed);
            let tps = current - last_count;
            let overall_tps = current as f64 / tick as f64;
            last_count = current;

            eprint!(
                "\r   ⏱️  {:>5}/{} tx | {:>4} tx/s (avg: {:.1}) | {} failed   ",
                current, TX_COUNT, tps, overall_tps, failed
            );

            if current >= TX_COUNT {
                eprintln!(); // Nouvelle ligne finale
                break;
            }
        }
    });

    // Créer une Future pour chaque worker
    //
    // Note sur les closures et move:
    // Le `move` est nécessaire car on veut que la closure prenne ownership
    // des variables capturées (client, recipient, etc.) plutôt que de les
    // emprunter. C'est requis car les Futures ont une durée de vie indéfinie.
    //
    // Voir: The Rust Programming Language, Chapitre 13.1 - Closures
    let handles: Vec<_> = workers
        .into_iter() // Consomme `workers`, chaque élément est déplacé
        .enumerate() // Ajoute l'index (id, (wallet, utxo))
        .map(|(id, (wallet, utxo))| {
            // Clone ce dont on a besoin pour chaque worker
            // (client a Clone dérivé via Arc interne de reqwest)
            let client = client.clone();
            let recipient = recipient_addr.clone();
            let admin = admin_addr.to_string();
            let counter = progress_counter.clone();
            let failed_counter = progress_failed.clone();

            // tokio::spawn crée une nouvelle task concurrente
            // La closure `async move` capture les variables par move
            tokio::spawn(async move {
                run_worker(
                    client,
                    NODE_URL,
                    wallet,
                    recipient,
                    admin,
                    utxo,
                    id,
                    counter,
                    failed_counter,
                )
                .await
            })
        })
        .collect();

    // Attendre que TOUS les workers terminent
    // join_all retourne un Vec<Result<(usize, usize), JoinError>>
    let results = futures::future::join_all(handles).await;

    // Agréger les résultats de tous les workers
    let mut successful = 0usize;
    let mut failed = 0usize;
    for (i, result) in results.into_iter().enumerate() {
        match result {
            // JoinHandle::Ok contient le résultat de run_worker
            Ok((s, f)) => {
                println!("   Worker {}: {} success, {} failed", i, s, f);
                successful += s;
                failed += f;
            }
            // JoinHandle::Err signifie que la task a paniqué
            Err(e) => {
                eprintln!("   Worker {} panicked: {}", i, e);
                failed += TX_PER_WORKER;
            }
        }
    }

    let total_duration = benchmark_start.elapsed();
    let final_tps = successful as f64 / total_duration.as_secs_f64();

    // Arrêter la task de progression
    progress_task.abort();

    // ========================================================================
    // ÉTAPE 7: Affichage des résultats
    // ========================================================================
    println!("\n═══════════════════════════════════════════════════════════════");
    println!("📊 BENCHMARK RESULTS");
    println!("═══════════════════════════════════════════════════════════════");
    println!("   Duration:      {:.2}s", total_duration.as_secs_f64());
    println!("   Workers:       {}", WORKER_COUNT);
    println!("   Tx per worker: {}", TX_PER_WORKER);
    println!("   Total tx:      {} submitted", TX_COUNT);
    println!("   Successful:    {}", successful);
    println!("   Failed:        {}", failed);
    println!("   ─────────────────────────────────────────");
    println!("   🚀 TPS:         {:.2} tx/sec", final_tps);
    println!("═══════════════════════════════════════════════════════════════\n");

    // ========================================================================
    // ÉTAPE 7b: Sauvegarder les métriques dans un fichier JSON
    // ========================================================================
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let metrics = serde_json::json!({
        "timestamp": timestamp,
        "workers": WORKER_COUNT,
        "tx_per_worker": TX_PER_WORKER,
        "tx_count": TX_COUNT,
        "successful": successful,
        "failed": failed,
        "duration_secs": total_duration.as_secs_f64(),
        "tps": final_tps
    });
    // ------------------------------------------------------------------------
    // RÉSULTATS & MÉTRIQUES
    // ------------------------------------------------------------------------

    // 1. Définir le dossier racine "benchmark_results" à la racine du WORKSPACE
    // On remonte l'arborescence jusqu'à trouver "Cargo.lock" ou ".git"
    let mut root_path = std::env::current_dir().unwrap();
    loop {
        if root_path.join("Cargo.lock").exists() || root_path.join(".git").exists() {
            break;
        }
        if !root_path.pop() {
            // Fallback: stay on CWD if we hit root without finding marker
            root_path = std::env::current_dir().unwrap();
            break;
        }
    }

    let target_dir = root_path.join("benchmark_results");
    std::fs::create_dir_all(&target_dir).ok();

    // 2. Générer le nom de fichier avec timestamp
    let now = Local::now();
    let timestamp_str = now.format("%Y%m%d_%H%M%S").to_string();
    let filename = format!("bench_{}.json", timestamp_str);
    let metrics_path = target_dir.join(&filename);

    // 3. Sauvegarder le JSON détaillé
    if let Err(e) = std::fs::write(
        &metrics_path,
        serde_json::to_string_pretty(&metrics).unwrap(),
    ) {
        eprintln!("   ⚠️  Failed to write metrics: {}", e);
    } else {
        println!("   📁 Metrics saved to {:?}", metrics_path);
    }

    // 4. Mettre à jour le fichier SUMMARY.md (Tableau trié, plus récent en haut)
    let summary_path = target_dir.join("SUMMARY.md");
    let summary_line = format!(
        "| {} | **{:.2}** | {} | {} | {:.2}s | {} |\n",
        now.format("%Y-%m-%d %H:%M:%S"),
        final_tps,
        WORKER_COUNT,
        TX_COUNT,
        total_duration.as_secs_f64(),
        if failed == 0 { "✅ PASS" } else { "❌ FAIL" }
    );

    let header = "# Benchmark History\n\n| Date | TPS (tx/s) | Workers | Total Tx | Duration | Result |\n|---|---|---|---|---|---|\n";

    let current_content = std::fs::read_to_string(&summary_path).unwrap_or_default();
    let final_summary_content = if current_content.trim().is_empty() {
        format!("{}{}", header, summary_line)
    } else if let Some(table_start) = current_content.find("|---|---|") {
        // Find end of that line
        if let Some(newline_pos) = current_content[table_start..].find('\n') {
            let insert_pos = table_start + newline_pos + 1;
            let (before, after) = current_content.split_at(insert_pos);
            format!("{}{}{}", before, summary_line, after)
        } else {
            // Header exists but no newline? Append.
            format!("{}\n{}", current_content, summary_line)
        }
    } else {
        // No table found, overwrite/create
        format!("{}{}", header, summary_line)
    };

    std::fs::write(&summary_path, final_summary_content).ok();
    println!("   📝 Summary updated in {:?}", summary_path);

    // Log structuré pour analyse ultérieure
    tracing::info!(
        target = "pms_bench",
        event = "benchmark_complete",
        workers = WORKER_COUNT,
        tx_per_worker = TX_PER_WORKER,
        tx_count = TX_COUNT,
        successful = successful,
        failed = failed,
        duration_ms = total_duration.as_millis() as u64,
        tps = final_tps,
        "Parallel benchmark completed"
    );

    // Vérification: au moins 90% de succès
    assert!(
        successful >= TX_COUNT * 90 / 100,
        "Less than 90% success rate: {} / {}",
        successful,
        TX_COUNT
    );

    Ok(())
}

// ============================================================================
// FONCTION WORKER
// ============================================================================
//
// ## Responsabilité
//
// Un worker envoie TX_PER_WORKER transactions en chaîne, chaque tx utilisant
// l'output (change) de la précédente comme input.
//
// ## Paramètres
//
// - client: Client HTTP réutilisé (avec connection pooling)
// - base_url: URL du nœud (ex: https://127.0.0.1:8080)
// - sender: Wallet du worker (possède les clés privées pour signer)
// - recipient_addr: Adresse destination des paiements
// - admin_addr: Adresse qui reçoit les fees
// - current_utxo: UTXO de départ (reçu lors du split initial)
// - worker_id: ID pour le logging
//
// ## Retour
//
// Tuple (successful, failed) comptant les tx réussies/échouées.
// ============================================================================
async fn run_worker(
    client: Client,
    base_url: &'static str, // 'static car NODE_URL est une constante
    sender: Wallet,
    recipient_addr: String,
    admin_addr: String,
    mut current_utxo: OutputRef, // mut car on le met à jour à chaque tx
    worker_id: usize,
    progress_counter: Arc<AtomicUsize>,
    progress_failed: Arc<AtomicUsize>,
) -> (usize, usize) {
    // Parser les constantes une seule fois
    let payment = rust_decimal::Decimal::from_str(PAYMENT_AMOUNT).unwrap();
    let fee = rust_decimal::Decimal::from_str(FEE_AMOUNT).unwrap();
    let sender_addr = sender.get_address("8e");

    let mut successful = 0usize;
    let mut failed = 0usize;

    // Boucle principale: envoie TX_PER_WORKER transactions
    for i in 0..TX_PER_WORKER {
        // Calculer le change après ce paiement
        // change = montant_actuel - paiement - fee
        let change = current_utxo.amount - payment - fee;

        // Envoyer la transaction
        match send_tx_fast(
            &client,
            base_url,
            &sender,
            &current_utxo.txid,
            current_utxo.index,
            &recipient_addr,
            PAYMENT_AMOUNT,
            &admin_addr,
            FEE_AMOUNT,
            &sender_addr,
            &change.to_string(),
        )
        .await
        {
            Ok((txid, change_index)) => {
                // Succès! Mettre à jour l'UTXO pour la prochaine tx
                // Le change de cette tx devient l'input de la suivante
                current_utxo = OutputRef {
                    txid,
                    index: change_index,
                    amount: change,
                };
                successful += 1;
                progress_counter.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                failed += 1;
                progress_failed.fetch_add(1, Ordering::Relaxed);
                if failed <= 3 {
                    // Afficher seulement les 3 premières erreurs pour éviter le spam
                    eprintln!("   ⚠️  Worker {} tx {} failed: {}", worker_id, i, e);
                }
                // IMPORTANT: On arrête ce worker si une tx échoue
                // car la chaîne UTXO est cassée (l'input n'existe pas)
                break;
            }
        }
    }

    (successful, failed)
}

// ============================================================================
// FONCTIONS UTILITAIRES
// ============================================================================

/// Crée un bloc Mint (création monétaire).
///
/// ## PoW (Proof of Work)
///
/// Le bloc doit avoir un ID commençant par "0" (4 bits de difficulté).
/// On incrémente le nonce jusqu'à trouver un hash valide.
///
/// Avec 4 bits de difficulté, il faut en moyenne 16 essais.
/// C'est un anti-spam léger, pas de la sécurité.
async fn mine_mint(
    client: &Client,
    base_url: &str,
    miner: &Wallet,
    to_addr: &str,
    amount: &str,
    parents: Vec<String>,
) -> Result<String> {
    let output = TxOutput::new(to_addr.to_string(), amount.to_string(), None);
    let payload = Some(PayloadEnvelope::Plain(PlainPayload::Mint {
        outputs: vec![output],
    }));
    let mut block = Block::new(parents, payload, 0, None, compute_block_id).unwrap();

    // PoW: trouver un nonce tel que l'ID commence par "0"
    loop {
        if block.id.starts_with("0") {
            break;
        }
        block.nonce += 1;
        block.id = compute_block_id(&block.parents, &block.payload, block.nonce);
    }

    // Construire le WireBlock pour l'API
    let mut wb = WireBlock {
        id: block.id.clone(),
        parents: block.parents.clone(),
        payload_json: Some(serde_json::to_string(&block.payload)?),
        nonce: block.nonce,
        network_id: "pms-mainnet".to_string(),
        protocol_version: 1,
        signer_pk_hex: miner.encoded_public_key(),
        signature_hex: String::new(),
        metadata: None,
    };
    wb.signature_hex = miner.sign(&canonical_wireblock_message(&wb)).unwrap();

    // Envoyer au nœud
    client
        .post(format!("{}/submit/block", base_url))
        .json(&wb)
        .send()
        .await?
        .error_for_status()?;

    Ok(wb.id)
}

/// Envoie une transaction UTXO (version lente, utilisée pour le setup).
///
/// Cette fonction construit manuellement le bloc et fait le PoW.
/// Utilisée uniquement pendant le setup (mint, split) où la vitesse
/// n'est pas critique.
async fn send_tx(
    client: &Client,
    base_url: &str,
    sender: &Wallet,
    _sender_addr: &str,
    utxo_txid: &str,
    utxo_index: u32,
    recipient_addr: &str,
    amount: &str,
    change_addr: &str,
    change_amount: &str,
    parents: Vec<String>,
) -> Result<(String, String)> {
    let mut outputs = vec![TxOutput::new(recipient_addr.to_string(), amount.to_string(), None)];

    if change_amount != "0.0" && change_amount != "0" {
        outputs.push(TxOutput::new(change_addr.to_string(), change_amount.to_string(), None));
    }

    let mut tx = Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: utxo_txid.to_string(),
                index: utxo_index,
            },
        }],
        outputs,
        fee: "0.0".to_string(),
        unlocks: vec![],
    };

    // Signer la transaction
    let msg = tx.signing_message("pms-mainnet").unwrap();
    let sig_b64 = sender.sign(&msg).unwrap();
    tx.unlocks = vec![Unlock {
        pubkey_hex: sender.encoded_public_key(),
        signature_b64: sig_b64,
    }];

    let payload = Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx)));
    let mut block = Block::new(parents, payload, 0, None, compute_block_id).unwrap();

    // PoW
    loop {
        if block.id.starts_with("0") {
            break;
        }
        block.nonce += 1;
        block.id = compute_block_id(&block.parents, &block.payload, block.nonce);
    }

    let mut wb = WireBlock {
        id: block.id.clone(),
        parents: block.parents.clone(),
        payload_json: Some(serde_json::to_string(&block.payload)?),
        nonce: block.nonce,
        network_id: "pms-mainnet".to_string(),
        protocol_version: 1,
        signer_pk_hex: sender.encoded_public_key(),
        signature_hex: String::new(),
        metadata: None,
    };
    wb.signature_hex = sender.sign(&canonical_wireblock_message(&wb)).unwrap();

    let resp = client
        .post(format!("{}/submit/block", base_url))
        .json(&wb)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Request failed: {} -> {}", status, body);
    }

    Ok((wb.id, "".to_string()))
}

/// Envoie une transaction via l'API optimisée /wallet/tx/send.
///
/// ## Pourquoi "fast" ?
///
/// Cette fonction utilise l'endpoint `/wallet/tx/send` qui fait le PoW
/// côté serveur. Cela évite de bloquer le client pendant le mining et
/// permet plus de parallélisme.
///
/// ## Retour
///
/// - `Ok((block_id, change_index))`: La tx a été acceptée
///   - block_id: ID du bloc créé
///   - change_index: Index de l'output de change dans la tx (pour la prochaine tx)
/// - `Err(...)`: La tx a été rejetée (double-spend, UTXO inexistant, etc.)
async fn send_tx_fast(
    client: &Client,
    base_url: &str,
    sender: &Wallet,
    utxo_txid: &str,
    utxo_index: u32,
    recipient_addr: &str,
    amount: &str,
    fee_addr: &str,
    fee_amount: &str,
    change_addr: &str,
    change_amount: &str,
) -> Result<(String, u32)> {
    // Construire les outputs
    // Ordre: [recipient, fee, change]
    let mut outputs = vec![TxOutput::new(recipient_addr.to_string(), amount.to_string(), None)];

    if fee_amount != "0.0" {
        outputs.push(TxOutput::new(fee_addr.to_string(), fee_amount.to_string(), None));
    }

    if change_amount != "0.0" && change_amount != "0" {
        outputs.push(TxOutput::new(change_addr.to_string(), change_amount.to_string(), None));
    }

    // Construire et signer la transaction
    let mut tx = Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: utxo_txid.to_string(),
                index: utxo_index,
            },
        }],
        outputs: outputs.clone(),
        fee: fee_amount.to_string(),
        unlocks: vec![],
    };

    let msg = tx.signing_message("pms-mainnet").unwrap();
    let sig_b64 = sender.sign(&msg).unwrap();
    tx.unlocks = vec![Unlock {
        pubkey_hex: sender.encoded_public_key(),
        signature_b64: sig_b64,
    }];

    // Extraire les clés publiques X25519 pour le chiffrement
    // (utilisé si le serveur chiffre les payloads)
    let mut recipients_xpk = vec![];
    for out in &tx.outputs {
        if let Ok((_h20, xpk)) = pms_wallet::decode_address(&out.address) {
            recipients_xpk.push(xpk);
        }
    }

    // Corps de la requête
    let req_body = serde_json::json!({
        "tx": tx,
        "recipients_xpk": recipients_xpk
    });

    // Envoyer via l'API optimisée
    let resp = client
        .post(format!("{}/wallet/tx/send", base_url))
        .json(&req_body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Request failed: {} -> {}", status, body);
    }

    // Parser la réponse pour obtenir le block ID
    let body = resp.text().await?;
    let json: serde_json::Value = serde_json::from_str(&body)?;
    let block_id = json["id"]
        .as_str()
        .ok_or(anyhow::anyhow!("No id in response"))?
        .to_string();

    // Calculer l'index du change dans les outputs
    // - Si fee != 0: outputs = [recipient, fee, change] → change = index 2
    // - Si fee == 0: outputs = [recipient, change] → change = index 1
    let change_index = if fee_amount != "0.0" { 2u32 } else { 1u32 };

    Ok((block_id, change_index))
}
