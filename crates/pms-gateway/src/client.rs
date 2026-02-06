use http::StatusCode;
use reqwest::Client;
use serde::de::DeserializeOwned;

pub struct EngineClient {
    base_url: String,
    http: Client,
}

impl EngineClient {
    pub fn new(base_url: &str) -> Self {
        // Create client that accepts self-signed certs (for internal HTTPS)
        let http = Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, reqwest::Error> {
        let url = format!("{}{}", self.base_url, path);
        self.http.get(&url).send().await?.json().await
    }

    pub async fn post<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, reqwest::Error> {
        let url = format!("{}{}", self.base_url, path);
        self.http.post(&url).json(body).send().await?.json().await
    }

    /// Proxy POST request - forwards raw body to Engine
    pub async fn proxy_post(
        &self,
        path: &str,
        headers: http::HeaderMap,
        body: axum::body::Bytes,
    ) -> Result<(StatusCode, String), anyhow::Error> {
        let url = format!("{}{}", self.base_url, path);

        let mut req = self.http.post(&url);

        // Forward Authorization header if present
        if let Some(auth) = headers.get("authorization") {
            req = req.header("Authorization", auth);
        }
        req = req.header("Content-Type", "application/json");

        let resp = req.body(body.to_vec()).send().await?;
        let status = StatusCode::from_u16(resp.status().as_u16())?;
        let body = resp.text().await?;
        Ok((status, body))
    }

    /// Proxy GET request - forwards to Engine
    pub async fn proxy_get(
        &self,
        path: &str,
        headers: http::HeaderMap,
    ) -> Result<(StatusCode, String), anyhow::Error> {
        let url = format!("{}{}", self.base_url, path);

        let mut req = self.http.get(&url);

        // Forward Authorization header if present
        if let Some(auth) = headers.get("authorization") {
            req = req.header("Authorization", auth);
        }

        let resp = req.send().await?;
        let status = StatusCode::from_u16(resp.status().as_u16())?;
        let body = resp.text().await?;
        Ok((status, body))
    }
}
