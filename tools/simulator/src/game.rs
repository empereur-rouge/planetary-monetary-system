//! Game logic for Edenite — cubes are NFTs with obfuscated attributes and rarity tiers.
//!
//! Attribute names are SHA256-obfuscated (not stored in clear text) in both NFT metadata
//! and smart contract definitions. Rarity is encoded in the token_id via leading zeros:
//! Basic (0), Common (1), Uncommon (2), Rare (3), Legendary (4), Unique (5).

use crate::client::DagClient;
use crate::config::GameConfig;
use crate::error::SimResult;
use crate::types::*;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Semaphore;
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey as XPublic, StaticSecret};

/// Max concurrent API calls for parallel cube minting
const MINT_CONCURRENCY: usize = 30;

/// Default divisor for the edenite formula.
///
/// Calibrated for cube ranges: weight 1-30 kg, size 0.5-5 cm, density 0.1-1.0.
/// Yields ~0.00171 EDN/cube → ~277 EDN/month at 9 cubes/min (10h/day).
const DEFAULT_DIVISOR: f64 = 13_700.0;

/// Salt used for attribute name obfuscation (SHA256-based)
const ATTR_OBFUSCATION_SALT: &[u8] = b"pms-cube-attrs-v1";

/// Round an f64 to 2 decimal places.
pub fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Rarity tier for a cube NFT, encoded in the token_id via leading zeros.
///
/// Probabilities are based on a roll over 100,000,000:
/// - Basic:     roll >= 100,000       (99.9%)
/// - Common:    10,000 <= roll < 100,000  (0.09%)
/// - Uncommon:  1,000 <= roll < 10,000    (0.009%)
/// - Rare:      10 <= roll < 1,000        (0.00099%)
/// - Legendary: 1 <= roll < 10            (0.000009%)
/// - Unique:    roll < 1                  (0.000001%)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum CubeRarity {
    Basic,
    Common,
    Uncommon,
    Rare,
    Legendary,
    Unique,
}

impl CubeRarity {
    /// Roll a rarity tier using weighted probability (out of 100,000,000).
    pub fn roll(rng: &mut impl Rng) -> Self {
        let r = rng.random_range(0u64..100_000_000);
        if r < 1 {
            CubeRarity::Unique
        } else if r < 10 {
            CubeRarity::Legendary
        } else if r < 1_000 {
            CubeRarity::Rare
        } else if r < 10_000 {
            CubeRarity::Uncommon
        } else if r < 100_000 {
            CubeRarity::Common
        } else {
            CubeRarity::Basic
        }
    }

    /// Number of leading zeros in the token_id for this rarity tier.
    pub fn leading_zeros(&self) -> usize {
        match self {
            CubeRarity::Basic => 0,
            CubeRarity::Common => 1,
            CubeRarity::Uncommon => 2,
            CubeRarity::Rare => 3,
            CubeRarity::Legendary => 4,
            CubeRarity::Unique => 5,
        }
    }

    /// Human-readable label for this rarity tier.
    pub fn label(&self) -> &str {
        match self {
            CubeRarity::Basic => "Basic",
            CubeRarity::Common => "Common",
            CubeRarity::Uncommon => "Uncommon",
            CubeRarity::Rare => "Rare",
            CubeRarity::Legendary => "Legendary",
            CubeRarity::Unique => "Unique",
        }
    }
}

impl std::fmt::Display for CubeRarity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Obfuscate an attribute name using SHA256(salt || name), truncated to 16 hex chars.
///
/// Produces deterministic, opaque identifiers so that attribute keys in NFT metadata
/// and contract definitions cannot be read in clear text.
fn obfuscate_attr(name: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ATTR_OBFUSCATION_SALT);
    hasher.update(name.as_bytes());
    let hash = hasher.finalize();
    hex::encode(&hash[..8])
}

