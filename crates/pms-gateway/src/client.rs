use http::StatusCode;
use reqwest::Client;
use serde::de::DeserializeOwned;

/// Headers propagés de la requête client entrante vers l'Engine amont.
///
/// - `authorization` / `x-api-key` : identité de l'appelant (auth engine).
/// - `x-forwarded-for` / `x-real-ip` : **IP du client d'origine**, pour que le
///   rate limiter per-client de l'engine (`SmartIpKeyExtractor`) key sur le
///   vrai client et non sur l'adresse TCP du gateway. Sans ça, TOUT le trafic
///   proxifié s'effondre dans un unique token bucket global keyé sur l'IP du
///   gateway → un seul abuseur consomme le quota de tout le monde (faille DoS
///   corrigée 2026-07).
///
/// # Sécurité
/// Le keying per-client n'est fiable que si le bord public (Caddy) **écrase**
/// `X-Forwarded-For` avec l'adresse réelle du peer (`header_up X-Forwarded-For
/// {remote_host}` dans le Caddyfile). Sinon un client peut pré-poser un XFF
/// falsifié et faire tourner sa clé de rate limit pour contourner la limite.
/// Le gateway n'étant joignable QUE via Caddy (public) et le simulateur
/// (interne, de confiance), forwarder le XFF entrant est sûr sous cette
/// condition. Cf. note de déploiement `documentation/features/gateway.md`.
const FORWARDED_HEADERS: [&str; 4] = [
    "authorization",
    "x-api-key",
    "x-forwarded-for",
    "x-real-ip",
];

/// Recopie sur `req` uniquement les headers de la whitelist [`FORWARDED_HEADERS`]
/// présents dans `headers`. Tout autre header entrant (Cookie, Host client,
/// etc.) est volontairement DROPPÉ — le gateway ne proxifie pas d'état
/// ambiant vers l'engine.
fn forward_client_headers(
    mut req: reqwest::RequestBuilder,
    headers: &http::HeaderMap,
) -> reqwest::RequestBuilder {
    for name in FORWARDED_HEADERS {
        if let Some(value) = headers.get(name) {
            req = req.header(name, value);
        }
    }
    req
}

/// HTTP client for proxying requests to the PMS Engine.
///
/// Supports both `http://` and `https://` upstream URLs.
/// When the upstream is HTTPS, self-signed certificates are accepted
/// (internal Docker network communication).
pub struct EngineClient {
    base_url: String,
    http: Client,
}

impl EngineClient {
    pub fn new(base_url: &str) -> Self {
        let is_https = base_url.starts_with("https://");

        let mut builder = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(5));

        // Only enable dangerous cert acceptance for HTTPS upstream
        if is_https {
            builder = builder.danger_accept_invalid_certs(true);
        }

        let http = builder.build().unwrap_or_else(|e| {
            // NEVER silently fall back to Client::new() — a default client
            // would reject self-signed certs, causing all HTTPS requests to fail
            // with opaque "error sending request" messages.
            panic!(
                "FATAL: Failed to build reqwest HTTP client for upstream {}: {}",
                base_url, e
            );
        });

        tracing::info!(
            upstream = %base_url,
            tls = is_https,
            "Engine client initialized"
        );

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

    /// Generic proxy — forwards any HTTP method to Engine.
    /// Body is only attached (with Content-Type: application/json) when non-empty.
    pub async fn proxy_request(
        &self,
        method: http::Method,
        path: &str,
        headers: http::HeaderMap,
        body: axum::body::Bytes,
    ) -> Result<(StatusCode, String, Option<String>), anyhow::Error> {
        let url = format!("{}{}", self.base_url, path);

        let mut req = self.http.request(method, &url);

        // Forward auth + client-IP headers (see FORWARDED_HEADERS).
        req = forward_client_headers(req, &headers);

        if !body.is_empty() {
            req = req
                .header("Content-Type", "application/json")
                .body(body.to_vec());
        }

        let resp = req.send().await?;
        let status = StatusCode::from_u16(resp.status().as_u16())?;
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let body = resp.text().await?;
        Ok((status, body, content_type))
    }

