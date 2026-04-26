use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct SimConfig {
    pub server: ServerTarget,
    #[serde(default)]
    pub gemini: Option<GeminiConfig>,
    pub simulation: SimulationParams,
    #[serde(default)]
    pub tui: TuiConfig,
    #[serde(default)]
    pub web: WebConfig,
    /// Inline agent definitions (backward compat)
    #[serde(default)]
    pub agents: Vec<AgentDef>,
    /// External agent definition files (relative to config dir)
    #[serde(default)]
    pub agent_files: Vec<String>,
    /// Coordinator wallet credentials (for coordinator agent)
    #[serde(default)]
    pub coordinator: Option<CoordinatorConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CoordinatorConfig {
    /// Private key in hex (64 chars). Supports "env:VAR" syntax.
    pub private_key_hex: String,
    /// Coordinator address. Supports "env:VAR" syntax.
    pub address: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerTarget {
    pub url: String,
    #[serde(default)]
    pub admin_token: Option<String>,
    /// PMS API key (X-API-Key header). Use "env:PMS_API_KEY" to read from env.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub ledger_id: Option<String>,
    #[serde(default = "default_true")]
    pub accept_invalid_certs: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeminiConfig {
    /// API key (mode api_key). Utiliser "env:GEMINI_API_KEY" pour lire depuis env.
    #[serde(default)]
    pub api_key: Option<String>,
    /// OAuth client ID (mode oauth — connexion via navigateur)
    #[serde(default)]
    pub client_id: Option<String>,
    /// OAuth client secret
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default = "default_gemini_model")]
    pub model: String,
}

fn default_gemini_model() -> String {
    "gemini-2.0-flash".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct SimulationParams {
    #[serde(default)]
    pub duration_secs: u64,
    #[serde(default = "default_tick_ms")]
    pub base_tick_ms: u64,
    /// Amount to faucet per agent for initial PMS (default: "50.00")
    #[serde(default = "default_faucet_amount")]
    pub faucet_amount: String,
    /// Optional single-game config (legacy, pre-v0.7.5). When `games`
    /// below is empty AND this is `Some`, it's promoted to the first
    /// (and only) entry in `games`.
    #[serde(default)]
    pub game: Option<GameConfig>,
    /// Multi-ledger game configs (recommendation #2, v0.7.5). Each
    /// entry boots an independent game engine on its own ledger with
    /// its own EDN-equivalent token. Agents pick which game to play
    /// via `[agents.game].game_index` (defaults to 0).
    ///
    /// Either `[simulation.game]` (single) or `[[simulation.games]]`
    /// (array) is honoured — `games` takes precedence when both exist.
    #[serde(default)]
    pub games: Vec<GameConfig>,
}

impl SimulationParams {
    /// Resolve the list of game configs to boot. Combines the legacy
    /// single `game` field with the new `games` array — caller gets a
    /// uniform `Vec` regardless of which TOML shape was used.
    pub fn resolved_games(&self) -> Vec<GameConfig> {
        if !self.games.is_empty() {
            self.games.clone()
        } else if let Some(g) = &self.game {
            vec![g.clone()]
        } else {
            Vec::new()
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GameConfig {
    /// Ledger ID for the game ledger (e.g. "eden")
    pub ledger_id: String,
    /// Network ID for the game ledger
    pub network_id: String,
    /// Native token symbol for the game ledger (e.g. "EDN")
    #[serde(default)]
    pub symbol: Option<String>,
    /// Divisor for the edenite reward formula (default: 19_300_000_000)
    #[serde(default)]
    pub divisor: Option<f64>,
    /// PMS amount to deposit into the ledger's gas pool at setup (anti-spam).
    /// Default: "10000" PMS — enough for ~10M transactions at 0.001 gas/tx.
    #[serde(default = "default_gas_pool_deposit")]
    pub gas_pool_deposit: String,
}

fn default_gas_pool_deposit() -> String {
    "10000".to_string()
}

fn default_faucet_amount() -> String {
    "50.00".to_string()
}
fn default_tick_ms() -> u64 {
    1000
}
fn default_true() -> bool {
    true
}

// ════════════════════════════════════════════════════════════════════════════
// Agent definition
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Deserialize)]
pub struct AgentDef {
    pub count: usize,
    #[serde(default = "default_agent_interval")]
    pub interval_ms: u64,
    #[serde(default)]
    pub name_prefix: Option<String>,
    #[serde(default = "default_ai_interval")]
    pub ai_interval: u32,
    pub behavior: AgentBehavior,
    /// Per-agent-group game settings (overrides global defaults)
    #[serde(default)]
    pub game: Option<AgentGameConfig>,
}

/// Per-agent game configuration
#[derive(Debug, Clone, Deserialize)]
pub struct AgentGameConfig {
    /// Whether game loop is enabled for this agent group (default: true)
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Number of cubes to mint per agent at startup (default: 5)
    #[serde(default = "default_cubes_per_agent")]
    pub cubes_per_agent: usize,
    /// Min cubes to re-mint when depleted (default: 80)
    #[serde(default = "default_cubes_remint_min")]
    pub cubes_remint_min: usize,
    /// Max cubes to re-mint when depleted (default: 120)
    #[serde(default = "default_cubes_remint_max")]
    pub cubes_remint_max: usize,
    /// Min % of EDN balance to send (default: 10.0)
    #[serde(default = "default_edn_send_min_pct")]
    pub edn_send_min_pct: f64,
    /// Max % of EDN balance to send (default: 50.0)
    #[serde(default = "default_edn_send_max_pct")]
    pub edn_send_max_pct: f64,
    /// Ticks to wait after burn before reminting (allows fee_distribution to deliver EDN).
    /// During cooldown, the agent checks for EDN balance each tick instead of reminting.
    /// Set to 0 to disable (old behavior). Default: 10 ticks.
    #[serde(default = "default_burn_cooldown_ticks")]
    pub burn_cooldown_ticks: u32,
    /// Number of sequential EDN transfers per tick during Phase 2 (default: 1).
    /// Like PMS `sends_per_tick`, multiplies EDN throughput without adding agents.
    /// Each send is sequential (UTXO chain from same wallet).
    #[serde(default = "default_edn_sends_per_tick")]
    pub edn_sends_per_tick: u32,

    /// Random jitter on `burn_cooldown_ticks` to spread bursts across the
    /// fleet. Each agent's actual cooldown is `base × (1 + uniform(-j, +j))`
    /// where `j` is this fraction. Default: `0.0` (no jitter — every
    /// agent fires at the same tick after a synchronised event, which
    /// produces concentrated bursts). For a 100-agent prod scenario set
    /// to `0.2` (±20%) so burns spread over a ~24-min window instead of
    /// hammering the engine in one tick.
    #[serde(default = "default_burn_cooldown_jitter_pct")]
    pub burn_cooldown_jitter_pct: f64,

    /// Target accumulated cubes before burning (progressive-mining mode,
    /// recommendation #1). When `Some(N)` the agent mines `mint_per_tick`
    /// cubes per tick until its inventory hits N, then burns the lot
    /// and enters cooldown. When `None` the legacy bulk-batch behaviour
    /// (bulk-mint `cubes_remint_min..max` then immediately burn) is used
    /// — required for back-compat with `agents_dev.toml` /
    /// `agents_docker.toml` and any other config that hasn't migrated.
    #[serde(default)]
    pub target_cubes: Option<usize>,

    /// In progressive-mining mode (when `target_cubes` is set), how many
    /// cubes to mint per tick during the accumulation phase. Default: 1
    /// — at a 10s tick that yields 6 cubes/min, matching the EDN-clicker
    /// production cadence.
    #[serde(default = "default_mint_per_tick")]
    pub mint_per_tick: usize,

    /// Index into `[[simulation.games]]` for the game this agent group
    /// plays. Default: 0 (first game). Used by the multi-ledger game
    /// dispatch (recommendation #2, v0.7.5) to spread the population
    /// across N independent EDN-equivalent ledgers — set different
    /// indices on different agent groups to load all ledgers
    /// simultaneously.
    #[serde(default)]
    pub game_index: usize,
}

impl Default for AgentGameConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cubes_per_agent: 5,
            cubes_remint_min: 80,
            cubes_remint_max: 120,
            edn_send_min_pct: 10.0,
            edn_send_max_pct: 50.0,
            burn_cooldown_ticks: 10,
            edn_sends_per_tick: 1,
            burn_cooldown_jitter_pct: 0.0,
            target_cubes: None,
            mint_per_tick: 1,
            game_index: 0,
        }
    }
}

