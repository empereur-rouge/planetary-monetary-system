use crate::client::DagClient;
use crate::error::SimResult;
use crate::game::{CubeAttributes, CubeRarity, GameEngine, generate_token_id};
use crate::metrics::MetricEvent;
use crate::types::{
    MintNftRequest, NftMetadataSim, SendSimpleRequest, WalletInfo,
};
use rand::Rng;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{mpsc, RwLock, Semaphore};

/// Default max concurrent API calls during bootstrap. Overridden via
/// `[simulation].bootstrap_concurrency` — see `Funder::with_concurrency`.
pub const DEFAULT_BOOTSTRAP_CONCURRENCY: usize = 128;

/// Bootstrap funder: coordinator-based PMS distribution + optional cube NFT minting.
///
/// Flow:
/// 1. Faucet a large sum to the coordinator wallet
/// 2. Coordinator sends `faucet_amount` PMS to each agent via send_simple
/// 3. Optionally mint cube NFTs for game-enabled agents
pub struct Funder {
    concurrency: usize,
}

impl Funder {
    pub fn new() -> Self {
        Self {
            concurrency: DEFAULT_BOOTSTRAP_CONCURRENCY,
        }
    }

    /// Build a funder with a custom concurrency cap. At 1000+ agents
    /// the default 128 boots in ~90 s; lower values keep the gateway
    /// rate-limiter happy if it's tightly tuned, higher values shave
    /// boot time when the gateway can absorb the storm.
    pub fn with_concurrency(concurrency: usize) -> Self {
        Self {
            concurrency: concurrency.max(1),
        }
    }

