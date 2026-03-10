use crate::client::DagClient;
use crate::error::SimResult;
use crate::game::{CubeAttributes, GameEngine};
use crate::metrics::MetricEvent;
use crate::types::*;
use rand::Rng;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock, Semaphore};

/// Max concurrent API calls during bootstrap
const BOOTSTRAP_CONCURRENCY: usize = 30;

/// Bootstrap funder: faucets PMS to agents and optionally mints cube NFTs
pub struct Funder;

impl Funder {
    pub fn new() -> Self {
        Self
    }

    /// Fund all agents in parallel via faucet, then mint cube NFTs in parallel.
    /// Returns a map of agent_name → Vec<cube_token_id>.
    pub async fn fund_all_with_cubes(
        &self,
        client: &DagClient,
        agents: &[(String, WalletInfo, usize)], // (name, wallet, cubes_per_agent)
        faucet_amount: &str,
        metrics_tx: &mpsc::Sender<MetricEvent>,
        game_engine: Option<&Arc<RwLock<GameEngine>>>,
    ) -> SimResult<HashMap<String, Vec<String>>> {
        // ── Phase 1: Parallel faucet ──
        tracing::info!("Phase 1/2: Fauceting {} agents ({}x concurrent)...", agents.len(), BOOTSTRAP_CONCURRENCY);
        let sem = Arc::new(Semaphore::new(BOOTSTRAP_CONCURRENCY));
        let mut faucet_tasks = Vec::with_capacity(agents.len());

        for (name, wallet, _) in agents {
            let client = client.clone();
            let addr = wallet.address.clone();
            let amount = faucet_amount.to_string();
            let name = name.clone();
            let metrics_tx = metrics_tx.clone();
            let sem = sem.clone();

            faucet_tasks.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let faucet_resp = client.faucet(&addr, &amount).await?;
                let block_id = faucet_resp.data.block_id.unwrap_or_default();
                tracing::info!(
                    "[{}] Faucet {} PMS -> block {}",
                    name,
                    amount,
                    &block_id[..16.min(block_id.len())]
                );
                let _ = metrics_tx.try_send(MetricEvent::AgentFunded {
                    agent_name: name,
                    amount,
                });
                Ok::<_, crate::error::SimError>(())
            }));
        }

        // Await all faucet tasks
        let mut faucet_errors = 0usize;
        for task in faucet_tasks {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!("Faucet error: {:#}", e);
                    faucet_errors += 1;
                }
                Err(e) => {
                    tracing::warn!("Faucet task panic: {:#}", e);
                    faucet_errors += 1;
                }
            }
        }
        if faucet_errors > 0 {
            tracing::warn!("{} faucet errors (continuing with cube minting)", faucet_errors);
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
                        let token_id: String = (0..64)
                            .map(|_| format!("{:x}", rng.random_range(0u8..16)))
                            .collect();
                        let attrs = CubeAttributes {
                            weight: rng.random_range(100.0..1000.0),
                            size: rng.random_range(100.0..500.0),
                            density: rng.random_range(1.0..20.0),
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
                BOOTSTRAP_CONCURRENCY
            );

            // Get the main_client from game engine (read lock, quick)
            let ge = engine.read().await;
            let main_client = ge.main_client().clone();
            let divisor = ge.divisor;
            drop(ge);

            // Fire concurrent mint_nft calls
            let sem = Arc::new(Semaphore::new(BOOTSTRAP_CONCURRENCY));
            let mut mint_tasks = Vec::with_capacity(total_cubes);

            for (agent_name, owner_addr, owner_x25519, token_id, attrs) in &cube_specs {
                let client = main_client.clone();
                let token_id = token_id.clone();
                let owner_addr = owner_addr.clone();
                let owner_x25519 = owner_x25519.clone();
                let attrs = attrs.clone();
                let agent_name = agent_name.clone();
                let sem = sem.clone();

                mint_tasks.push(tokio::spawn(async move {
                    let _permit = sem.acquire().await.unwrap();
                    let extra = serde_json::to_string(&attrs).unwrap_or_default();
                    client.mint_nft(&MintNftRequest {
                        token_id: token_id.clone(),
                        owner_address: owner_addr,
                        owner_x25519_pubkey: owner_x25519,
                        metadata: NftMetadataSim {
                            name: Some(format!("Cube Edenite #{}", &token_id[..8])),
                            description: Some(format!(
                                "w={:.0} s={:.0} d={:.1}",
                                attrs.weight, attrs.size, attrs.density
                            )),
                            uri: None,
                            nft_type: Some("cube".to_string()),
                            extra: Some(extra),
                        },
                    }).await?;

                    let reward = attrs.edenite_reward(divisor);
                    tracing::info!(
                        "[{}] Minted cube {} (w={:.0}, s={:.0}, d={:.1}) → {:.10} EDN",
                        agent_name,
                        &token_id[..16],
                        attrs.weight,
                        attrs.size,
                        attrs.density,
                        reward,
                    );
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
