mod agent;
mod client;
mod comms;
mod config;
mod error;
mod game;
mod gemini;
mod metrics;
mod tui;
mod types;
mod web;

use agent::{spawn_agent, AgentContext, AgentHandle, PeerInfo};
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

    // 1. Load config + external agent files
    let config_str = std::fs::read_to_string(&cli.config)
        .map_err(|e| anyhow::anyhow!("Cannot read {}: {}", cli.config, e))?;
    let mut config: SimConfig = toml::from_str(&config_str)?;
    config.resolve_secrets();
    config
        .load_all_agents(&cli.config)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    // 2. Init tracing (stderr to not interfere with TUI)
    tracing_subscriber::fmt()
        .with_env_filter("pms_simulator=info")
        .with_writer(std::io::stderr)
        .init();

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

    // 5. Metrics pipeline
    let (metrics_tx, metrics_rx) = mpsc::unbounded_channel();
    let shared_metrics = create_shared_metrics();
    let shared_clone = shared_metrics.clone();
    tokio::spawn(async move {
        run_aggregator(metrics_rx, shared_clone).await;
    });

    // 6. Comms router
    let (chat_log_tx, chat_log_rx) = mpsc::unbounded_channel();
    let comms = CommsRouter::new(chat_log_tx);

    // 6b. Web dashboard (broadcast channel for WebSocket fan-out)
    let (ws_tx, _ws_rx) = broadcast::channel::<String>(256);
    if config.web.enabled {
        let web_state = web::WebState {
            tx: ws_tx.clone(),
        };
        tokio::spawn(web::run_web_server(config.web.port, web_state));
    }

    // 7. Create agent wallets via API (parallel, up to 50 concurrent)
    let cancel = CancellationToken::new();
    let peer_registry = Arc::new(RwLock::new(Vec::<PeerInfo>::new()));

    let mut agent_specs: Vec<(String, usize)> = Vec::new(); // (name, def_idx)
    let mut agent_idx = 0u32;
    for (def_idx, agent_def) in config.agents.iter().enumerate() {
        let prefix = agent_def
            .name_prefix
            .as_deref()
            .unwrap_or("agent");
        for _ in 0..agent_def.count {
            agent_specs.push((format!("{}-{}", prefix, agent_idx), def_idx));
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

    // Register peers
    {
        let mut reg = peer_registry.write().await;
        for (name, wallet, _) in &all_agents {
            reg.push(PeerInfo {
                name: name.clone(),
                address: wallet.address.clone(),
            });
        }
    }

    // 8. Optional game engine setup (Edenite cube NFTs)
    let game_engine = if let Some(ref game_config) = config.simulation.game {
        tracing::info!("Setting up game engine (ledger: {})...", game_config.ledger_id);
        let engine = game::GameEngine::setup(&client, game_config).await?;
        tracing::info!("Game engine ready (edenite on ledger '{}')", engine.ledger_id);
        Some(Arc::new(RwLock::new(engine)))
    } else {
        None
    };

    // 9. Fund all agents via faucet + optional cube NFT minting
    // cubes_per_agent is now per-agent-def from AgentGameConfig
    tracing::info!(
        "Funding {} agents via faucet ({} PMS each)...",
        all_agents.len(),
        config.simulation.faucet_amount
    );
    let funder = agent::funder::Funder::new();

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
        game_engine,
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
            } => {
                let inbox = comms.register(&name).await;
                let mut agent = agent::random::RandomAgent::new(
                    name.clone(),
                    wallet,
                    inbox,
                    *min_amount,
                    *max_amount,
                    *send_probability,
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
        };

        handles.push(spawn_agent(agent_box, ctx.clone(), agent_def.interval_ms));
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

    // 13. Forward chat messages to WebSocket broadcast
    let ws_tx_clone = ws_tx.clone();
    let (tui_chat_tx, tui_chat_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut rx = chat_log_rx;
        while let Some(msg) = rx.recv().await {
            // Serialize to JSON for WebSocket clients
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = ws_tx_clone.send(json);
            }
            // Forward summary to TUI channel
            let _ = tui_chat_tx.send(msg);
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
        let _ = handle.join.await;
    }

    println!("Simulation complete.");
    Ok(())
}
