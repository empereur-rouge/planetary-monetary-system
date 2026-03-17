//! Game logic for Edenite — cubes are NFTs with attributes (weight, size, density).
//!
//! When a cube NFT is burned, the agent earns edenite based on:
//! `Edenite = (Weight * Size * Density) / 19,300,000,000`

use crate::client::DagClient;
use crate::config::GameConfig;
use crate::error::SimResult;
use crate::types::*;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Max concurrent API calls for parallel cube minting
const MINT_CONCURRENCY: usize = 30;

/// Default divisor for the edenite formula
const DEFAULT_DIVISOR: f64 = 19_300_000_000.0;

/// Attributes of a cube NFT
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CubeAttributes {
    pub weight: f64,
    pub size: f64,
    pub density: f64,
}

impl CubeAttributes {
    /// Calculate edenite reward: (weight * size * density) / divisor
    pub fn edenite_reward(&self, divisor: f64) -> f64 {
        (self.weight * self.size * self.density) / divisor
    }
}

/// Game engine — manages the game ledger, edenite token, and cube NFTs
pub struct GameEngine {
    /// Client pointing to the game ledger (e.g., /l/eden/...)
    game_client: DagClient,
    /// Game ledger ID
    pub ledger_id: String,
    /// Edenite asset ID on the game ledger
    pub edenite_asset_id: String,
    /// Divisor for the reward formula
    pub divisor: f64,
    /// Registry of minted cubes: token_id → attributes
    cube_registry: HashMap<String, CubeAttributes>,
}

impl GameEngine {
    /// Get a reference to the game ledger client (for parallel cube minting via admin route)
    pub fn game_client(&self) -> &DagClient {
        &self.game_client
    }

    /// Register a pre-minted cube in the local registry
    pub fn register_cube(&mut self, token_id: String, attrs: CubeAttributes) {
        self.cube_registry.insert(token_id, attrs);
    }

