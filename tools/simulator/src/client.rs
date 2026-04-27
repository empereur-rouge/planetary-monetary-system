use crate::config::ServerTarget;
use crate::error::{SimError, SimResult};
use crate::types::*;
use reqwest::{Client, RequestBuilder, Response};
use std::time::{Duration, Instant};

/// Max retries on 429 Too Many Requests
const MAX_RETRIES: u32 = 5;
/// Base delay between retries (doubles each attempt: 500ms, 1s, 2s, 4s, 8s)
const BASE_RETRY_DELAY_MS: u64 = 500;
/// Max delay per retry (cap)
const MAX_RETRY_DELAY: Duration = Duration::from_secs(10);

/// Wrapper with timing info
pub struct TimedResponse<T> {
    pub data: T,
    pub latency: Duration,
}

/// Typed HTTP client for DAG-PMS API (via gateway)
#[derive(Clone)]
pub struct DagClient {
    http: Client,
    base_url: String,
    prefix: String,
    admin_token: Option<String>,
    api_key: Option<String>,
}

impl DagClient {
    pub fn new(target: &ServerTarget) -> Self {
        // Pool sized for 1000+ concurrent agents driving the gateway
        // simultaneously. reqwest's default `pool_max_idle_per_host`
        // is 32 — at production-scale load (1000 clickers × 6 cubes/min
        // mint + EDN sends + PMS sends across 4 game ledgers) the
        // per-agent send loops queue on the connection pool and
        // observed TPS collapses to 32× single-call latency. Bumping
        // to 512 idle conns lets every burst send fan out without
        // serialising on a half-closed pool. `tcp_keepalive(60s)`
        // keeps long-lived connections warm so we don't pay TLS
        // handshake on every request after a quiet patch (e.g. burn
        // cooldown).
        let http = Client::builder()
            .timeout(Duration::from_secs(15))
            .danger_accept_invalid_certs(target.accept_invalid_certs)
            .pool_max_idle_per_host(512)
            .pool_idle_timeout(Some(Duration::from_secs(90)))
            .tcp_keepalive(Some(Duration::from_secs(60)))
            .tcp_nodelay(true)
            .build()
            .expect("reqwest client build");

        let prefix = match &target.ledger_id {
            Some(id) => format!("/l/{}", id),
            None => String::new(),
        };

        Self {
            http,
            base_url: target.url.trim_end_matches('/').to_string(),
            prefix,
            admin_token: target.admin_token.clone(),
            api_key: target.api_key.clone(),
        }
    }

