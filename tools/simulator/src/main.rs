mod agent;
mod client;
mod comms;
mod config;
mod error;
mod game;
mod gemini;
mod metrics;
pub mod sim_metrics;
mod tui;
mod types;
mod web;

use agent::{spawn_agent, AgentContext, AgentHandle, PeerInfo};
use base64::Engine;
use client::DagClient;
use comms::CommsRouter;
use config::{AgentBehavior, SimConfig};
use gemini::GeminiClient;
use metrics::aggregator::{create_shared_metrics, run_aggregator};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio_util::sync::CancellationToken;

#[derive(clap::Parser)]
#[command(name = "pms-simulator", about = "DAG-PMS AI agent simulator")]
struct Cli {
    /// Path to simulator config TOML
    #[arg(short, long, default_value = "simulator.dev.toml")]
    config: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli: Cli = clap::Parser::parse();

    // 1. Init tracing first (so retry logs are visible)
    tracing_subscriber::fmt()
        .with_env_filter("pms_simulator=info")
        .with_writer(std::io::stderr)
        .init();

    // 2. Load config + validate credentials with backoff retry
    //    If required env vars (PMS_API_KEY, PMS_COORDINATOR_KEY, PMS_COORDINATOR_ADDR,
    //    PMS_ADMIN_TOKEN) are missing, the simulator retries with increasing delay
    //    instead of crash-looping (which fills the disk with containerd snapshots).
    //    Schedule: 30s, 45s, 60s, 75s, ... (+15s per attempt), max 10 attempts.
    let mut config: SimConfig;
    let mut attempt: u64 = 0;
    loop {
        let config_str = std::fs::read_to_string(&cli.config)
            .map_err(|e| anyhow::anyhow!("Cannot read {}: {}", cli.config, e))?;
        config = toml::from_str(&config_str)?;
        config.resolve_secrets();
        config
            .load_all_agents(&cli.config)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        let errors = config.validate_credentials();
        if errors.is_empty() {
            if attempt > 0 {
                tracing::info!("Credentials resolved on attempt {}/{}", attempt + 1, config::MAX_STARTUP_ATTEMPTS);
            }
            break;
        }

        attempt += 1;
        if attempt >= config::MAX_STARTUP_ATTEMPTS {
            tracing::error!(
                "FATAL: Required credentials still missing after {} attempts. Giving up.\n  {}",
                config::MAX_STARTUP_ATTEMPTS,
                errors.join("\n  ")
            );
            tracing::error!(
                "Fix the environment variables and restart the simulator manually:\n  \
                 docker compose -f docker-compose.testnet.yml up -d --force-recreate pms-simulator"
            );
            // Exit 0 so `restart: on-failure` does NOT restart the container.
            std::process::exit(0);
        }

        let delay = config::RETRY_BASE_DELAY_SECS + config::RETRY_INCREMENT_SECS * (attempt - 1);
        tracing::warn!(
            "Missing required credentials (attempt {}/{}): {}. Retrying in {}s...",
            attempt,
            config::MAX_STARTUP_ATTEMPTS,
            errors.join(", "),
            delay
        );
        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
    }

    tracing::info!(
        "Loaded {} agent definitions ({} from inline, {} from agent_files)",
        config.agents.len(),
        config.agents.len(), // total after merge
        config.agent_files.len()
    );

    // 3. Create HTTP client & health check
    let client = DagClient::new(&config.server);
    tracing::info!("Connecting to gateway at {}...", config.server.url);

    match client.health_check().await {
        Ok(true) => tracing::info!("Gateway is healthy"),
        Ok(false) => anyhow::bail!("Gateway at {} returned unhealthy", config.server.url),
        Err(e) => anyhow::bail!("Cannot reach gateway at {}: {}", config.server.url, e),
    }

