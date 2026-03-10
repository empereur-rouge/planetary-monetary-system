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
    /// Client for main ledger (NFT mint/burn)
    main_client: DagClient,
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
    /// Get a reference to the main ledger client (for parallel cube minting)
    pub fn main_client(&self) -> &DagClient {
        &self.main_client
    }

    /// Register a pre-minted cube in the local registry
    pub fn register_cube(&mut self, token_id: String, attrs: CubeAttributes) {
        self.cube_registry.insert(token_id, attrs);
    }

    /// Setup: create game ledger + edenite token
    /// Ignores 409 CONFLICT (already exists) for idempotent restarts
    pub async fn setup(client: &DagClient, config: &GameConfig) -> SimResult<Self> {
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

        // 2. Create game client pointing to the game ledger
        let game_client = client.with_ledger(&ledger_id);

        // 3. Create edenite token on the game ledger
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

        Ok(Self {
            game_client,
            main_client: client.clone(),
            ledger_id,
            edenite_asset_id: "edenite".to_string(),
            divisor: config.divisor.unwrap_or(DEFAULT_DIVISOR),
            cube_registry: HashMap::new(),
        })
    }

    /// Mint a cube NFT with random attributes via PMS /v1/nft/mint
    /// Returns the token_id
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

        // Mint via PMS NFT API (main ledger)
        self.main_client
            .mint_nft(&MintNftRequest {
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

        // Store attributes locally for burn calculation
        self.cube_registry.insert(token_id.clone(), attrs);

        Ok(token_id)
    }

    /// Burn a cube NFT and mint edenite reward on the game ledger
    /// Returns the edenite amount received
    pub async fn burn_cube_for_edenite(
        &mut self,
        private_key_b64: &str,
        agent_addr: &str,
        token_id: &str,
    ) -> SimResult<String> {
        // 1. Get cube attributes (remove from registry — it's being burned)
        let attrs = self
            .cube_registry
            .remove(token_id)
            .ok_or_else(|| {
                crate::error::SimError::Other(anyhow::anyhow!("Cube {} not found in registry", token_id))
            })?;

        // 2. Burn the NFT via burn-simple
        self.main_client
            .burn_nft_simple(&BurnNftSimpleRequest {
                private_key_b64: private_key_b64.to_string(),
                token_id: token_id.to_string(),
            })
            .await?;

        // 3. Calculate edenite reward
        let edenite_amount = attrs.edenite_reward(self.divisor);
        let edenite_str = format!("{:.10}", edenite_amount);

        // 4. Mint edenite to the agent on the game ledger
        self.game_client
            .mint_token(&MintTokenRequest {
                asset_id: self.edenite_asset_id.clone(),
                to: agent_addr.to_string(),
                amount: edenite_str.clone(),
            })
            .await?;

        tracing::info!(
            "Burned cube {} → {} EDN to {}",
            &token_id[..16.min(token_id.len())],
            edenite_str,
            &agent_addr[..20.min(agent_addr.len())]
        );

        Ok(edenite_str)
    }

    /// Burn multiple cube NFTs in a single API call and mint total edenite reward.
    /// Returns (edenite_amount_str, count_burned).
    pub async fn burn_cubes_for_edenite(
        &mut self,
        private_key_b64: &str,
        agent_addr: &str,
        token_ids: Vec<String>,
    ) -> SimResult<(String, usize)> {
        if token_ids.is_empty() {
            return Ok(("0".to_string(), 0));
        }

        // 1. Calculate total edenite reward (remove from registry — they're being burned)
        let mut total_edenite = 0.0f64;
        for tid in &token_ids {
            if let Some(attrs) = self.cube_registry.remove(tid) {
                total_edenite += attrs.edenite_reward(self.divisor);
            }
        }

        let count = token_ids.len();

        // 2. Batch burn all NFTs in one call
        self.main_client
            .burn_nft_batch_simple(&BurnNftBatchSimpleRequest {
                private_key_b64: private_key_b64.to_string(),
                token_ids: token_ids.clone(),
            })
            .await?;

        // 3. Mint total edenite to the agent on the game ledger
        let edenite_str = format!("{:.10}", total_edenite);
        self.game_client
            .mint_token(&MintTokenRequest {
                asset_id: self.edenite_asset_id.clone(),
                to: agent_addr.to_string(),
                amount: edenite_str.clone(),
            })
            .await?;

        tracing::info!(
            "Batch burned {} cubes → {} EDN to {}",
            count,
            edenite_str,
            &agent_addr[..20.min(agent_addr.len())]
        );

        Ok((edenite_str, count))
    }

    /// Send edenite from one agent to another (via admin mint to receiver).
    /// The sender's balance is tracked locally by the agent.
    pub async fn send_edenite(
        &self,
        to_addr: &str,
        amount: &str,
    ) -> SimResult<()> {
        self.game_client
            .mint_token(&MintTokenRequest {
                asset_id: self.edenite_asset_id.clone(),
                to: to_addr.to_string(),
                amount: amount.to_string(),
            })
            .await?;
        Ok(())
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