    /// Proxy GET request and stream the response body
    pub async fn proxy_stream(
        &self,
        path: &str,
        headers: http::HeaderMap,
    ) -> Result<(StatusCode, axum::body::Body, Option<String>), anyhow::Error> {
        let url = format!("{}{}", self.base_url, path);

        let mut req = self.http.get(&url);

        // Forward auth + client-IP headers (see FORWARDED_HEADERS).
        req = forward_client_headers(req, &headers);

        let resp = req.send().await?;
        let status = StatusCode::from_u16(resp.status().as_u16())?;
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        // Convert stream to axum::body::Body
        let stream = resp.bytes_stream();
        let body = axum::body::Body::from_stream(stream);

        Ok((status, body, content_type))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use std::sync::{Arc, Mutex};

    /// Engine mock qui enregistre les headers de la dernière requête reçue.
    /// Retourne (base_url, slot d'enregistrement). Sert à prouver, au VRAI
    /// boundary HTTP (reqwest → hyper), ce que le gateway transmet réellement.
    async fn spawn_recording_engine() -> (String, Arc<Mutex<Option<http::HeaderMap>>>) {
        let recorded: Arc<Mutex<Option<http::HeaderMap>>> = Arc::new(Mutex::new(None));
        let rec = recorded.clone();
        let app = Router::new().fallback(move |headers: http::HeaderMap| {
            let rec = rec.clone();
            async move {
                *rec.lock().unwrap() = Some(headers);
                "ok"
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        (format!("http://{addr}"), recorded)
    }

    /// Le gateway DOIT transmettre l'IP client (`X-Forwarded-For`/`X-Real-IP`)
    /// et l'auth à l'engine — sinon le rate limiter engine key sur l'IP du
    /// gateway (bucket global). Et il NE doit PAS transmettre d'headers
    /// ambiants arbitraires (Cookie).
    #[tokio::test]
    async fn proxy_forwards_client_ip_and_auth_but_not_arbitrary_headers() {
        let (base_url, recorded) = spawn_recording_engine().await;
        let client = EngineClient::new(&base_url);

        let mut inbound = http::HeaderMap::new();
        inbound.insert("x-forwarded-for", "203.0.113.7".parse().unwrap());
        inbound.insert("x-real-ip", "203.0.113.7".parse().unwrap());
        inbound.insert("x-api-key", "test-key-abc".parse().unwrap());
        inbound.insert("authorization", "Bearer tok123".parse().unwrap());
        // Header ambiant qui NE doit PAS être proxifié.
        inbound.insert("cookie", "session=secret".parse().unwrap());

        let (status, _body, _ct) = client
            .proxy_request(
                http::Method::POST,
                "/v1/balance",
                inbound,
                axum::body::Bytes::from_static(b"{}"),
            )
            .await
            .expect("proxy_request should reach the mock engine");
        assert_eq!(status, StatusCode::OK);

        let got = recorded
            .lock()
            .unwrap()
            .clone()
            .expect("mock engine recorded a request");

        let xff = got.get("x-forwarded-for").and_then(|v| v.to_str().ok());
        let xri = got.get("x-real-ip").and_then(|v| v.to_str().ok());
        let key = got.get("x-api-key").and_then(|v| v.to_str().ok());
        let auth = got.get("authorization").and_then(|v| v.to_str().ok());
        let cookie = got.get("cookie").and_then(|v| v.to_str().ok());
        println!(
            "Engine reçu → X-Forwarded-For={xff:?} X-Real-IP={xri:?} X-API-Key={key:?} Authorization={auth:?} Cookie={cookie:?}"
        );

        assert_eq!(
            xff,
            Some("203.0.113.7"),
            "le gateway DOIT forwarder X-Forwarded-For (rate limit per-client engine)"
        );
        assert_eq!(xri, Some("203.0.113.7"), "X-Real-IP doit être forwardé aussi");
        assert_eq!(key, Some("test-key-abc"), "X-API-Key doit être forwardé");
        assert_eq!(auth, Some("Bearer tok123"), "Authorization doit être forwardé");
        assert_eq!(
            cookie, None,
            "le gateway ne doit PAS forwarder un header ambiant (Cookie)"
        );
    }
}
