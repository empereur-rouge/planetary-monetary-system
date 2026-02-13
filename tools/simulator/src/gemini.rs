use crate::error::{SimError, SimResult};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// Structured directive returned by Gemini
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AgentDirective {
    Send {
        to_agent: String,
        amount: String,
        #[serde(default)]
        reason: String,
    },
    Wait {
        #[serde(default = "default_wait")]
        duration_ms: u64,
        #[serde(default)]
        reason: String,
    },
    Observe {
        #[serde(default = "default_what")]
        what: String,
    },
    Message {
        to_agent: String,
        content: String,
    },
}

fn default_wait() -> u64 {
    2000
}
fn default_what() -> String {
    "balance".to_string()
}

// ════════════════════════════════════════════════════════════════════════════
// OAuth token management
// ════════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OAuthClientCredentials {
    client_id: String,
    client_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedTokens {
    access_token: String,
    refresh_token: String,
    /// Unix timestamp when access token expires
    expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: u64,
    #[allow(dead_code)]
    token_type: String,
}

const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GEMINI_SCOPE: &str = "https://www.googleapis.com/auth/generative-language";
const REDIRECT_PORT: u16 = 18492;

fn token_cache_path() -> PathBuf {
    let dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pms-simulator");
    std::fs::create_dir_all(&dir).ok();
    dir.join("gemini_tokens.json")
}