/// Generate a 64-char hex token_id with leading zeros determined by rarity.
///
/// The first character after the zeros is guaranteed to be `1-f` (non-zero)
/// to prevent ambiguity between rarity tiers.
pub fn generate_token_id(rarity: &CubeRarity, rng: &mut impl Rng) -> String {
    let zeros = rarity.leading_zeros();
    let remaining = 64 - zeros;

    let mut id = String::with_capacity(64);

    // Leading zeros
    for _ in 0..zeros {
        id.push('0');
    }

    // First non-zero character (1-f) to prevent tier ambiguity
    if remaining > 0 {
        id.push_str(&format!("{:x}", rng.random_range(1u8..16)));
    }

    // Fill remaining characters with random hex
    for _ in 1..remaining {
        id.push_str(&format!("{:x}", rng.random_range(0u8..16)));
    }

    id
}

/// Derive public keys (ECDSA k256 + X25519) from a secp256k1 private key hex.
/// Returns `(public_key_hex, x25519_pubkey_hex)` or None on error.
///
/// Matches the derivation logic in `pms-wallet/src/wallet.rs`:
/// - ECDSA public key from secp256k1 scalar
/// - X25519 key derived via HKDF-SHA256(ecdsa_privkey, "pms/x25519-sk/v1")
fn derive_public_keys_from_privkey_hex(private_key_hex: &str) -> Option<(String, String)> {
    use k256::ecdsa::SigningKey;
    use k256::elliptic_curve::sec1::ToEncodedPoint;

    // 1. Parse secp256k1 private key (32 bytes hex)
    let priv_bytes = hex::decode(private_key_hex).ok()?;
    let signing_key = SigningKey::from_slice(&priv_bytes).ok()?;

    // 2. Derive ECDSA public key (k256/secp256k1) — 33 bytes compressed
    let verifying_key = signing_key.verifying_key();
    let pub_point = verifying_key.to_encoded_point(true); // compressed
    let pub_hex = hex::encode(pub_point.as_bytes());

    // 3. Derive X25519 keypair via HKDF-SHA256 (same as pms-wallet)
    let hk = Hkdf::<Sha256>::new(None, &priv_bytes);
    let mut sk_bytes = [0u8; 32];
    hk.expand(b"pms/x25519-sk/v1", &mut sk_bytes).ok()?;
    let sk = StaticSecret::from(sk_bytes);
    let xpk = XPublic::from(&sk);
    let x25519_hex = hex::encode(xpk.as_bytes());

    Some((pub_hex, x25519_hex))
}

/// Derive a bech32m address from ECDSA public key + X25519 public key.
/// Matches the address derivation in `pms-wallet/src/wallet.rs::get_address()`.
fn derive_address_from_keys(ecdsa_pub_hex: &str, x25519_pub_hex: &str, hrp: &str) -> String {
    use bech32::{ToBase32, Variant, encode};

    let pub_bytes = hex::decode(ecdsa_pub_hex).expect("valid pub hex");
    let hash = Sha256::digest(&pub_bytes);
    let h20 = &hash[..20];
    let xpk = hex::decode(x25519_pub_hex).expect("valid x25519 hex");

    let mut payload = Vec::with_capacity(52);
    payload.extend_from_slice(h20);
    payload.extend_from_slice(&xpk);

    encode(hrp, payload.to_base32(), Variant::Bech32m).expect("valid bech32m")
}

/// Attributes of a cube NFT.
///
/// `rarity` is cosmetic (encoded in the token_id via leading zeros)
/// and does NOT affect the edenite reward formula.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CubeAttributes {
    pub weight: f64,
    pub size: f64,
    pub density: f64,
    pub rarity: CubeRarity,
}

impl CubeAttributes {
    /// Calculate edenite reward: (weight * size * density) / divisor.
    /// Rarity is purely cosmetic and does not affect the formula.
    pub fn edenite_reward(&self, divisor: f64) -> f64 {
        (self.weight * self.size * self.density) / divisor
    }