    /// Create a new client pointing to a specific ledger (prefix /l/{ledger_id})
    pub fn with_ledger(&self, ledger_id: &str) -> Self {
        Self {
            http: self.http.clone(),
            base_url: self.base_url.clone(),
            prefix: format!("/l/{}", ledger_id),
            admin_token: self.admin_token.clone(),
            api_key: self.api_key.clone(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}{}", self.base_url, self.prefix, path)
    }

    fn admin_url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn auth_header(&self) -> Option<String> {
        self.admin_token
            .as_ref()
            .map(|t| format!("Bearer {}", t))
    }

    /// Apply X-API-Key header if configured
    fn with_api_key(&self, builder: RequestBuilder) -> RequestBuilder {
        match &self.api_key {
            Some(key) => builder.header("X-API-Key", key),
            None => builder,
        }
    }

    /// Send a request with automatic retry on 429 (Too Many Requests).
    /// Uses exponential backoff: 500ms, 1s, 2s, 4s, 8s (capped at 10s).
    async fn send_with_retry(&self, builder: RequestBuilder) -> Result<Response, reqwest::Error> {
        let mut attempt = 0u32;
        let mut current = builder;

        loop {
            let cloned = current.try_clone();
            let resp = current.send().await?;

            if resp.status() == 429 && attempt < MAX_RETRIES {
                let delay = Duration::from_millis(BASE_RETRY_DELAY_MS * 2u64.pow(attempt))
                    .min(MAX_RETRY_DELAY);

                tracing::warn!(
                    "Rate limited (429), retrying in {:?} (attempt {}/{})",
                    delay,
                    attempt + 1,
                    MAX_RETRIES
                );
                tokio::time::sleep(delay).await;
                attempt += 1;

                match cloned {
                    Some(b) => current = b,
                    None => return Ok(resp), // can't retry streaming body
                }
            } else {
                return Ok(resp);
            }
        }
    }

    // ════════════════════════════════════════════════════════════════
    // Public API
    // ════════════════════════════════════════════════════════════════

    /// Raw POST with a body string and the configured X-API-Key header.
    /// The caller controls every byte of the body — used by the
    /// adversarial agent for malformed-JSON attacks. Returns the raw
    /// `reqwest::Response` so the caller can inspect the status code
    /// without going through the typed deserialiser.
    pub async fn raw_post_json(
        &self,
        path: &str,
        body: &str,
    ) -> Result<Response, reqwest::Error> {
        let builder = self
            .http
            .post(self.url(path))
            .header("Content-Type", "application/json")
            .body(body.to_string());
        let builder = self.with_api_key(builder);
        builder.send().await
    }

    /// Raw POST with the X-API-Key header **dropped**. Used by the
    /// adversarial agent's `no_auth` attack — the gateway should respond
    /// with 401 before the request ever reaches the engine.
    pub async fn raw_post_no_auth(
        &self,
        path: &str,
        body: String,
    ) -> Result<Response, reqwest::Error> {
        self.http
            .post(self.url(path))
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
    }

    pub async fn health_check(&self) -> SimResult<bool> {
        let resp = self
            .http
            .get(format!("{}/livez", self.base_url))
            .send()
            .await?;
        Ok(resp.status().is_success())
    }

    pub async fn create_wallet(&self) -> SimResult<TimedResponse<WalletInfo>> {
        let start = Instant::now();
        let builder = self
            .http
            .post(self.url("/v1/wallet/create"))
            .json(&serde_json::json!({}));
        let builder = self.with_api_key(builder);

        let resp = self.send_with_retry(builder).await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SimError::ServerError {
                status,
                message: body,
            });
        }