    // 4. Create Gemini client (only if smart agents need it)
    let gemini = if config.needs_gemini() {
        let gemini_cfg = config.gemini.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Smart agents require [gemini] config section")
        })?;
        let g = if let (Some(client_id), Some(client_secret)) =
            (&gemini_cfg.client_id, &gemini_cfg.client_secret)
        {
            tracing::info!("Gemini auth: OAuth (browser flow)");
            GeminiClient::with_oauth(client_id, client_secret, &gemini_cfg.model).await?
        } else if let Some(api_key) = &gemini_cfg.api_key {
            tracing::info!("Gemini auth: API key");
            GeminiClient::with_api_key(api_key, &gemini_cfg.model)
        } else {
            anyhow::bail!(
                "Smart agents need Gemini auth. Set gemini.api_key or gemini.client_id + gemini.client_secret"
            );
        };
        tracing::info!("Gemini model: {}", gemini_cfg.model);
        Some(g)
    } else {
        tracing::info!("No smart agents — Gemini not needed");
        None
    };

    // 5. Metrics pipeline (bounded to prevent OOM under backpressure)
    let (metrics_tx, metrics_rx) = mpsc::channel(4096);
    let shared_metrics = create_shared_metrics();
    let shared_clone = shared_metrics.clone();
    tokio::spawn(async move {
        run_aggregator(metrics_rx, shared_clone).await;
    });

    // 6. Comms router (bounded channel for global chat log)
    let (chat_log_tx, chat_log_rx) = mpsc::channel(2048);
    let comms = CommsRouter::new(chat_log_tx);

    // 6b. Web dashboard (broadcast channel for WebSocket fan-out)
    let (ws_tx, _ws_rx) = broadcast::channel::<String>(256);
    if config.web.enabled {
        let web_state = web::WebState {
            tx: ws_tx.clone(),
        };
        tokio::spawn(web::run_web_server(config.web.port, web_state));
    }

    // 6c. Build coordinator wallet from config (hex → base64 conversion)
    let coordinator_wallet: Option<types::WalletInfo> = if let Some(ref coord) = config.coordinator {
        let hex_bytes = hex::decode(&coord.private_key_hex)
            .map_err(|e| anyhow::anyhow!("Invalid coordinator private_key_hex: {}", e))?;
        let private_key_b64 = base64::engine::general_purpose::STANDARD.encode(&hex_bytes);
        tracing::info!(
            "Coordinator wallet configured: {}...{}",
            &coord.address[..8.min(coord.address.len())],
            &coord.address[coord.address.len().saturating_sub(4)..]
        );
        Some(types::WalletInfo {
            address: coord.address.clone(),
            private_key_b64,
            private_key_hex: coord.private_key_hex.clone(),
            public_key_hex: String::new(),
            x25519_pub_hex: String::new(),
            mnemonic_words: None,
        })
    } else {
        None
    };

    // 7. Create agent wallets via API (parallel, up to 50 concurrent)
    let cancel = CancellationToken::new();
    let peer_registry = Arc::new(RwLock::new(Vec::<PeerInfo>::new()));

    // Separate coordinator agents from regular agents
    let mut agent_specs: Vec<(String, usize)> = Vec::new(); // (name, def_idx)
    let mut coordinator_specs: Vec<(String, usize)> = Vec::new(); // coordinator agents
    let mut agent_idx = 0u32;
    for (def_idx, agent_def) in config.agents.iter().enumerate() {
        let prefix = agent_def
            .name_prefix
            .as_deref()
            .unwrap_or("agent");
        let is_coordinator = matches!(agent_def.behavior, AgentBehavior::Coordinator { .. });
        for _ in 0..agent_def.count {
            let name = format!("{}-{}", prefix, agent_idx);
            if is_coordinator {
                coordinator_specs.push((name, def_idx));
            } else {
                agent_specs.push((name, def_idx));
            }
            agent_idx += 1;
        }
    }

    tracing::info!("Creating {} wallets (parallel)...", agent_specs.len());
    let wallet_sem = Arc::new(tokio::sync::Semaphore::new(50));
    let mut wallet_tasks = Vec::new();
    for (name, def_idx) in &agent_specs {
        let client = client.clone();
        let name = name.clone();
        let def_idx = *def_idx;
        let sem = wallet_sem.clone();
        wallet_tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let wallet_resp = client.create_wallet().await?;
            Ok::<_, anyhow::Error>((name, wallet_resp.data, def_idx))
        }));
    }

    let mut all_agents: Vec<(String, types::WalletInfo, usize)> = Vec::new();
    for task in wallet_tasks {
        let (name, wallet, def_idx) = task.await??;
        all_agents.push((name, wallet, def_idx));
    }
    tracing::info!("All {} wallets created", all_agents.len());

    // Register peers (regular agents + coordinator)
    {
        let mut reg = peer_registry.write().await;
        for (name, wallet, _) in &all_agents {
            reg.push(PeerInfo {
                name: name.clone(),
                address: wallet.address.clone(),
            });
        }
        // Register coordinator agents as peers too
        if let Some(ref coord_w) = coordinator_wallet {
            for (name, _) in &coordinator_specs {
                reg.push(PeerInfo {
                    name: name.clone(),
                    address: coord_w.address.clone(),
                });
            }
        }
    }

    // 8. Game engine setup (Edenite cube NFTs).
    //
    // Supports multi-ledger via `[[simulation.games]]` (recommendation #2,
    // v0.7.5). Single-ledger `[simulation.game]` is still honoured —
    // `resolved_games()` flattens both shapes into a uniform Vec.
    // Pass coordinator private key hex for public key derivation (owner
    // of each game ledger).
    let coord_privkey = config.coordinator.as_ref().map(|c| c.private_key_hex.as_str());
    let games_to_boot = config.simulation.resolved_games();
    let mut game_engines: Vec<Arc<RwLock<game::GameEngine>>> =
        Vec::with_capacity(games_to_boot.len());
    for (i, game_config) in games_to_boot.iter().enumerate() {
        tracing::info!(
            "Setting up game engine {}/{} (ledger: {})...",
            i + 1,
            games_to_boot.len(),
            game_config.ledger_id
        );
        let engine = game::GameEngine::setup(&client, game_config, coord_privkey).await?;
        tracing::info!(
            "Game engine ready (edenite on ledger '{}')",
            engine.ledger_id
        );
        game_engines.push(Arc::new(RwLock::new(engine)));
    }
    // Legacy alias for agents that don't care about multi-game.
    let game_engine: Option<Arc<RwLock<game::GameEngine>>> =
        game_engines.first().cloned();

    // 9. Fund all agents via coordinator distribution + optional cube NFT minting
    let coord_w = coordinator_wallet.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "No [coordinator] config — required for PMS distribution to agents. \
             Add [coordinator] section with address + private_key_hex."
        )
    })?;

    tracing::info!(
        "Funding {} agents via coordinator ({} PMS each, concurrency: {})...",
        all_agents.len(),
        config.simulation.faucet_amount,
        config.simulation.bootstrap_concurrency,
    );
    let funder = agent::funder::Funder::with_concurrency(
        config.simulation.bootstrap_concurrency,
    );

    // Build fund list with per-agent cubes_per_agent
    let fund_list: Vec<(String, types::WalletInfo, usize)> = all_agents
        .iter()
        .map(|(name, wallet, def_idx)| {
            let cubes = config.agents[*def_idx]
                .game
                .as_ref()
                .map(|gc| gc.cubes_per_agent)
                .unwrap_or(0);
            (name.clone(), wallet.clone(), cubes)
        })
        .collect();

    let cube_map = funder
        .fund_all_with_cubes(
            &client,
            &fund_list,
            &config.simulation.faucet_amount,
            &metrics_tx,
            game_engine.as_ref(),
            coord_w,
        )
        .await?;

    tracing::info!("All agents funded successfully (cubes minted: {})", cube_map.values().map(|v| v.len()).sum::<usize>());

    // 10. Create shared context
    let ctx = Arc::new(AgentContext {
        client: client.clone(),
        gemini,
        comms: comms.clone(),
        metrics_tx,
        peer_registry,
        cancel: cancel.clone(),
        game_engines,
        game_engine,
        coordinator_wallet: coordinator_wallet.clone(),
    });

    // 11. Spawn agents
    let mut handles: Vec<AgentHandle> = Vec::new();

    for (name, wallet, def_idx) in all_agents {
        let agent_def = &config.agents[def_idx];

        let agent_box: Box<dyn agent::Agent> = match &agent_def.behavior {
            AgentBehavior::Smart { system_prompt } => {
                let inbox = comms.register(&name).await;
                Box::new(agent::smart::SmartAgent::new(
                    name.clone(),
                    wallet,
                    system_prompt.clone(),
                    agent_def.ai_interval,
                    inbox,
                ))
            }
            AgentBehavior::Random {
                min_amount,
                max_amount,
                send_probability,
                sends_per_tick,
            } => {
                let inbox = comms.register(&name).await;
                let mut agent = agent::random::RandomAgent::new(
                    name.clone(),
                    wallet,
                    inbox,
                    *min_amount,
                    *max_amount,
                    *send_probability,
                    *sends_per_tick,
                    agent_def.game.clone(),
                );
                if let Some(cubes) = cube_map.get(&name) {
                    agent.set_cube_ids(cubes.clone());
                }
                Box::new(agent)
            }
            AgentBehavior::Observer => {
                let _inbox = comms.register(&name).await;
                Box::new(agent::observer::ObserverAgent::new(name.clone(), wallet))
            }
            AgentBehavior::Coordinator { .. } => {
                // Coordinator agents are spawned separately below
                continue;
            }
            AgentBehavior::Adversarial {
                attacks,
                sends_per_tick,
            } => {
                let _inbox = comms.register(&name).await;
                Box::new(agent::adversarial::AdversarialAgent::new(
                    name.clone(),
                    wallet,
                    attacks.clone(),
                    *sends_per_tick,
                ))
            }
        };

        handles.push(spawn_agent(agent_box, ctx.clone(), agent_def.interval_ms));
    }

    // 11b. Spawn coordinator agents (use coordinator wallet, no faucet)
    for (name, def_idx) in coordinator_specs {
        let agent_def = &config.agents[def_idx];
        if let Some(ref coord_w) = coordinator_wallet {
            if let AgentBehavior::Coordinator {
                min_amount,
                max_amount,
                send_probability,
                sends_per_tick: _,
            } = &agent_def.behavior
            {
                let agent_box: Box<dyn agent::Agent> =
                    Box::new(agent::coordinator::CoordinatorAgent::new(
                        name.clone(),
                        coord_w.clone(),
                        *min_amount,
                        *max_amount,
                        *send_probability,
                    ));
                handles.push(spawn_agent(agent_box, ctx.clone(), agent_def.interval_ms));
                tracing::info!(
                    "Spawned coordinator agent '{}' (interval: {}ms, {:.2}-{:.2} PMS)",
                    name,
                    agent_def.interval_ms,
                    min_amount,
                    max_amount
                );
            }
        } else {
            tracing::warn!(
                "Coordinator agent '{}' defined but no [coordinator] config — skipping",
                name
            );
        }
    }

    tracing::info!(
        "Spawned {} agents, simulation starting...",
        handles.len()
    );

    // 12. Duration timer
    if config.simulation.duration_secs > 0 {
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(
                config.simulation.duration_secs,
            ))
            .await;
            cancel_clone.cancel();
        });
    }

    // 12b. Memory watchdog — log RSS every 30s, graceful shutdown at 400 MB
    {
        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            let max_rss_bytes: u64 = 400 * 1024 * 1024; // 400 MB
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                tokio::select! {
                    _ = cancel_clone.cancelled() => break,
                    _ = interval.tick() => {
                        if let Ok(rss) = read_rss_bytes() {
                            let rss_mb = rss / (1024 * 1024);
                            tracing::info!("Memory watchdog: RSS = {} MB", rss_mb);
                            if rss > max_rss_bytes {
                                tracing::error!(
                                    "Memory watchdog: RSS {} MB exceeds limit {} MB — shutting down",
                                    rss_mb, max_rss_bytes / (1024 * 1024)
                                );
                                cancel_clone.cancel();
                                break;
                            }
                        }
                    }
                }
            }
        });
    }

    // 13. Forward chat messages to WebSocket broadcast
    let ws_tx_clone = ws_tx.clone();
    let (tui_chat_tx, tui_chat_rx) = mpsc::channel(512);
    tokio::spawn(async move {
        let mut rx = chat_log_rx;
        while let Some(msg) = rx.recv().await {
            // Serialize to JSON for WebSocket clients
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = ws_tx_clone.send(json);
            }
            // Forward summary to TUI channel (drop if TUI can't keep up)
            let _ = tui_chat_tx.try_send(msg);
        }
    });

    // 14. TUI or headless
    if config.tui.enabled {
        let mut tui_app = tui::TuiApp::new(shared_metrics, tui_chat_rx);
        tui_app.run(config.tui.refresh_ms)?;
        cancel.cancel();
    } else {
        tracing::info!("Running headless (Ctrl+C or SIGTERM to stop)...");
        tokio::select! {
            _ = cancel.cancelled() => {}
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Signal received, shutting down...");
                cancel.cancel();
            }
        }
    }

    // 15. Graceful shutdown
    tracing::info!("Shutting down...");
    for handle in handles {
        if let Err(e) = handle.join.await {
            tracing::error!("agent task panicked: {e:?}");
        }
    }

    println!("Simulation complete.");
    Ok(())
}