    /// Setup: create game ledger + edenite token + register smart contracts.
    /// Registers two contracts:
    /// 1. `edenite-cube-burn` — NFT burn → EDN reward via AttributeFormula
    /// 2. `eden-transfer-fee` — transfer fee (5%) → creator (coordinator) revenue
    /// Ignores 409 CONFLICT (already exists) for idempotent restarts.
    pub async fn setup(
        client: &DagClient,
        config: &GameConfig,
        coordinator_address: Option<&str>,
    ) -> SimResult<Self> {
        let ledger_id = config.ledger_id.clone();
        let network_id = config.network_id.clone();

        // 1. Create game ledger
        tracing::info!("Creating game ledger '{}'...", ledger_id);
        match client
            .create_ledger(&CreateLedgerRequest {
                id: ledger_id.clone(),
                network_id: network_id.clone(),
                prefix: ledger_id.clone(),
                symbol: config.symbol.clone(),
                owner_pubkey: coordinator_address.map(|s| s.to_string()),
                owner_x25519_pubkey: None, // Simulator doesn't need X25519 for ownership
            })
            .await
        {
            Ok(_) => tracing::info!("Game ledger '{}' created", ledger_id),
            Err(e) => {
                let msg = format!("{e}");
                if msg.contains("409") || msg.contains("already exists") || msg.contains("CONFLICT")
                {
                    tracing::info!("Game ledger '{}' already exists, reusing", ledger_id);
                } else {
                    return Err(e);
                }
            }
        }

        // 2. Fund the gas pool (anti-spam) so transactions work on this ledger
        let gas_deposit = &config.gas_pool_deposit;
        tracing::info!(
            "Depositing {} PMS into gas pool for ledger '{}'...",
            gas_deposit,
            ledger_id
        );
        match client
            .deposit_gas(&GasPoolDepositRequest {
                ledger_id: ledger_id.clone(),
                amount: gas_deposit.clone(),
            })
            .await
        {
            Ok(resp) => tracing::info!(
                "Gas pool funded: {} PMS (new balance: {})",
                gas_deposit,
                resp.new_balance.as_deref().unwrap_or("?")
            ),
            Err(e) => {
                // Non-fatal: gas_per_tx might be 0 (disabled) or endpoint may not exist
                tracing::warn!("Gas pool deposit failed (non-fatal): {:#}", e);
            }
        }

        // 3. Create game client pointing to the game ledger
        let game_client = client.with_ledger(&ledger_id);

        // 4. Create edenite token on the game ledger
        tracing::info!("Creating edenite token on ledger '{}'...", ledger_id);
        match game_client
            .create_token(&CreateTokenRequest {
                asset_id: "edenite".to_string(),
                symbol: "EDN".to_string(),
                name: "Edenite".to_string(),
                decimals: 8,
                max_supply: None,
            })
            .await
        {
            Ok(_) => tracing::info!("Edenite token created"),
            Err(e) => {
                let msg = format!("{e}");
                if msg.contains("409") || msg.contains("already exists") || msg.contains("CONFLICT")
                {
                    tracing::info!("Edenite token already exists, reusing");
                } else {
                    return Err(e);
                }
            }
        }

        // 5. Register smart contract: cube burn → EDN reward via contract engine
        // The contract evaluates on every NFT burn on the Edenite ledger where nft_type="cube".
        // It computes: (weight * size * density) / divisor → EDN amount, accumulated in FeePool.
        // Fee distribution then creates EDN UTXOs for the burner.
        let divisor = config.divisor.unwrap_or(DEFAULT_DIVISOR) as u64;
        tracing::info!(
            "Registering smart contract 'edenite-cube-burn' on ledger '{}' (divisor={})...",
            ledger_id,
            divisor
        );
        match client
            .register_contract(&RegisterContractRequest {
                name: "edenite-cube-burn".to_string(),
                scope: ContractScopeSim::Ledger(vec![ledger_id.clone()]),
                trigger: ContractTriggerSim::OnNftBurn {
                    nft_type: Some("cube".to_string()),
                },
                actions: vec![ContractActionSim::AccumulateRefund {
                    asset_id: Some("edenite".to_string()),
                    formula: MintFormulaSim::AttributeFormula {
                        attribute_names: vec![
                            "weight".to_string(),
                            "size".to_string(),
                            "density".to_string(),
                        ],
                        divisor,
                    },
                }],
                enabled: true,
            })
            .await
        {
            Ok(resp) => tracing::info!(
                "Smart contract registered: {:?}",
                resp.contract_id.as_deref().unwrap_or("?")
            ),
            Err(e) => {
                let msg = format!("{e}");
                if msg.contains("409") || msg.contains("already exists") || msg.contains("CONFLICT")
                {
                    tracing::info!("Smart contract 'edenite-cube-burn' already exists, reusing");
                } else {
                    return Err(e);
                }
            }
        }

        // 6. Register transfer fee contract: 5% fee on all transfers → coordinator
        if let Some(coord_addr) = coordinator_address {
            tracing::info!(
                "Registering transfer fee contract on ledger '{}' (5% → {})...",
                ledger_id,
                &coord_addr[..20.min(coord_addr.len())]
            );
            match client
                .register_contract(&RegisterContractRequest {
                    name: "eden-transfer-fee".to_string(),
                    scope: ContractScopeSim::Ledger(vec![ledger_id.clone()]),
                    trigger: ContractTriggerSim::OnTransfer { asset_id: None },
                    actions: vec![ContractActionSim::TransferFee {
                        formula: TransferFeeFormulaSim::PercentageBps { rate_bps: 500 },
                        splits: vec![TransferFeeSplitSim {
                            address: coord_addr.to_string(),
                            share_bps: 10_000,
                        }],
                    }],
                    enabled: true,
                })
                .await
            {
                Ok(resp) => tracing::info!(
                    "Transfer fee contract registered: {:?}",
                    resp.contract_id.as_deref().unwrap_or("?")
                ),
                Err(e) => {
                    let msg = format!("{e}");
                    if msg.contains("409")
                        || msg.contains("already exists")
                        || msg.contains("CONFLICT")
                    {
                        tracing::info!("Transfer fee contract already exists, reusing");
                    } else {
                        return Err(e);
                    }
                }
            }
        }

        Ok(Self {
            game_client,
            ledger_id,
            edenite_asset_id: "edenite".to_string(),
            divisor: config.divisor.unwrap_or(DEFAULT_DIVISOR),
            cube_registry: HashMap::new(),
        })
    }