    /// Serialize attributes to JSON with SHA256-obfuscated key names.
    ///
    /// Produces something like `{"a1b2c3d4e5f60718": 543.2, ...}` instead of
    /// `{"weight": 543.2, ...}` — attribute keys are opaque hex identifiers.
    pub fn to_obfuscated_extra(&self) -> String {
        serde_json::json!({
            obfuscate_attr("weight"): self.weight,
            obfuscate_attr("size"): self.size,
            obfuscate_attr("density"): self.density,
        })
        .to_string()
    }

    /// Return obfuscated attribute names for smart contract registration.
    ///
    /// Must match the keys produced by [`to_obfuscated_extra`] so the contract
    /// engine can locate attribute values in the NFT's `extra` JSON.
    pub fn obfuscated_attr_names() -> Vec<String> {
        vec![
            obfuscate_attr("weight"),
            obfuscate_attr("size"),
            obfuscate_attr("density"),
        ]
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
    ///
    /// # Arguments
    /// * `coordinator_private_key_hex` - Coordinator's secp256k1 private key (32 bytes hex).
    ///   Used to derive public keys for ledger ownership.
    pub async fn setup(
        client: &DagClient,
        config: &GameConfig,
        coordinator_private_key_hex: Option<&str>,
    ) -> SimResult<Self> {
        let ledger_id = config.ledger_id.clone();
        let network_id = config.network_id.clone();

        // Derive coordinator public keys (ECDSA + X25519) for ledger ownership
        let (owner_pubkey, owner_x25519_pubkey, coordinator_address) =
            if let Some(privkey_hex) = coordinator_private_key_hex {
                match derive_public_keys_from_privkey_hex(privkey_hex) {
                    Some((pub_hex, x25519_hex)) => {
                        // Derive bech32m address for transfer fee contract
                        let addr = derive_address_from_keys(&pub_hex, &x25519_hex, "8e");
                        (Some(pub_hex), Some(x25519_hex), Some(addr))
                    }
                    None => {
                        tracing::warn!(
                            "Failed to derive public keys from coordinator private key — \
                             ledger will be created without owner keys"
                        );
                        (None, None, None)
                    }
                }
            } else {
                (None, None, None)
            };

        // 1. Create game ledger
        tracing::info!("Creating game ledger '{}'...", ledger_id);
        match client
            .create_ledger(&CreateLedgerRequest {
                id: ledger_id.clone(),
                network_id: network_id.clone(),
                prefix: ledger_id.clone(),
                symbol: config.symbol.clone(),
                owner_pubkey: owner_pubkey.clone(),
                owner_x25519_pubkey: owner_x25519_pubkey.clone(),
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
        // Attribute names are SHA256-obfuscated so the formula is not readable in clear text.
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
                        attribute_names: CubeAttributes::obfuscated_attr_names(),
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
        if let Some(ref coord_addr) = coordinator_address {
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
                            address: coord_addr.clone(),
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

            let rarity = CubeRarity::roll(&mut rng);
            let token_id = generate_token_id(&rarity, &mut rng);

            let attrs = CubeAttributes {
                weight: round2(rng.random_range(1.0..30.0)),
                size: round2(rng.random_range(0.5..5.0)),
                density: round2(rng.random_range(0.1..1.0)),
                rarity,
            };

            let extra = attrs.to_obfuscated_extra();
            (token_id, attrs, extra)
        };

        // Mint on Edenite ledger via admin route (coordinator-only)
        self.game_client
            .admin_mint_nft(&MintNftRequest {
                token_id: token_id.clone(),
                owner_address: owner_address.to_string(),
                owner_x25519_pubkey: owner_x25519_pubkey.to_string(),
                metadata: NftMetadataSim {
                    name: Some(format!(
                        "[{}] Cube Edenite #{}",
                        attrs.rarity.label(),
                        &token_id[..8]
                    )),
                    description: Some(format!("[{}]", attrs.rarity.label())),
                    uri: None,
                    nft_type: Some("cube".to_string()),
                    extra: Some(extra),
                },
            })
            .await?;

        let reward = attrs.edenite_reward(self.divisor);
        tracing::info!(
            "Minted [{}] cube {} → potential {:.10} EDN",
            attrs.rarity.label(),
            &token_id[..16],
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
                    let rarity = CubeRarity::roll(&mut rng);
                    let token_id = generate_token_id(&rarity, &mut rng);
                    let attrs = CubeAttributes {
                        weight: round2(rng.random_range(1.0..30.0)),
                        size: round2(rng.random_range(0.5..5.0)),
                        density: round2(rng.random_range(0.1..1.0)),
                        rarity,
                    };
                    let extra = attrs.to_obfuscated_extra();
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
            let rarity_label = attrs.rarity.label().to_string();
            let extra = extra.clone();

            tasks.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let result = client
                    .admin_mint_nft(&MintNftRequest {
                        token_id: token_id.clone(),
                        owner_address,
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
        tracing::info!(
            "send_edenite: {} EDN to {}... (asset={})",
            amount,
            &to_addr[..20.min(to_addr.len())],
            &self.edenite_asset_id,
        );
        let resp = self.game_client
            .send_simple(&SendSimpleRequest {
                private_key_b64: private_key_b64.to_string(),
                to: to_addr.to_string(),
                amount: amount.to_string(),
                asset_id: Some(self.edenite_asset_id.clone()),
            })
            .await?;
        let block_id = resp.data.block_id.as_deref().unwrap_or("?");
        let gas_fee = resp.data.fee.as_deref().unwrap_or("0");
        let transfer_fee = resp.data.transfer_fee.as_deref().unwrap_or("0");
        tracing::info!(
            "send_edenite: OK — block={}..., gas_fee={}, transfer_fee={}",
            &block_id[..16.min(block_id.len())],
            gas_fee,
            transfer_fee,
        );
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

    /// Get the edenite asset ID (for caching in agents).
    pub fn edenite_asset_id(&self) -> &str {
        &self.edenite_asset_id
    }

    /// Get the divisor (for logging in agents).
    pub fn divisor(&self) -> f64 {
        self.divisor
    }

    // ── Lock-free helpers ──────────────────────────────────────────────
    //
    // These methods split game operations into phases that run WITHOUT
    // holding the RwLock during HTTP calls, eliminating write-lock
    // serialization that was capping Eden TPS at ~60.
    //
    // Pattern: acquire write lock → quick registry op → drop lock →
    //          HTTP calls (no lock) → re-acquire lock → register results.

    /// Remove cubes from registry and return them with their attributes + total expected EDN.
    /// Quick operation (HashMap removes only, no I/O). Call under write lock, then drop lock.
    pub fn drain_cubes(&mut self, token_ids: &[String]) -> (Vec<(String, CubeAttributes)>, f64) {
        let mut drained = Vec::with_capacity(token_ids.len());
        let mut total_edn = 0.0;
        for tid in token_ids {
            if let Some(attrs) = self.cube_registry.remove(tid) {
                total_edn += attrs.edenite_reward(self.divisor);
                drained.push((tid.clone(), attrs));
            }
        }
        (drained, total_edn)
    }

    /// Restore drained cubes on burn failure (re-insert into registry).
    pub fn restore_cubes(&mut self, cubes: Vec<(String, CubeAttributes)>) {
        for (tid, attrs) in cubes {
            self.cube_registry.insert(tid, attrs);
        }
    }

    /// Execute a batch burn HTTP call. Static — does NOT hold any lock.
    pub async fn execute_burn_batch(
        client: &DagClient,
        private_key_b64: &str,
        token_ids: Vec<String>,
    ) -> SimResult<()> {
        client
            .burn_nft_batch_simple(&BurnNftBatchSimpleRequest {
                private_key_b64: private_key_b64.to_string(),
                token_ids,
            })
            .await?;
        Ok(())
    }

    /// Generate cube mint specs. Static — pure RNG, no lock needed.
    pub fn generate_mint_specs(count: usize) -> Vec<(String, CubeAttributes, String)> {
        if count == 0 {
            return vec![];
        }
        let mut rng = rand::rng();
        (0..count)
            .map(|_| {
                let rarity = CubeRarity::roll(&mut rng);
                let token_id = generate_token_id(&rarity, &mut rng);
                let attrs = CubeAttributes {
                    weight: round2(rng.random_range(1.0..30.0)),
                    size: round2(rng.random_range(0.5..5.0)),
                    density: round2(rng.random_range(0.1..1.0)),
                    rarity,
                };
                let extra = attrs.to_obfuscated_extra();
                (token_id, attrs, extra)
            })
            .collect()
    }

    /// Execute parallel mint HTTP calls. Static — does NOT hold any lock.
    /// Returns `(minted_cubes, ok_count, err_count)`.
    pub async fn execute_mints_parallel(
        client: &DagClient,
        specs: &[(String, CubeAttributes, String)],
        owner_address: &str,
        owner_x25519_pubkey: &str,
    ) -> (Vec<(String, CubeAttributes)>, usize, usize) {
        if specs.is_empty() {
            return (vec![], 0, 0);
        }

        let sem = Arc::new(Semaphore::new(MINT_CONCURRENCY));
        let mut tasks = Vec::with_capacity(specs.len());

        for (token_id, attrs, extra) in specs {
            let client = client.clone();
            let sem = sem.clone();
            let token_id = token_id.clone();
            let owner_address = owner_address.to_string();
            let owner_x25519 = owner_x25519_pubkey.to_string();
            let attrs_clone = attrs.clone();
            let rarity_label = attrs.rarity.label().to_string();
            let extra = extra.clone();

            tasks.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let result = client
                    .admin_mint_nft(&MintNftRequest {
                        token_id: token_id.clone(),
                        owner_address,
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
                    })
                    .await;
                (token_id, attrs_clone, result)
            }));
        }

        let mut minted = Vec::with_capacity(specs.len());
        let mut ok_count = 0usize;
        let mut err_count = 0usize;

        for task in tasks {
            match task.await {
                Ok((token_id, attrs, Ok(_))) => {
                    minted.push((token_id, attrs));
                    ok_count += 1;
                }
                Ok((token_id, _, Err(e))) => {
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
            specs.len()
        );

        (minted, ok_count, err_count)
    }

    /// Register minted cubes in the local registry. Returns token IDs.
    pub fn register_minted(&mut self, cubes: Vec<(String, CubeAttributes)>) -> Vec<String> {
        cubes
            .into_iter()
            .map(|(tid, attrs)| {
                self.cube_registry.insert(tid.clone(), attrs);
                tid
            })
            .collect()
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
                rarity: CubeRarity::Basic,
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
        // Max values: weight 30kg, size 5cm, density 1.0
        let attrs = CubeAttributes {
            weight: 30.0,
            size: 5.0,
            density: 1.0,
            rarity: CubeRarity::Basic,
        };
        let reward = attrs.edenite_reward(DEFAULT_DIVISOR);
        // CRITICAL (v0.9.3): pin the calibration constant. Réutiliser
        // `150.0 / DEFAULT_DIVISOR` des deux côtés était tautologique — un
        // changement de divisor déplaçait les deux et n'était pas attrapé. Le
        // divisor doit matcher le contrat testnet (cf. CLAUDE.md).
        assert_eq!(DEFAULT_DIVISOR, 13_700.0, "game calibration divisor must stay 13700");
        // Valeur ABSOLUE attendue, indépendante du divisor : 150 / 13700.
        let golden = 0.010_948_905_109_489_05_f64;
        println!(
            "Reward for w=30, s=5.0, d=1.0: {reward:.12} EDN (golden {golden:.12})"
        );
        assert!(
            (reward - golden).abs() < 1e-9,
            "reward {reward} != golden {golden} (divisor calibration drift?)"
        );
    }

    #[test]
    fn obfuscate_attr_is_deterministic() {
        // Same input must always produce the same output
        let h1 = obfuscate_attr("weight");
        let h2 = obfuscate_attr("weight");
        assert_eq!(h1, h2, "obfuscate_attr must be deterministic");
        println!("obfuscate_attr(\"weight\") = {}", h1);

        // Different inputs must produce different outputs
        let h_size = obfuscate_attr("size");
        let h_density = obfuscate_attr("density");
        println!("obfuscate_attr(\"size\")    = {}", h_size);
        println!("obfuscate_attr(\"density\") = {}", h_density);
        assert_ne!(h1, h_size);
        assert_ne!(h1, h_density);
        assert_ne!(h_size, h_density);

        // Must be 16 hex chars (8 bytes)
        assert_eq!(h1.len(), 16, "obfuscated key must be 16 hex chars");
        assert_eq!(h_size.len(), 16);
        assert_eq!(h_density.len(), 16);

        // CRITICAL (v0.9.3): pin the EXACT documented hex keys. Determinism +
        // distinctness + length ne suffisaient pas — un changement de
        // ATTR_OBFUSCATION_SALT casserait silencieusement le match contrat↔NFT
        // sur testnet (reward = 0), tout en restant déterministe/16-chars.
        // Cf. CLAUDE.md "Edenite Cube System".
        assert_eq!(h1, "e6c84244b96fe92d", "weight obfuscated key drifted");
        assert_eq!(h_size, "7f41d7f9c843a618", "size obfuscated key drifted");
        assert_eq!(h_density, "c0d4a83995fb0edb", "density obfuscated key drifted");
    }

    #[test]
    fn obfuscated_extra_has_no_cleartext_attrs() {
        let attrs = CubeAttributes {
            weight: 543.2,
            size: 234.5,
            density: 15.3,
            rarity: CubeRarity::Basic,
        };
        let extra = attrs.to_obfuscated_extra();
        println!("Obfuscated extra JSON: {}", extra);

        // Must NOT contain clear-text attribute names
        assert!(!extra.contains("weight"), "extra must not contain 'weight'");
        assert!(!extra.contains("size"), "extra must not contain 'size'");
        assert!(!extra.contains("density"), "extra must not contain 'density'");

        // Must contain the obfuscated keys
        let key_w = obfuscate_attr("weight");
        let key_s = obfuscate_attr("size");
        let key_d = obfuscate_attr("density");
        assert!(extra.contains(&key_w), "extra must contain obfuscated weight key");
        assert!(extra.contains(&key_s), "extra must contain obfuscated size key");
        assert!(extra.contains(&key_d), "extra must contain obfuscated density key");

        // Must be valid JSON with the correct values
        let parsed: serde_json::Value = serde_json::from_str(&extra).expect("valid JSON");
        let val_w = parsed[&key_w].as_f64().expect("weight value");
        let val_s = parsed[&key_s].as_f64().expect("size value");
        let val_d = parsed[&key_d].as_f64().expect("density value");
        println!("Parsed values: w={}, s={}, d={}", val_w, val_s, val_d);
        assert!((val_w - 543.2).abs() < 0.01);
        assert!((val_s - 234.5).abs() < 0.01);
        assert!((val_d - 15.3).abs() < 0.01);
    }

    #[test]
    fn obfuscated_attr_names_match_extra_keys() {
        let names = CubeAttributes::obfuscated_attr_names();
        let attrs = CubeAttributes {
            weight: 100.0,
            size: 200.0,
            density: 3.0,
            rarity: CubeRarity::Rare,
        };
        let extra = attrs.to_obfuscated_extra();
        let parsed: serde_json::Value = serde_json::from_str(&extra).expect("valid JSON");

        println!("Contract attribute_names: {:?}", names);
        println!("Extra JSON keys: {:?}", parsed.as_object().unwrap().keys().collect::<Vec<_>>());

        for name in &names {
            assert!(
                parsed.get(name).is_some(),
                "contract attr '{}' missing from extra JSON",
                name
            );
        }
    }

    #[test]
    fn generate_token_id_leading_zeros() {
        let mut rng = rand::rng();

        for rarity in [
            CubeRarity::Basic,
            CubeRarity::Common,
            CubeRarity::Uncommon,
            CubeRarity::Rare,
            CubeRarity::Legendary,
            CubeRarity::Unique,
        ] {
            let zeros = rarity.leading_zeros();
            let tid = generate_token_id(&rarity, &mut rng);
            println!(
                "[{}] token_id = {} (expected {} leading zeros)",
                rarity.label(),
                tid,
                zeros
            );

            assert_eq!(tid.len(), 64, "token_id must be 64 chars");

            // Check leading zeros
            let leading = tid.chars().take_while(|c| *c == '0').count();
            assert!(
                leading >= zeros,
                "[{}] expected >= {} leading zeros, got {} in {}",
                rarity.label(),
                zeros,
                leading,
                tid
            );

            // For non-Basic tiers, the first non-zero char must be 1-f
            if zeros > 0 && zeros < 64 {
                let first_nonzero = tid.chars().nth(zeros).unwrap();
                assert!(
                    first_nonzero != '0',
                    "[{}] first char after zeros must be 1-f, got '0' in {}",
                    rarity.label(),
                    tid
                );
            }
        }
    }

    #[test]
    fn rarity_roll_distribution() {
        // Roll 100K times and check that Basic dominates
        let mut rng = rand::rng();
        let mut counts = [0u32; 6]; // Basic, Common, Uncommon, Rare, Legendary, Unique

        let n = 100_000;
        for _ in 0..n {
            match CubeRarity::roll(&mut rng) {
                CubeRarity::Basic => counts[0] += 1,
                CubeRarity::Common => counts[1] += 1,
                CubeRarity::Uncommon => counts[2] += 1,
                CubeRarity::Rare => counts[3] += 1,
                CubeRarity::Legendary => counts[4] += 1,
                CubeRarity::Unique => counts[5] += 1,
            }
        }

        println!("Rarity distribution over {} rolls:", n);
        println!("  Basic:     {} ({:.2}%)", counts[0], counts[0] as f64 / n as f64 * 100.0);
        println!("  Common:    {} ({:.4}%)", counts[1], counts[1] as f64 / n as f64 * 100.0);
        println!("  Uncommon:  {} ({:.4}%)", counts[2], counts[2] as f64 / n as f64 * 100.0);
        println!("  Rare:      {} ({:.6}%)", counts[3], counts[3] as f64 / n as f64 * 100.0);
        println!("  Legendary: {} ({:.6}%)", counts[4], counts[4] as f64 / n as f64 * 100.0);
        println!("  Unique:    {} ({:.8}%)", counts[5], counts[5] as f64 / n as f64 * 100.0);

        // Basic must be the vast majority (>99%)
        assert!(
            counts[0] as f64 / n as f64 > 0.99,
            "Basic should be >99%, got {:.2}%",
            counts[0] as f64 / n as f64 * 100.0
        );
    }

    #[test]
    fn rarity_reward_is_independent() {
        // Same attributes, different rarities → same reward
        let attrs_basic = CubeAttributes {
            weight: 500.0,
            size: 300.0,
            density: 10.0,
            rarity: CubeRarity::Basic,
        };
        let attrs_legendary = CubeAttributes {
            weight: 500.0,
            size: 300.0,
            density: 10.0,
            rarity: CubeRarity::Legendary,
        };

        let r1 = attrs_basic.edenite_reward(DEFAULT_DIVISOR);
        let r2 = attrs_legendary.edenite_reward(DEFAULT_DIVISOR);
        println!(
            "Basic reward: {:.10}, Legendary reward: {:.10} (should be equal)",
            r1, r2
        );
        assert!(
            (r1 - r2).abs() < 1e-15,
            "rarity must not affect reward: {} vs {}",
            r1,
            r2
        );
    }
}