fn default_cubes_per_agent() -> usize {
    5
}
fn default_cubes_remint_min() -> usize {
    80
}
fn default_cubes_remint_max() -> usize {
    120
}
fn default_edn_send_min_pct() -> f64 {
    10.0
}
fn default_edn_send_max_pct() -> f64 {
    50.0
}
fn default_burn_cooldown_ticks() -> u32 {
    10
}
fn default_edn_sends_per_tick() -> u32 {
    1
}
fn default_burn_cooldown_jitter_pct() -> f64 {
    0.0
}
fn default_mint_per_tick() -> usize {
    1
}

fn default_agent_interval() -> u64 {
    2000
}
fn default_ai_interval() -> u32 {
    5
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentBehavior {
    Smart {
        #[serde(default = "default_system_prompt")]
        system_prompt: String,
    },
    Random {
        #[serde(default = "default_min_amount")]
        min_amount: f64,
        #[serde(default = "default_max_amount")]
        max_amount: f64,
        #[serde(default = "default_send_probability")]
        send_probability: f64,
        /// Number of PMS transactions to send per tick (default: 1).
        /// Increase to multiply PMS throughput without adding agents.
        /// Each send is sequential (UTXO chain from same wallet).
        #[serde(default = "default_sends_per_tick")]
        sends_per_tick: u32,
    },
    Observer,
    Coordinator {
        #[serde(default = "default_coord_min_amount")]
        min_amount: f64,
        #[serde(default = "default_coord_max_amount")]
        max_amount: f64,
        #[serde(default = "default_coord_send_probability")]
        send_probability: f64,
        /// Number of PMS transactions to send per tick (default: 1).
        #[serde(default = "default_sends_per_tick")]
        sends_per_tick: u32,
    },
    /// Adversarial spammer (recommendation #3, v0.7.5). Sends a random
    /// mix of malformed / unauthenticated / double-spending / unsigned
    /// transactions to validate the engine's rejection paths. Each tick
    /// picks one attack from `attacks` (uniform), fires it, and records
    /// the response code in the simulator's `pms_simulator_tx_failed_total`
    /// counter. Used together with the legitimate flood-spammer to
    /// stress both the rate-limit + the validation pipeline.
    Adversarial {
        /// List of attack kinds to randomise across each tick. Empty
        /// = all built-in attacks. Possible values:
        ///   - `bad_signature`     : valid tx, garbled signature
        ///   - `bad_utxo`          : input refs a non-existent UTXO
        ///   - `double_spend`      : reuses a stale UTXO ref
        ///   - `over_balance`      : amount > available balance
        ///   - `malformed_json`    : raw POST with broken JSON
        ///   - `no_auth`           : drops the X-API-Key header
        ///   - `replay`            : resubmits the previous successful block
        #[serde(default)]
        attacks: Vec<String>,
        /// Sends per tick (each picks an independent attack). Default: 1.
        #[serde(default = "default_sends_per_tick")]
        sends_per_tick: u32,
    },
}

fn default_system_prompt() -> String {
    "Tu es un utilisateur de paiement DAG-PMS. Envoie des montants variés aux autres agents, observe les balances, et communique tes intentions.".to_string()
}
fn default_min_amount() -> f64 {
    0.1
}
fn default_max_amount() -> f64 {
    5.0
}
fn default_send_probability() -> f64 {
    0.5
}
fn default_coord_min_amount() -> f64 {
    0.1
}
fn default_coord_max_amount() -> f64 {
    1.0
}
fn default_coord_send_probability() -> f64 {
    1.0
}
fn default_sends_per_tick() -> u32 {
    1
}

#[derive(Debug, Clone, Deserialize)]
pub struct TuiConfig {
    #[serde(default = "default_tui_refresh")]
    pub refresh_ms: u64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            refresh_ms: 250,
            enabled: true,
        }
    }
}