/// Read current process RSS from /proc/self/statm (Linux) or task_info (macOS).
fn read_rss_bytes() -> Result<u64, ()> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").map_err(|_| ())?;
        let rss_pages: u64 = statm
            .split_whitespace()
            .nth(1) // second field = RSS in pages
            .and_then(|s| s.parse().ok())
            .ok_or(())?;
        Ok(rss_pages * 4096) // page size = 4 KB on Linux
    }
    #[cfg(target_os = "macos")]
    {
        // On macOS, use mach task_info
        use std::mem;
        extern "C" {
            fn mach_task_self() -> u32;
            fn task_info(
                target_task: u32,
                flavor: u32,
                task_info_out: *mut u64,
                task_info_count: *mut u32,
            ) -> i32;
        }
        const MACH_TASK_BASIC_INFO: u32 = 20;
        // struct mach_task_basic_info has 5 u64 fields (on 64-bit)
        let mut info = [0u64; 5];
        let mut count = (mem::size_of_val(&info) / mem::size_of::<u32>()) as u32;
        let kr = unsafe {
            task_info(
                mach_task_self(),
                MACH_TASK_BASIC_INFO,
                info.as_mut_ptr(),
                &mut count,
            )
        };
        if kr != 0 {
            return Err(());
        }
        Ok(info[1]) // resident_size is the second field
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(())
    }
}