    /// Fund all agents via coordinator distribution, then mint cube NFTs in parallel.
    /// Returns a map of agent_name → Vec<cube_token_id>.
    pub async fn fund_all_with_cubes(
        &self,
        client: &DagClient,
        agents: &[(String, WalletInfo, usize)], // (name, wallet, cubes_per_agent)
        faucet_amount: &str,
        metrics_tx: &mpsc::Sender<MetricEvent>,
        game_engine: Option<&Arc<RwLock<GameEngine>>>,
        coordinator_wallet: &WalletInfo,
    ) -> SimResult<HashMap<String, Vec<String>>> {
        let per_agent: f64 = faucet_amount.parse().unwrap_or(50.0);
        // Faucet enough for all agents + buffer for fees and future refuels
        let total_needed = per_agent * agents.len() as f64 * 3.0; // 3x buffer
        let total_str = format!("{:.2}", total_needed);

        // ── Phase 1a: Faucet to coordinator wallet ──
        tracing::info!(
            "Phase 1/2: Fauceting {} PMS to coordinator ({} agents × {} PMS × 3x buffer)...",
            total_str,
            agents.len(),
            faucet_amount
        );
        let faucet_resp = client.faucet(&coordinator_wallet.address, &total_str).await?;
        let block_id = faucet_resp.data.block_id.unwrap_or_default();
        tracing::info!(
            "[coordinator] Faucet {} PMS → block {}",
            total_str,
            &block_id[..16.min(block_id.len())]
        );

        // ── Phase 1b: Coordinator distributes to each agent via send_simple ──
        tracing::info!(
            "Phase 1/2: Coordinator distributing {} PMS to {} agents ({}x concurrent)...",
            faucet_amount,
            agents.len(),
            self.concurrency
        );
        let sem = Arc::new(Semaphore::new(self.concurrency));
        let progress = Arc::new(AtomicUsize::new(0));
        let total_agents = agents.len();
        let mut send_tasks = Vec::with_capacity(agents.len());
        let log_per_n_agents = total_agents.max(100) / 20; // ~5% steps, min 5

        for (name, wallet, _) in agents {
            let client = client.clone();
            let to_addr = wallet.address.clone();
            let amount = faucet_amount.to_string();
            let name = name.clone();
            let metrics_tx = metrics_tx.clone();
            let sem = sem.clone();
            let coord_key = coordinator_wallet.private_key_b64.clone();
            let progress = progress.clone();

            send_tasks.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let resp = client.send_simple(&SendSimpleRequest {
                    private_key_b64: coord_key,
                    to: to_addr.clone(),
                    amount: amount.clone(),
                    asset_id: None,
                }).await?;
                let block_id = resp.data.block_id.unwrap_or_default();
                let done = progress.fetch_add(1, Ordering::Relaxed) + 1;
                // Emit per-agent log line only at small fleet sizes
                // (otherwise a 1000-agent fleet floods the log with
                // redundant lines). Bigger fleets get coarse progress
                // milestones every ~5%.
                if total_agents <= 200 {
                    tracing::info!(
                        "[coordinator → {}] Sent {} PMS (block {})",
                        name,
                        amount,
                        &block_id[..16.min(block_id.len())]
                    );
                } else if done % log_per_n_agents == 0 || done == total_agents {
                    tracing::info!(
                        "Coordinator funding progress: {}/{} ({:.0}%)",
                        done,
                        total_agents,
                        (done as f64 / total_agents as f64) * 100.0
                    );
                }
                let _ = metrics_tx.try_send(MetricEvent::AgentFunded {
                    agent_name: name,
                    amount,
                });
                Ok::<_, crate::error::SimError>(())
            }));
        }

        // Await all send tasks
        let mut send_errors = 0usize;
        for task in send_tasks {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!("Coordinator send error: {:#}", e);
                    send_errors += 1;
                }
                Err(e) => {
                    tracing::warn!("Coordinator send task panic: {:#}", e);
                    send_errors += 1;
                }
            }
        }
        if send_errors > 0 {
            tracing::warn!("{} coordinator send errors (continuing with cube minting)", send_errors);
        }

        // ── Phase 2: Parallel cube minting ──
        let cube_map = if let Some(engine) = game_engine {
            // Pre-generate all cube specs (no async, just rand)
            let mut cube_specs: Vec<(String, String, String, String, CubeAttributes)> = Vec::new();
            for (name, wallet, cubes_per_agent) in agents {
                if *cubes_per_agent == 0 {
                    continue;
                }
                for _ in 0..*cubes_per_agent {
                    let (token_id, attrs) = {
                        let mut rng = rand::rng();
                        let rarity = CubeRarity::roll(&mut rng);
                        let token_id = generate_token_id(&rarity, &mut rng);
                        let attrs = CubeAttributes {
                            weight: crate::game::round2(rng.random_range(1.0..30.0)),
                            size: crate::game::round2(rng.random_range(0.5..5.0)),
                            density: crate::game::round2(rng.random_range(0.1..1.0)),
                            rarity,
                        };
                        (token_id, attrs)
                    };
                    cube_specs.push((
                        name.clone(),
                        wallet.address.clone(),
                        wallet.x25519_pub_hex.clone(),
                        token_id,
                        attrs,
                    ));
                }
            }

            let total_cubes = cube_specs.len();
            tracing::info!(
                "Phase 2/2: Minting {} cubes for {} agents ({}x concurrent)...",
                total_cubes,
                agents.iter().filter(|(_, _, c)| *c > 0).count(),
                self.concurrency
            );

            // Get the game_client from game engine (admin auth for custom ledger minting)
            let ge = engine.read().await;
            let game_client = ge.game_client().clone();
            let divisor = ge.divisor;
            drop(ge);

            // Fire concurrent mint_nft calls
            let sem = Arc::new(Semaphore::new(self.concurrency));
            let mut mint_tasks = Vec::with_capacity(total_cubes);
            let mint_progress = Arc::new(AtomicUsize::new(0));
            let mint_log_step = total_cubes.max(100) / 20;

            for (agent_name, owner_addr, owner_x25519, token_id, attrs) in &cube_specs {
                let client = game_client.clone();
                let token_id = token_id.clone();
                let owner_addr = owner_addr.clone();
                let owner_x25519 = owner_x25519.clone();
                let attrs = attrs.clone();
                let agent_name = agent_name.clone();
                let sem = sem.clone();
                let mint_progress = mint_progress.clone();

                mint_tasks.push(tokio::spawn(async move {
                    let _permit = sem.acquire().await.unwrap();
                    let extra = attrs.to_obfuscated_extra();
                    let rarity_label = attrs.rarity.label().to_string();
                    client.admin_mint_nft(&MintNftRequest {
                        token_id: token_id.clone(),
                        owner_address: owner_addr,
                        owner_x25519_pubkey: owner_x25519,
                        metadata: NftMetadataSim {
                            name: Some(format!(
                                "[{}] Cube Edenite #{}",
                                rarity_label,
                                &token_id[..8]
                            )),
                            description: Some(format!("[{}]", rarity_label)),
                            uri: None,
                            nft_type: Some("cube".to_string()),
                            extra: Some(extra),
                        },
                    }).await?;

                    let reward = attrs.edenite_reward(divisor);
                    let done = mint_progress.fetch_add(1, Ordering::Relaxed) + 1;
                    if total_cubes <= 500 {
                        tracing::info!(
                            "[{}] Minted [{}] cube {} → {:.10} EDN",
                            agent_name,
                            rarity_label,
                            &token_id[..16],
                            reward,
                        );
                    } else if done % mint_log_step == 0 || done == total_cubes {
                        tracing::info!(
                            "Cube mint progress: {}/{} ({:.0}%)",
                            done,
                            total_cubes,
                            (done as f64 / total_cubes as f64) * 100.0
                        );
                    }
                    Ok::<_, crate::error::SimError>(())
                }));
            }

            // Await all mint tasks
            let mut mint_ok = 0usize;
            let mut mint_err = 0usize;
            for task in mint_tasks {
                match task.await {
                    Ok(Ok(())) => mint_ok += 1,
                    Ok(Err(e)) => {
                        tracing::warn!("Cube mint error: {:#}", e);
                        mint_err += 1;
                    }
                    Err(e) => {
                        tracing::warn!("Cube mint task panic: {:#}", e);
                        mint_err += 1;
                    }
                }
            }
            tracing::info!(
                "Cube minting done: {} ok, {} errors",
                mint_ok, mint_err
            );

            // Bulk insert all successfully minted cubes into the registry
            let mut ge = engine.write().await;
            let mut result_map: HashMap<String, Vec<String>> = HashMap::new();
            for (agent_name, _, _, token_id, attrs) in cube_specs {
                // Only register cubes that were minted (we trust mint_ok count)
                ge.register_cube(token_id.clone(), attrs);
                result_map
                    .entry(agent_name)
                    .or_default()
                    .push(token_id);
            }
            drop(ge);

            result_map
        } else {
            HashMap::new()
        };

        Ok(cube_map)
    }
}