        let data: WalletInfo = resp.json().await?;
        Ok(TimedResponse {
            data,
            latency: start.elapsed(),
        })
    }

    pub async fn send_simple(
        &self,
        req: &SendSimpleRequest,
    ) -> SimResult<TimedResponse<SendResponse>> {
        let start = Instant::now();
        let builder = self.http.post(self.url("/v1/wallet/send-simple")).json(req);
        let builder = self.with_api_key(builder);
        let resp = self.send_with_retry(builder).await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SimError::ServerError {
                status,
                message: body,
            });
        }

        let data: SendResponse = resp.json().await?;
        Ok(TimedResponse {
            data,
            latency: start.elapsed(),
        })
    }

    pub async fn faucet(
        &self,
        to: &str,
        amount: &str,
    ) -> SimResult<TimedResponse<FaucetResponse>> {
        let start = Instant::now();
        let mut builder = self
            .http
            .post(self.admin_url("/admin/faucet"))
            .json(&FaucetRequest {
                to: to.to_string(),
                amount: amount.to_string(),
            });

        if let Some(auth) = self.auth_header() {
            builder = builder.header("Authorization", auth);
        }

        let resp = self.send_with_retry(builder).await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SimError::ServerError {
                status,
                message: body,
            });
        }

        let data: FaucetResponse = resp.json().await?;
        Ok(TimedResponse {
            data,
            latency: start.elapsed(),
        })
    }

    pub async fn balance(&self, address: &str) -> SimResult<String> {
        let builder = self
            .http
            .post(self.url("/v1/balance"))
            .json(&BalanceRequest {
                address: address.to_string(),
            });
        let builder = self.with_api_key(builder);

        let resp = self.send_with_retry(builder).await?;

        if !resp.status().is_success() {
            return Ok("0".to_string());
        }

        let data: BalanceResponse = resp.json().await?;
        Ok(data.balance)
    }

    pub async fn get_tips(&self, limit: usize) -> SimResult<Vec<String>> {
        let builder = self
            .http
            .post(self.url("/v1/dag/tips"))
            .json(&TipsRequest { limit });
        let builder = self.with_api_key(builder);

        let resp = self.send_with_retry(builder).await?;

        if !resp.status().is_success() {
            return Ok(vec![]);
        }

        let tips: Vec<String> = resp.json().await?;
        Ok(tips)
    }

    pub async fn get_supply(&self) -> SimResult<SupplyResponse> {
        let builder = self.with_api_key(self.http.get(self.url("/v1/supply")));
        let resp = self.send_with_retry(builder).await?;
        let data: SupplyResponse = resp.json().await?;
        Ok(data)
    }

    pub async fn list_tokens(&self) -> SimResult<serde_json::Value> {
        let builder = self.with_api_key(self.http.get(self.url("/v1/tokens")));
        let resp = self.send_with_retry(builder).await?;
        let data: serde_json::Value = resp.json().await?;
        Ok(data)
    }

    // ════════════════════════════════════════════════════════════════
    // Admin API — Ledger & Token
    // ════════════════════════════════════════════════════════════════

    /// POST /admin/ledgers/create (global admin, no ledger prefix)
    pub async fn create_ledger(
        &self,
        req: &CreateLedgerRequest,
    ) -> SimResult<CreateLedgerResponse> {
        let mut builder = self
            .http
            .post(self.admin_url("/admin/ledgers/create"))
            .json(req);

        if let Some(auth) = self.auth_header() {
            builder = builder.header("Authorization", auth);
        }

        let resp = self.send_with_retry(builder).await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SimError::ServerError {
                status,
                message: body,
            });
        }

        let data: CreateLedgerResponse = resp.json().await?;
        Ok(data)
    }

    /// POST /admin/tokens/create (uses ledger prefix)
    pub async fn create_token(
        &self,
        req: &CreateTokenRequest,
    ) -> SimResult<CreateTokenResponse> {
        let mut builder = self
            .http
            .post(self.url("/admin/tokens/create"))
            .json(req);

        if let Some(auth) = self.auth_header() {
            builder = builder.header("Authorization", auth);
        }

        let resp = self.send_with_retry(builder).await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SimError::ServerError {
                status,
                message: body,
            });
        }

        let data: CreateTokenResponse = resp.json().await?;
        Ok(data)
    }

    /// POST /admin/tokens/mint (uses ledger prefix)
    pub async fn mint_token(
        &self,
        req: &MintTokenRequest,
    ) -> SimResult<MintTokenResponse> {
        let mut builder = self
            .http
            .post(self.url("/admin/tokens/mint"))
            .json(req);

        if let Some(auth) = self.auth_header() {
            builder = builder.header("Authorization", auth);
        }

        let resp = self.send_with_retry(builder).await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SimError::ServerError {
                status,
                message: body,
            });
        }

        let data: MintTokenResponse = resp.json().await?;
        Ok(data)
    }

    // ════════════════════════════════════════════════════════════════
    // Admin API — Contracts
    // ════════════════════════════════════════════════════════════════

    /// POST /admin/contracts — Register a smart contract (global admin)
    pub async fn register_contract(
        &self,
        req: &RegisterContractRequest,
    ) -> SimResult<RegisterContractResponse> {
        let mut builder = self
            .http
            .post(self.admin_url("/admin/contracts"))
            .json(req);

        if let Some(auth) = self.auth_header() {
            builder = builder.header("Authorization", auth);
        }

        let resp = self.send_with_retry(builder).await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SimError::ServerError {
                status,
                message: body,
            });
        }

        let data: RegisterContractResponse = resp.json().await?;
        Ok(data)
    }

    // ════════════════════════════════════════════════════════════════
    // Admin API — Gas Pool
    // ════════════════════════════════════════════════════════════════

    /// POST /admin/gas-pool/deposit — Fund a ledger's gas pool (admin auth)
    pub async fn deposit_gas(
        &self,
        req: &GasPoolDepositRequest,
    ) -> SimResult<GasPoolDepositResponse> {
        let mut builder = self
            .http
            .post(self.admin_url("/admin/gas-pool/deposit"))
            .json(req);

        if let Some(auth) = self.auth_header() {
            builder = builder.header("Authorization", auth);
        }

        let resp = self.send_with_retry(builder).await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SimError::ServerError {
                status,
                message: body,
            });
        }

        let data: GasPoolDepositResponse = resp.json().await?;
        Ok(data)
    }

    // ════════════════════════════════════════════════════════════════
    // NFT API
    // ════════════════════════════════════════════════════════════════

    /// POST /v1/nft/mint — Mint an NFT on main ledger (API key auth)
    pub async fn mint_nft(
        &self,
        req: &MintNftRequest,
    ) -> SimResult<MintNftResponse> {
        let builder = self
            .http
            .post(self.url("/v1/nft/mint"))
            .json(req);
        let builder = self.with_api_key(builder);

        let resp = self.send_with_retry(builder).await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SimError::ServerError {
                status,
                message: body,
            });
        }

        let data: MintNftResponse = resp.json().await?;
        Ok(data)
    }

    /// POST /admin/nft/mint — Mint an NFT on custom ledger (admin auth required)
    /// Security: NFT minting on custom ledgers is admin-only to prevent
    /// unauthorized NFT creation that could exploit smart contracts.
    pub async fn admin_mint_nft(
        &self,
        req: &MintNftRequest,
    ) -> SimResult<MintNftResponse> {
        let mut builder = self
            .http
            .post(self.url("/admin/nft/mint"))
            .json(req);

        if let Some(auth) = self.auth_header() {
            builder = builder.header("Authorization", auth);
        }

        let resp = self.send_with_retry(builder).await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SimError::ServerError {
                status,
                message: body,
            });
        }

        let data: MintNftResponse = resp.json().await?;
        Ok(data)
    }

    // ════════════════════════════════════════════════════════════════
    // UTXO / Token Balance API
    // ════════════════════════════════════════════════════════════════

    /// GET /v1/wallet/{address}/utxos — Query all UTXOs for an address
    pub async fn get_utxos(&self, address: &str) -> SimResult<UtxosResponse> {
        let builder = self.with_api_key(
            self.http
                .get(self.url(&format!("/v1/wallet/{}/utxos", address))),
        );

        let resp = self.send_with_retry(builder).await?;

        if !resp.status().is_success() {
            return Ok(UtxosResponse { utxos: vec![] });
        }

        let data: UtxosResponse = resp.json().await?;
        Ok(data)
    }

    /// Query the balance for a specific asset_id by summing matching UTXOs.
    /// Returns the total balance as a string (for precision).
    pub async fn token_balance(&self, address: &str, asset_id: &str) -> SimResult<String> {
        let utxos = self.get_utxos(address).await?;
        let total: f64 = utxos
            .utxos
            .iter()
            .filter(|u| u.asset_id.as_deref() == Some(asset_id))
            .filter_map(|u| u.amount.parse::<f64>().ok())
            .sum();
        Ok(format!("{:.10}", total))
    }

    /// POST /v1/nft/burn-batch-simple — Burn multiple NFTs in one block
    pub async fn burn_nft_batch_simple(
        &self,
        req: &BurnNftBatchSimpleRequest,
    ) -> SimResult<BurnNftBatchSimpleResponse> {
        let builder = self
            .http
            .post(self.url("/v1/nft/burn-batch-simple"))
            .json(req);
        let builder = self.with_api_key(builder);

        let resp = self.send_with_retry(builder).await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SimError::ServerError {
                status,
                message: body,
            });
        }

        let data: BurnNftBatchSimpleResponse = resp.json().await?;
        Ok(data)
    }

    /// POST /v1/nft/burn-simple — Burn an NFT (server builds + signs block)
    pub async fn burn_nft_simple(
        &self,
        req: &BurnNftSimpleRequest,
    ) -> SimResult<BurnNftSimpleResponse> {
        let builder = self
            .http
            .post(self.url("/v1/nft/burn-simple"))
            .json(req);
        let builder = self.with_api_key(builder);

        let resp = self.send_with_retry(builder).await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SimError::ServerError {
                status,
                message: body,
            });
        }

        let data: BurnNftSimpleResponse = resp.json().await?;
        Ok(data)
    }
}