    /// Mint a cube NFT with random attributes on the Edenite ledger.
    /// Uses admin auth (POST /admin/nft/mint) since NFT minting on custom
    /// ledgers is restricted to coordinator-only for security.
    /// Returns the token_id.
    pub async fn mint_cube(
        &mut self,
        owner_address: &str,
        owner_x25519_pubkey: &str,
    ) -> SimResult<String> {
        // Scope rng usage before any .await (ThreadRng is !Send)
        let (token_id, attrs, extra) = {
            let mut rng = rand::rng();

            let token_id: String = (0..64)
                .map(|_| format!("{:x}", rng.random_range(0u8..16)))
                .collect();

            let attrs = CubeAttributes {
                weight: rng.random_range(100.0..1000.0),
                size: rng.random_range(100.0..500.0),
                density: rng.random_range(1.0..20.0),
            };

            let extra = serde_json::to_string(&attrs).unwrap_or_default();
            (token_id, attrs, extra)
        };

        // Mint on Edenite ledger via admin route (coordinator-only)
        self.game_client
            .admin_mint_nft(&MintNftRequest {
                token_id: token_id.clone(),
                owner_address: owner_address.to_string(),
                owner_x25519_pubkey: owner_x25519_pubkey.to_string(),
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
            })
            .await?;

        let reward = attrs.edenite_reward(self.divisor);
        tracing::info!(
            "Minted cube {} (w={:.0}, s={:.0}, d={:.1}) → potential {:.10} EDN",
            &token_id[..16],
            attrs.weight,
            attrs.size,
            attrs.density,
            reward
        );

        // Store attributes locally for logging; contract engine computes actual reward
        self.cube_registry.insert(token_id.clone(), attrs);

        Ok(token_id)
    }