fn load_cached_tokens() -> Option<CachedTokens> {
    let path = token_cache_path();
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_cached_tokens(tokens: &CachedTokens) {
    let path = token_cache_path();
    if let Ok(json) = serde_json::to_string_pretty(tokens) {
        std::fs::write(&path, json).ok();
    }
}

/// Perform the full OAuth Authorization Code flow with a local redirect server.
/// Opens the user's browser for Google login, catches the redirect on localhost.
async fn oauth_browser_flow(
    http: &Client,
    creds: &OAuthClientCredentials,
) -> SimResult<CachedTokens> {
    let redirect_uri = format!("http://127.0.0.1:{}", REDIRECT_PORT);

    let auth_url = format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent",
        GOOGLE_AUTH_URL,
        urlencoding::encode(&creds.client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(GEMINI_SCOPE),
    );

    // Start a one-shot local server to receive the redirect
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", REDIRECT_PORT))
        .await
        .map_err(|e| SimError::Gemini(format!("cannot bind localhost:{}: {e}", REDIRECT_PORT)))?;

    eprintln!("\n╔══════════════════════════════════════════════════════════╗");
    eprintln!("║  Connexion Google requise pour Gemini API               ║");
    eprintln!("║  Le navigateur va s'ouvrir...                           ║");
    eprintln!("╚══════════════════════════════════════════════════════════╝\n");

    // Open browser
    if let Err(e) = open::that(&auth_url) {
        eprintln!("Impossible d'ouvrir le navigateur: {e}");
        eprintln!("Ouvre manuellement cette URL:\n{}\n", auth_url);
    }

    // Wait for the redirect with the auth code
    let (stream, _addr) = listener
        .accept()
        .await
        .map_err(|e| SimError::Gemini(format!("accept failed: {e}")))?;

    let mut buf = vec![0u8; 4096];
    stream
        .readable()
        .await
        .map_err(|e| SimError::Gemini(format!("read failed: {e}")))?;
    let n = stream
        .try_read(&mut buf)
        .map_err(|e| SimError::Gemini(format!("read failed: {e}")))?;
    let request_str = String::from_utf8_lossy(&buf[..n]);

    // Parse the code from GET /?code=...&scope=...
    let code = request_str
        .lines()
        .next()
        .and_then(|line| {
            // GET /?code=XXX&scope=... HTTP/1.1
            let path = line.split_whitespace().nth(1)?;
            let query = path.split('?').nth(1)?;
            query.split('&').find_map(|param| {
                let (key, val) = param.split_once('=')?;
                if key == "code" {
                    Some(val.to_string())
                } else {
                    None
                }
            })
        })
        .ok_or_else(|| SimError::Gemini("no auth code in redirect".to_string()))?;

    // Send a nice response to the browser
    let response_html = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n\
        <html><body style='background:#111;color:#0f0;font-family:monospace;text-align:center;padding-top:100px'>\
        <h1>Connecte !</h1><p>Tu peux fermer cet onglet et retourner au simulateur.</p>\
        </body></html>";
    stream.try_write(response_html.as_bytes()).ok();

    // Exchange code for tokens
    let token_resp: GoogleTokenResponse = http
        .post(GOOGLE_TOKEN_URL)
        .form(&[
            ("code", code.as_str()),
            ("client_id", &creds.client_id),
            ("client_secret", &creds.client_secret),
            ("redirect_uri", &redirect_uri),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
        .map_err(|e| SimError::Gemini(format!("token exchange failed: {e}")))?
        .json()
        .await
        .map_err(|e| SimError::Gemini(format!("token parse failed: {e}")))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let cached = CachedTokens {
        access_token: token_resp.access_token,
        refresh_token: token_resp.refresh_token.unwrap_or_default(),
        expires_at: now + token_resp.expires_in - 60, // 60s safety margin
    };

    save_cached_tokens(&cached);
    eprintln!("Token obtenu et sauvegarde !\n");

    Ok(cached)
}

/// Refresh an expired access token using the refresh token.
async fn refresh_access_token(
    http: &Client,
    creds: &OAuthClientCredentials,
    refresh_token: &str,
) -> SimResult<CachedTokens> {
    let token_resp: GoogleTokenResponse = http
        .post(GOOGLE_TOKEN_URL)
        .form(&[
            ("client_id", creds.client_id.as_str()),
            ("client_secret", creds.client_secret.as_str()),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await
        .map_err(|e| SimError::Gemini(format!("token refresh failed: {e}")))?
        .json()
        .await
        .map_err(|e| SimError::Gemini(format!("token refresh parse failed: {e}")))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let cached = CachedTokens {
        access_token: token_resp.access_token,
        refresh_token: token_resp
            .refresh_token
            .unwrap_or_else(|| refresh_token.to_string()),
        expires_at: now + token_resp.expires_in - 60,
    };

    save_cached_tokens(&cached);
    Ok(cached)
}

// ════════════════════════════════════════════════════════════════════════════
// Auth mode
// ════════════════════════════════════════════════════════════════════════════

#[derive(Clone)]
enum AuthMode {
    ApiKey(String),
    OAuth {
        creds: OAuthClientCredentials,
        tokens: Arc<RwLock<Option<CachedTokens>>>,
    },
}

// ════════════════════════════════════════════════════════════════════════════
// Gemini Client
// ════════════════════════════════════════════════════════════════════════════

/// Client for the Gemini API — supports API key or OAuth
#[derive(Clone)]
pub struct GeminiClient {
    http: Client,
    model: String,
    auth: AuthMode,
}

#[derive(Serialize)]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
    generation_config: GenerationConfig,
}

#[derive(Serialize)]
struct GeminiContent {
    role: String,
    parts: Vec<GeminiPart>,
}

#[derive(Serialize)]
struct GeminiPart {
    text: String,
}

#[derive(Serialize)]
struct GenerationConfig {
    response_mime_type: String,
    temperature: f32,
    max_output_tokens: u32,
}

#[derive(Deserialize)]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
}

#[derive(Deserialize)]
struct GeminiCandidate {
    content: GeminiCandidateContent,
}

#[derive(Deserialize)]
struct GeminiCandidateContent {
    parts: Vec<GeminiResponsePart>,
}

#[derive(Deserialize)]
struct GeminiResponsePart {
    text: String,
}

impl GeminiClient {
    /// Create a client using an API key
    pub fn with_api_key(api_key: &str, model: &str) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("gemini client build");

        Self {
            http,
            model: model.to_string(),
            auth: AuthMode::ApiKey(api_key.to_string()),
        }
    }

    /// Create a client using OAuth (browser flow).
    /// Loads cached tokens if available, otherwise triggers browser login.
    pub async fn with_oauth(
        client_id: &str,
        client_secret: &str,
        model: &str,
    ) -> SimResult<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("gemini client build");

        let creds = OAuthClientCredentials {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
        };

        // Try to load cached tokens
        let cached = if let Some(tokens) = load_cached_tokens() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            if now < tokens.expires_at {
                tracing::info!("Gemini OAuth: token en cache valide");
                tokens
            } else if !tokens.refresh_token.is_empty() {
                tracing::info!("Gemini OAuth: refresh du token...");
                refresh_access_token(&http, &creds, &tokens.refresh_token).await?
            } else {
                tracing::info!("Gemini OAuth: token expire, connexion navigateur...");
                oauth_browser_flow(&http, &creds).await?
            }
        } else {
            tracing::info!("Gemini OAuth: premiere connexion, ouverture navigateur...");
            oauth_browser_flow(&http, &creds).await?
        };

        Ok(Self {
            http,
            model: model.to_string(),
            auth: AuthMode::OAuth {
                creds,
                tokens: Arc::new(RwLock::new(Some(cached))),
            },
        })
    }

    /// Get a valid access token, refreshing if needed
    async fn get_bearer_token(&self) -> SimResult<String> {
        match &self.auth {
            AuthMode::ApiKey(_) => unreachable!(),
            AuthMode::OAuth { creds, tokens } => {
                let mut guard = tokens.write().await;
                let cached = guard
                    .as_ref()
                    .ok_or_else(|| SimError::Gemini("no tokens available".to_string()))?;

                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();

                if now < cached.expires_at {
                    return Ok(cached.access_token.clone());
                }

                // Refresh
                tracing::info!("Gemini OAuth: refresh du token...");
                let new_tokens =
                    refresh_access_token(&self.http, creds, &cached.refresh_token).await?;
                let token = new_tokens.access_token.clone();
                *guard = Some(new_tokens);
                Ok(token)
            }
        }
    }

    /// Call Gemini to decide the agent's next action.
    pub async fn decide(
        &self,
        system_prompt: &str,
        context: &str,
    ) -> SimResult<AgentDirective> {
        let base_url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
            self.model
        );

        let request = GeminiRequest {
            contents: vec![GeminiContent {
                role: "user".to_string(),
                parts: vec![GeminiPart {
                    text: format!(
                        "{}\n\n--- CONTEXT ---\n{}\n\n--- INSTRUCTION ---\n\
                        Reponds UNIQUEMENT avec un JSON valide. Choisis UNE action parmi:\n\
                        - {{\"action\": \"send\", \"to_agent\": \"<name>\", \"amount\": \"<decimal>\", \"reason\": \"<why>\"}}\n\
                        - {{\"action\": \"wait\", \"duration_ms\": <ms>, \"reason\": \"<why>\"}}\n\
                        - {{\"action\": \"observe\", \"what\": \"balance|tips|supply\"}}\n\
                        - {{\"action\": \"message\", \"to_agent\": \"<name>\", \"content\": \"<msg>\"}}\n\
                        \nJSON:",
                        system_prompt, context
                    ),
                }],
            }],
            generation_config: GenerationConfig {
                response_mime_type: "application/json".to_string(),
                temperature: 0.7,
                max_output_tokens: 256,
            },
        };

        // Retry loop with exponential backoff for rate limits (429)
        let mut attempts = 0u32;
        let resp = loop {
            let r = match &self.auth {
                AuthMode::ApiKey(key) => {
                    let url = format!("{}?key={}", base_url, key);
                    self.http.post(&url).json(&request).send().await?
                }
                AuthMode::OAuth { .. } => {
                    let token = self.get_bearer_token().await?;
                    self.http
                        .post(&base_url)
                        .bearer_auth(&token)
                        .json(&request)
                        .send()
                        .await?
                }
            };

            if r.status().as_u16() == 429 && attempts < 5 {
                attempts += 1;
                let wait = Duration::from_secs(2u64.pow(attempts)); // 2, 4, 8, 16, 32s
                tracing::warn!(
                    "Gemini rate limited (429), retry {}/5 in {}s...",
                    attempts,
                    wait.as_secs()
                );
                tokio::time::sleep(wait).await;
                continue;
            }

            break r;
        };

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(SimError::Gemini(format!("HTTP {}: {}", status, body)));
        }

        let gemini_resp: GeminiResponse = resp.json().await?;

        let text = gemini_resp
            .candidates
            .and_then(|c| c.into_iter().next())
            .and_then(|c| c.content.parts.into_iter().next().map(|p| p.text))
            .ok_or_else(|| SimError::Gemini("empty response".to_string()))?;

        let directive: AgentDirective = serde_json::from_str(&text).map_err(|e| {
            SimError::Gemini(format!("failed to parse directive: {e}\nraw: {text}"))
        })?;

        Ok(directive)
    }
}

// URL encoding helper (avoids adding a full crate)
mod urlencoding {
    pub fn encode(input: &str) -> String {
        let mut output = String::with_capacity(input.len() * 3);
        for byte in input.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    output.push(byte as char);
                }
                _ => {
                    output.push('%');
                    output.push_str(&format!("{:02X}", byte));
                }
            }
        }
        output
    }
}