fn default_tui_refresh() -> u64 {
    250
}

#[derive(Debug, Clone, Deserialize)]
pub struct WebConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_web_port")]
    pub port: u16,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 9090,
        }
    }
}

fn default_web_port() -> u16 {
    9090
}

// ════════════════════════════════════════════════════════════════════════════
// Agent file loading
// ════════════════════════════════════════════════════════════════════════════

/// Container for agent definitions loaded from external files
#[derive(Debug, Deserialize)]
struct AgentFileContent {
    #[serde(default)]
    agents: Vec<AgentDef>,
}

impl SimConfig {
    /// Resolve env: prefixed values from environment variables
    pub fn resolve_secrets(&mut self) {
        if let Some(ref mut gemini) = self.gemini {
            if let Some(ref mut key) = gemini.api_key {
                resolve_env(key);
            }
            if let Some(ref mut id) = gemini.client_id {
                resolve_env(id);
            }
            if let Some(ref mut secret) = gemini.client_secret {
                resolve_env(secret);
            }
        }
        if let Some(ref token) = self.server.admin_token {
            if let Some(stripped) = token.strip_prefix("env:") {
                self.server.admin_token = std::env::var(stripped).ok();
            }
        }
        if let Some(ref key) = self.server.api_key {
            if let Some(stripped) = key.strip_prefix("env:") {
                self.server.api_key = std::env::var(stripped).ok();
            }
        }
        if let Some(ref mut coord) = self.coordinator {
            resolve_env(&mut coord.private_key_hex);
            resolve_env(&mut coord.address);
        }
    }