    /// Mint multiple cube NFTs in parallel on the Edenite ledger.
    /// Pre-generates all specs synchronously, then fires concurrent admin_mint_nft calls.
    /// Returns a Vec of successfully minted token_ids.
    pub async fn mint_cubes_parallel(
        &mut self,
        owner_address: &str,
        owner_x25519_pubkey: &str,
        count: usize,
    ) -> Vec<String> {
        if count == 0 {
            return vec![];
        }

        // 1. Pre-generate all cube specs (sync, no async)
        let specs: Vec<(String, CubeAttributes, String)> = {
            let mut rng = rand::rng();
            (0..count)
                .map(|_| {
                    let token_id: String = (0..64)
                        .map(|_| format!("{:x}", rng.random_range(0u8..16)))
                        .collect();
                    let attrs = CubeAttributes {
                        weight: rng.random_range(100.0..1000.0),
                        size: rng.random_range(100.0..500.0),
                        density: rng.random_range(1.0..20.0),
                    };
                    let extra = serde_json::to_string(&attrs).unwrap_or_default();
                    (token_id, attrs, extra)
                })
                .collect()
        };

        // 2. Fire concurrent admin_mint_nft calls
        let client = self.game_client.clone();
        let sem = Arc::new(Semaphore::new(MINT_CONCURRENCY));
        let mut tasks = Vec::with_capacity(count);

        for (token_id, attrs, extra) in &specs {
            let client = client.clone();
            let sem = sem.clone();
            let token_id = token_id.clone();
            let owner_address = owner_address.to_string();
            let owner_x25519 = owner_x25519_pubkey.to_string();
            let attrs_clone = attrs.clone();
            let extra = extra.clone();

            tasks.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let result = client
                    .admin_mint_nft(&MintNftRequest {
                        token_id: token_id.clone(),
                        owner_address,
                        owner_x25519_pubkey: owner_x25519,
                        metadata: NftMetadataSim {
                            name: Some(format!("Cube Edenite #{}", &token_id[..8])),
                            description: Some(format!(
                                "w={:.0} s={:.0} d={:.1}",
                                attrs_clone.weight, attrs_clone.size, attrs_clone.density
                            )),
                            uri: None,
                            nft_type: Some("cube".to_string()),
                            extra: Some(extra),
                        },
                    })
                    .await;
                (token_id, result)
            }));
        }

        // 3. Collect results and register successful mints
        let mut minted_ids = Vec::with_capacity(count);
        let mut ok_count = 0usize;
        let mut err_count = 0usize;

        // Build a lookup map for specs (token_id → attrs)
        let spec_map: HashMap<String, CubeAttributes> = specs
            .into_iter()
            .map(|(id, attrs, _)| (id, attrs))
            .collect();

        for task in tasks {
            match task.await {
                Ok((token_id, Ok(_))) => {
                    if let Some(attrs) = spec_map.get(&token_id) {
                        self.cube_registry.insert(token_id.clone(), attrs.clone());
                    }
                    minted_ids.push(token_id);
                    ok_count += 1;
                }
                Ok((token_id, Err(e))) => {
                    tracing::warn!("Cube mint failed {}: {:#}", &token_id[..16], e);
                    err_count += 1;
                }
                Err(e) => {
                    tracing::warn!("Cube mint task panic: {:#}", e);
                    err_count += 1;
                }
            }
        }

        tracing::info!(
            "Parallel mint: {} ok, {} errors (requested {})",
            ok_count,
            err_count,
            count
        );

        minted_ids
    }

    /// Burn a cube NFT on the Edenite ledger.
    /// The smart contract automatically accumulates EDN refund in the FeePool.
    /// EDN is distributed to the burner at the next fee distribution cycle.
    /// Returns the expected edenite amount (for logging/metrics only).
    pub async fn burn_cube_for_edenite(
        &mut self,
        private_key_b64: &str,
        _agent_addr: &str,
        token_id: &str,
    ) -> SimResult<String> {
        // 1. Get cube attributes (remove from registry — it's being burned)
        let attrs = self
            .cube_registry
            .remove(token_id)
            .ok_or_else(|| {
                crate::error::SimError::Other(anyhow::anyhow!("Cube {} not found in registry", token_id))
            })?;

        // 2. Burn the NFT on the Edenite ledger — contract will auto-accumulate EDN refund
        self.game_client
            .burn_nft_simple(&BurnNftSimpleRequest {
                private_key_b64: private_key_b64.to_string(),
                token_id: token_id.to_string(),
            })
            .await?;

        // 3. Calculate expected reward (for logging only — actual mint via contract + fee_distribution)
        let expected_edn = attrs.edenite_reward(self.divisor);
        let edenite_str = format!("{:.10}", expected_edn);

        tracing::info!(
            "Burned cube {} → expected ~{} EDN (via contract)",
            &token_id[..16.min(token_id.len())],
            edenite_str,
        );

        Ok(edenite_str)
    }

    /// Burn multiple cube NFTs in a single API call on the Edenite ledger.
    /// The smart contract accumulates EDN refund in the FeePool for each burn.
    /// Returns (expected_edenite_amount_str, count_burned).
    pub async fn burn_cubes_for_edenite(
        &mut self,
        private_key_b64: &str,
        _agent_addr: &str,
        token_ids: Vec<String>,
    ) -> SimResult<(String, usize)> {
        if token_ids.is_empty() {
            return Ok(("0".to_string(), 0));
        }

        // 1. Calculate expected total edenite reward (remove from registry)
        let mut total_edenite = 0.0f64;
        for tid in &token_ids {
            if let Some(attrs) = self.cube_registry.remove(tid) {
                total_edenite += attrs.edenite_reward(self.divisor);
            }
        }

        let count = token_ids.len();

        // 2. Batch burn all NFTs on the Edenite ledger — contract handles EDN accumulation
        self.game_client
            .burn_nft_batch_simple(&BurnNftBatchSimpleRequest {
                private_key_b64: private_key_b64.to_string(),
                token_ids: token_ids.clone(),
            })
            .await?;

        let edenite_str = format!("{:.10}", total_edenite);

        tracing::info!(
            "Batch burned {} cubes → expected ~{} EDN (via contract)",
            count,
            edenite_str,
        );

        Ok((edenite_str, count))
    }

    /// Send edenite from one agent to another via real UTXO transfer.
    /// Uses send-simple with asset_id on the game ledger.
    pub async fn send_edenite(
        &self,
        private_key_b64: &str,
        to_addr: &str,
        amount: &str,
    ) -> SimResult<()> {
        self.game_client
            .send_simple(&SendSimpleRequest {
                private_key_b64: private_key_b64.to_string(),
                to: to_addr.to_string(),
                amount: amount.to_string(),
                asset_id: Some(self.edenite_asset_id.clone()),
            })
            .await?;
        Ok(())
    }

    /// Query the EDN balance for an address on the Edenite ledger.
    /// Sums UTXOs with asset_id = "edenite".
    pub async fn get_edn_balance(&self, address: &str) -> SimResult<f64> {
        let bal_str = self
            .game_client
            .token_balance(address, &self.edenite_asset_id)
            .await?;
        Ok(bal_str.parse::<f64>().unwrap_or(0.0))
    }

    /// Number of cubes currently tracked in the registry (for diagnostics).
    pub fn registry_len(&self) -> usize {
        self.cube_registry.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_registry_shrinks_after_remove() {
        let mut registry = HashMap::new();

        // Simulate minting 10 cubes
        for i in 0..10 {
            let token_id = format!("cube-{:04}", i);
            let attrs = CubeAttributes {
                weight: 500.0 + i as f64,
                size: 250.0,
                density: 10.0,
            };
            registry.insert(token_id, attrs);
        }
        println!("After minting 10 cubes: registry.len() = {}", registry.len());
        assert_eq!(registry.len(), 10);

        // Simulate burning 5 cubes (remove from registry)
        let to_burn: Vec<String> = (0..5).map(|i| format!("cube-{:04}", i)).collect();
        let mut total_reward = 0.0;
        for tid in &to_burn {
            if let Some(attrs) = registry.remove(tid) {
                total_reward += attrs.edenite_reward(DEFAULT_DIVISOR);
            }
        }
        println!(
            "After burning 5 cubes: registry.len() = {}, total_reward = {:.10}",
            registry.len(),
            total_reward
        );
        assert_eq!(registry.len(), 5, "burned cubes must be removed from registry");
        assert!(total_reward > 0.0, "reward must be positive");

        // Burned cubes must no longer be in registry
        for tid in &to_burn {
            assert!(
                !registry.contains_key(tid),
                "burned cube {} still in registry",
                tid
            );
        }
        println!("All burned cubes confirmed absent from registry");

        // Remaining cubes must still be present
        for i in 5..10 {
            let tid = format!("cube-{:04}", i);
            assert!(
                registry.contains_key(&tid),
                "unburned cube {} missing from registry",
                tid
            );
        }
        println!("All 5 unburned cubes confirmed present in registry");
    }

    #[test]
    fn edenite_reward_formula_correct() {
        let attrs = CubeAttributes {
            weight: 1000.0,
            size: 500.0,
            density: 19.3,
        };
        let reward = attrs.edenite_reward(DEFAULT_DIVISOR);
        // Expected: (1000 * 500 * 19.3) / 19_300_000_000 = 9_650_000 / 19_300_000_000 = 0.0005
        let expected = 0.0005;
        println!(
            "Reward for w=1000, s=500, d=19.3: {:.10} (expected {:.10})",
            reward, expected
        );
        assert!(
            (reward - expected).abs() < 1e-12,
            "reward {} != expected {}",
            reward,
            expected
        );
    }
}
