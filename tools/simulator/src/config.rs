use serde::Deserialize;

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
    #[serde(default)]
    pub agents: Vec<AgentDef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerTarget {
    pub url: String,
    #[serde(default)]
    pub admin_token: Option<String>,
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
    /// Amount of CUBEs to burn per agent for initial PMS (default: 500.00)
    /// Rate: 10 CUBE = 1 PMS, so 500 CUBE → 50 PMS
    #[serde(default = "default_cubes_to_burn")]
    pub cubes_to_burn: String,
}

fn default_cubes_to_burn() -> String {
    "500.00".to_string()
}
fn default_tick_ms() -> u64 {
    1000
}
fn default_true() -> bool {
    true
}

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
    },
    Observer,
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
    }

    /// Check if any agent needs Gemini AI
    pub fn needs_gemini(&self) -> bool {
        self.agents.iter().any(|a| matches!(a.behavior, AgentBehavior::Smart { .. }))
    }
}

fn resolve_env(val: &mut String) {
    if let Some(stripped) = val.strip_prefix("env:") {
        if let Ok(env_val) = std::env::var(stripped) {
            *val = env_val;
        }
    }
}