    /// Load all agent definitions: inline `[[agents]]` + external `agent_files`.
    /// `config_path` is the path to the main config file (used to resolve relative paths).
    pub fn load_all_agents(&mut self, config_path: &str) -> Result<(), String> {
        if self.agent_files.is_empty() {
            return Ok(());
        }

        let config_dir = Path::new(config_path)
            .parent()
            .unwrap_or(Path::new("."));

        for file_path in &self.agent_files {
            let full_path = config_dir.join(file_path);
            let content = std::fs::read_to_string(&full_path).map_err(|e| {
                format!("Cannot read agent file {}: {}", full_path.display(), e)
            })?;
            let file: AgentFileContent = toml::from_str(&content).map_err(|e| {
                format!("Cannot parse agent file {}: {}", full_path.display(), e)
            })?;
            self.agents.extend(file.agents);
        }

        Ok(())
    }

    /// Check if any agent needs Gemini AI
    pub fn needs_gemini(&self) -> bool {
        self.agents.iter().any(|a| matches!(a.behavior, AgentBehavior::Smart { .. }))
    }

    /// Validate that all required credentials are present and non-empty.
    /// Returns a list of error messages for each missing credential.
    /// Call this after [`resolve_secrets()`] to catch unresolved `env:` values.
    pub fn validate_credentials(&self) -> Vec<String> {
        let mut errors = Vec::new();

        // API key is required for gateway authentication
        match &self.server.api_key {
            None => errors.push("server.api_key is not set".to_string()),
            Some(key) if key.is_empty() || key.starts_with("env:") => {
                let var = key.strip_prefix("env:").unwrap_or("PMS_API_KEY");
                errors.push(format!("server.api_key: env var {var} is not set or empty"));
            }
            _ => {}
        }

        // Admin token is required for coordinator operations
        match &self.server.admin_token {
            None => errors.push("server.admin_token is not set".to_string()),
            Some(tok) if tok.is_empty() || tok.starts_with("env:") => {
                let var = tok.strip_prefix("env:").unwrap_or("PMS_ADMIN_TOKEN");
                errors.push(format!("server.admin_token: env var {var} is not set or empty"));
            }
            _ => {}
        }

        // Coordinator credentials are required for PMS distribution to agents
        match &self.coordinator {
            None => errors.push("[coordinator] section is missing from config".to_string()),
            Some(coord) => {
                if coord.private_key_hex.is_empty() || coord.private_key_hex.starts_with("env:") {
                    let var = coord.private_key_hex.strip_prefix("env:").unwrap_or("PMS_COORDINATOR_KEY");
                    errors.push(format!("coordinator.private_key_hex: env var {var} is not set or empty"));
                }
                if coord.address.is_empty() || coord.address.starts_with("env:") {
                    let var = coord.address.strip_prefix("env:").unwrap_or("PMS_COORDINATOR_ADDR");
                    errors.push(format!("coordinator.address: env var {var} is not set or empty"));
                }
            }
        }

        errors
    }
}

/// Resolve `env:VAR_NAME` syntax from environment variables.
/// Silently keeps the original value if env var is not found (non-env: values pass through).
/// Use [`SimConfig::validate_credentials`] after resolving to check for missing required values.
fn resolve_env(val: &mut String) {
    if let Some(stripped) = val.strip_prefix("env:") {
        match std::env::var(stripped) {
            Ok(env_val) if !env_val.is_empty() => *val = env_val,
            _ => {
                // Keep the "env:VAR" string so validate_credentials() can detect it
            }
        }
    }
}

/// Maximum number of startup attempts before giving up.
pub const MAX_STARTUP_ATTEMPTS: u64 = 10;
/// Base delay between startup attempts (seconds). Increases by RETRY_INCREMENT each attempt.
pub const RETRY_BASE_DELAY_SECS: u64 = 30;
/// Seconds added to delay on each successive attempt.
pub const RETRY_INCREMENT_SECS: u64 = 15;
