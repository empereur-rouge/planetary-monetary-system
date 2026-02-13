use crate::config::ServerTarget;
use crate::error::{SimError, SimResult};
use crate::types::*;
use reqwest::Client;
use std::time::{Duration, Instant};

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
}

impl DagClient {
    pub fn new(target: &ServerTarget) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(15))
            .danger_accept_invalid_certs(target.accept_invalid_certs)
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

    // ════════════════════════════════════════════════════════════════
    // Public API
    // ════════════════════════════════════════════════════════════════

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
        let resp = self
            .http
            .post(self.url("/v1/wallet/create"))
            .json(&serde_json::json!({}))
            .send()
            .await?;

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
        let resp = self.http.post(self.url("/v1/wallet/send-simple")).json(req).send().await?;

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
        let mut req_builder = self
            .http
            .post(self.admin_url("/admin/faucet"))
            .json(&FaucetRequest {
                to: to.to_string(),
                amount: amount.to_string(),
            });

        if let Some(auth) = self.auth_header() {
            req_builder = req_builder.header("Authorization", auth);
        }

        let resp = req_builder.send().await?;

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

    pub async fn cube_claim(
        &self,
        to: &str,
    ) -> SimResult<TimedResponse<CubeClaimResponse>> {
        let start = Instant::now();
        let resp = self
            .http
            .post(self.url("/v1/cube/claim"))
            .json(&CubeClaimRequest {
                to: to.to_string(),
            })
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SimError::ServerError {
                status,
                message: body,
            });
        }

        let data: CubeClaimResponse = resp.json().await?;
        Ok(TimedResponse {
            data,
            latency: start.elapsed(),
        })
    }

    pub async fn cube_burn(
        &self,
        private_key_b64: &str,
        amount: &str,
    ) -> SimResult<TimedResponse<CubeBurnResponse>> {
        let start = Instant::now();
        let resp = self
            .http
            .post(self.url("/v1/cube/burn"))
            .json(&CubeBurnRequest {
                private_key_b64: private_key_b64.to_string(),
                amount: amount.to_string(),
            })
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SimError::ServerError {
                status,
                message: body,
            });
        }

        let data: CubeBurnResponse = resp.json().await?;
        Ok(TimedResponse {
            data,
            latency: start.elapsed(),
        })
    }

    pub async fn balance(&self, address: &str) -> SimResult<String> {
        let resp = self
            .http
            .post(self.url("/v1/balance"))
            .json(&BalanceRequest {
                address: address.to_string(),
            })
            .send()
            .await?;

        if !resp.status().is_success() {
            return Ok("0".to_string());
        }

        let data: BalanceResponse = resp.json().await?;
        Ok(data.balance)
    }

    pub async fn get_tips(&self, limit: usize) -> SimResult<Vec<String>> {
        let resp = self
            .http
            .post(self.url("/v1/dag/tips"))
            .json(&TipsRequest { limit })
            .send()
            .await?;

        if !resp.status().is_success() {
            return Ok(vec![]);
        }

        let tips: Vec<String> = resp.json().await?;
        Ok(tips)
    }

    pub async fn get_supply(&self) -> SimResult<SupplyResponse> {
        let resp = self.http.get(self.url("/v1/supply")).send().await?;
        let data: SupplyResponse = resp.json().await?;
        Ok(data)
    }

    pub async fn list_tokens(&self) -> SimResult<serde_json::Value> {
        let resp = self.http.get(self.url("/v1/tokens")).send().await?;
        let data: serde_json::Value = resp.json().await?;
        Ok(data)
    }
}
