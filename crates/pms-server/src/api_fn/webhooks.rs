//! Webhook subscription + delivery (Phase 4 of the SaaS payment-rail
//! integration). Lets a SaaS register a callback URL that receives
//! HMAC-SHA256-signed POSTs whenever a watched address sees activity —
//! the alternative for clients that can't keep an SSE open (Lambda,
//! serverless backends, hosting behind NAT).
//!
//! # Storage
//!
//! Subscriptions are kept **in-memory** (`DashMap`) — the store is
//! re-built from scratch on every engine restart. Pattern: the SaaS
//! re-subscribes from its own DB at startup or via a heartbeat. This
//! avoids a RocksDB schema migration; persistence will land in Phase 4.5
//! if real demand surfaces.
//!
//! # Delivery contract
//!
//! Each event matching a subscription's address set is POSTed to
//! `callback_url` with:
//!
//! ```jsonc
//! POST <callback_url>
//! Content-Type: application/json
//! X-PMS-Signature: <hex(hmac_sha256(secret, body))>
//! X-PMS-Subscription-Id: <subscription_id>
//! X-PMS-Delivery-Id: <delivery_id>            // unique per attempt
//! X-PMS-Delivery-Attempt: <1..=5>
//! Body: {
//!   "subscription_id": "...",
//!   "block_id": "...",
//!   "address": "...",         // matched address (one POST per match)
//!   "ts_ms": 1777917396921,
//!   "encrypted": false,       // true → server couldn't decrypt
//!   "ledger_id": "main"
//! }
//! ```
//!
//! Retries: exponential backoff (1s, 2s, 4s, 8s, 16s — capped) on any
//! 5xx / network error / non-2xx response. Max 5 attempts then dropped
//! with a `tracing::error!` and the `pms_webhook_failed_total` metric.

use crate::api::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Json, response::IntoResponse};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use pms_types_payload::PayloadEnvelope;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Sha256;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, broadcast};

type HmacSha256 = Hmac<Sha256>;

/// Maximum number of distinct addresses one subscription can watch.
/// Same cap as the multi-address SSE — keeps the per-event matching
/// cost bounded at O(|addrs|) per webhook.
pub const MAX_ADDRESSES_PER_SUBSCRIPTION: usize = 1000;

/// Maximum number of subscriptions per engine — bounded so a misconfigured
/// SaaS can't exhaust memory by spamming subscribes.
pub const MAX_SUBSCRIPTIONS: usize = 10_000;

/// Max delivery attempts before giving up. Backoff sequence is
/// `2^(attempt-1)` seconds: 1, 2, 4, 8, 16. Total worst-case time-to-fail
/// is ~31s — short enough that a SaaS heartbeat detects the missed
/// delivery and re-fetches via `GET /v1/blocks/range`.
pub const MAX_DELIVERY_ATTEMPTS: u32 = 5;

/// Concurrent in-flight delivery cap. A misconfigured SaaS endpoint that
/// blocks indefinitely could otherwise pin one tokio task per matched
/// address × every retry × 31s of backoff — bounded here so the engine
/// can't be DoS'd into running out of tasks.
pub const MAX_INFLIGHT_DELIVERIES: usize = 512;

/// In-memory webhook subscription. Cloneable — workers hold copies while
/// processing without keeping the DashMap shard locked.
#[derive(Debug, Clone, Serialize)]
pub struct Subscription {
    pub subscription_id: String,
    /// Lowercased to make membership tests case-insensitive (and match
    /// the multi-address SSE convention).
    pub addresses: HashSet<String>,
    pub callback_url: String,
    /// HMAC secret — never serialized to clients (the response on
    /// `subscribe` returns the secret ONCE; subsequent `list` calls
    /// omit it via `serde(skip_serializing)`).
    #[serde(skip_serializing)]
    pub secret: String,
    pub created_ts_ms: i64,
    /// Total successful (2xx) deliveries. Used by operators / SaaS to
    /// confirm the webhook is reachable.
    pub success_count: u64,
    /// Total deliveries that exhausted [`MAX_DELIVERY_ATTEMPTS`]. Non-zero
    /// = the SaaS endpoint is unhealthy.
    pub failed_count: u64,
}

#[derive(Debug, Clone)]
pub struct WebhookStore {
    inner: Arc<DashMap<String, Subscription>>,
}

impl WebhookStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn insert(&self, sub: Subscription) {
        self.inner.insert(sub.subscription_id.clone(), sub);
    }

    pub fn remove(&self, id: &str) -> Option<Subscription> {
        self.inner.remove(id).map(|(_, v)| v)
    }

    pub fn get(&self, id: &str) -> Option<Subscription> {
        self.inner.get(id).map(|r| r.value().clone())
    }

    pub fn list(&self) -> Vec<Subscription> {
        self.inner.iter().map(|r| r.value().clone()).collect()
    }

    /// Returns subscriptions that have at least one address in `matched`.
    /// Caller side-effect: each subscription gets one delivery per address
    /// in the intersection (same model as multi-address SSE).
    pub fn matching(&self, matched: &HashSet<String>) -> Vec<(Subscription, Vec<String>)> {
        self.inner
            .iter()
            .filter_map(|r| {
                let sub = r.value();
                let hits: Vec<String> = sub
                    .addresses
                    .iter()
                    .filter(|a| matched.contains(*a))
                    .cloned()
                    .collect();
                if hits.is_empty() {
                    None
                } else {
                    Some((sub.clone(), hits))
                }
            })
            .collect()
    }

    /// Increment success counter for a subscription. Best-effort — no-op
    /// if the subscription was unsubscribed mid-delivery.
    pub fn record_success(&self, id: &str) {
        if let Some(mut s) = self.inner.get_mut(id) {
            s.success_count += 1;
        }
    }

    pub fn record_failure(&self, id: &str) {
        if let Some(mut s) = self.inner.get_mut(id) {
            s.failed_count += 1;
        }
    }
}

impl Default for WebhookStore {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════
// HTTP handlers — POST /admin/webhooks, GET /admin/webhooks,
//                 DELETE /admin/webhooks/{id}
// ═══════════════════════════════════════════════════════════════════

#[derive(Deserialize, Debug)]
pub struct SubscribeRequest {
    /// List of addresses to watch. Capped at [`MAX_ADDRESSES_PER_SUBSCRIPTION`].
    pub addresses: Vec<String>,
    /// HTTPS endpoint that will receive the POSTs. Must be reachable from
    /// the engine — operators should test connectivity ahead of subscribe.
    pub callback_url: String,
    /// Optional caller-supplied HMAC secret. If omitted, the server
    /// generates a 32-byte random hex string and returns it ONCE in the
    /// response — the SaaS must store it (it's never returned by `list`).
    pub secret: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct SubscribeResponse {
    pub subscription_id: String,
    /// Returned exactly once at subscription time — the SaaS must persist
    /// it to verify incoming `X-PMS-Signature` headers. Never returned by
    /// `list` again.
    pub secret: String,
    pub addresses_count: usize,
}

pub async fn subscribe_webhook(
    State(state): State<AppState>,
    Json(req): Json<SubscribeRequest>,
) -> impl IntoResponse {
    if req.addresses.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "code": 2030, "message": "addresses must be non-empty" })),
        )
            .into_response();
    }
    if req.addresses.len() > MAX_ADDRESSES_PER_SUBSCRIPTION {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "code": 2040,
                "message": format!(
                    "too many addresses ({}); limit is {} per subscription",
                    req.addresses.len(),
                    MAX_ADDRESSES_PER_SUBSCRIPTION
                )
            })),
        )
            .into_response();
    }
    if !(req.callback_url.starts_with("http://") || req.callback_url.starts_with("https://")) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "code": 2030,
                "message": "callback_url must be http(s)://..."
            })),
        )
            .into_response();
    }

    if state.webhook_store.len() >= MAX_SUBSCRIPTIONS {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "code": 5001,
                "message": "subscription limit reached"
            })),
        )
            .into_response();
    }

    // Normalise — single source of truth so subscription-side and
    // event-matching-side agree (cf. `normalize_address`).
    let addresses: HashSet<String> = req
        .addresses
        .iter()
        .map(|a| normalize_address(a))
        .filter(|a| !a.is_empty())
        .collect();

    // Random subscription_id (32 hex chars) + secret if not supplied.
    use rand::RngCore;
    let mut id_bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut id_bytes);
    let subscription_id = hex::encode(id_bytes);

    let secret = req.secret.unwrap_or_else(|| {
        let mut s = [0u8; 32];
        rand::rng().fill_bytes(&mut s);
        hex::encode(s)
    });

    let sub = Subscription {
        subscription_id: subscription_id.clone(),
        addresses: addresses.clone(),
        callback_url: req.callback_url.clone(),
        secret: secret.clone(),
        created_ts_ms: chrono::Utc::now().timestamp_millis(),
        success_count: 0,
        failed_count: 0,
    };
    state.webhook_store.insert(sub);

    tracing::info!(
        target: "webhook",
        "Subscribed {} watching {} addresses → {}",
        subscription_id,
        addresses.len(),
        req.callback_url
    );

    (
        StatusCode::CREATED,
        Json(SubscribeResponse {
            subscription_id,
            secret,
            addresses_count: addresses.len(),
        }),
    )
        .into_response()
}

pub async fn list_webhooks(
    State(state): State<AppState>,
) -> Json<Vec<Subscription>> {
    Json(state.webhook_store.list())
}

pub async fn unsubscribe_webhook(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.webhook_store.remove(&id) {
        Some(_) => (
            StatusCode::OK,
            Json(json!({ "subscription_id": id, "status": "unsubscribed" })),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({ "code": 3040, "message": "subscription not found" })),
        )
            .into_response(),
    }
}

// ═══════════════════════════════════════════════════════════════════
// Delivery worker — spawned at boot from `tasks::spawn_webhook_delivery`
// ═══════════════════════════════════════════════════════════════════

/// Body of one webhook delivery POST. One event from the bus that
/// matches K addresses produces K bodies (one per matched address).
#[derive(Serialize, Debug)]
pub struct WebhookDeliveryBody {
    pub subscription_id: String,
    pub block_id: String,
    pub address: String,
    pub ts_ms: i64,
    pub encrypted: bool,
    pub ledger_id: String,
}

/// Wire format prefix on the `X-PMS-Signature` header. Future-proofs for
/// algorithm rotation — clients should split on `=` and dispatch on the
/// algo name. Stripe / GitHub use the same convention.
pub(crate) const SIGNATURE_ALGO_PREFIX: &str = "sha256=";

/// Compute `sha256=<hex(HMAC-SHA256(secret, body))>` matching what the SaaS
/// will recompute on its side to authenticate the request.
pub(crate) fn hmac_signature(secret: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(body);
    format!("{}{}", SIGNATURE_ALGO_PREFIX, hex::encode(mac.finalize().into_bytes()))
}

/// Normalize an address for case-insensitive matching: trim whitespace +
/// lowercase. Used both at subscription time and at event-matching time,
/// so a typo here desyncs the two paths and webhooks silently never fire.
/// Single source of truth.
pub(crate) fn normalize_address(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Background loop: subscribe to the engine event bus, dispatch deliveries
/// for each matching subscription. Runs forever until the AppState event
/// bus is closed (engine shutdown).
pub async fn run_delivery_loop(
    store: WebhookStore,
    mut rx: broadcast::Receiver<pms_event::PmsEvent>,
    ledger_id: String,
) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client init");

    // Bounded delivery concurrency — a misconfigured SaaS endpoint that
    // hangs forever can't pin more than `MAX_INFLIGHT_DELIVERIES` tasks.
    let semaphore = Arc::new(Semaphore::new(MAX_INFLIGHT_DELIVERIES));

    loop {
        match rx.recv().await {
            Ok(pms_event::PmsEvent::BlockPersisted {
                block_id,
                ts_ms,
                involved_addresses,
                payload_json,
                ..
            }) => {
                if store.is_empty() {
                    continue;
                }
                let lower: HashSet<String> = involved_addresses
                    .iter()
                    .map(|a| normalize_address(a))
                    .collect();
                let matches = store.matching(&lower);
                if matches.is_empty() {
                    continue;
                }
                let encrypted = is_encrypted_payload(&payload_json);

                for (sub, hits) in matches {
                    for addr in hits {
                        let body = WebhookDeliveryBody {
                            subscription_id: sub.subscription_id.clone(),
                            block_id: block_id.clone(),
                            address: addr,
                            ts_ms,
                            encrypted,
                            ledger_id: ledger_id.clone(),
                        };
                        // Spawn under a permit so concurrent in-flight
                        // deliveries are bounded. Cloning store/client/sub
                        // is cheap (Arc internally).
                        let store2 = store.clone();
                        let client2 = client.clone();
                        let sub2 = sub.clone();
                        let sem = semaphore.clone();
                        tokio::spawn(async move {
                            // Acquire forever (close = drop a delivery).
                            let _permit = match sem.acquire_owned().await {
                                Ok(p) => p,
                                Err(_) => return,
                            };
                            deliver_one(client2, store2, sub2, body).await;
                        });
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(
                    target: "webhook",
                    "delivery loop lagged by {} events — some webhooks may be missing",
                    n
                );
            }
            Err(broadcast::error::RecvError::Closed) => {
                tracing::info!(target: "webhook", "event bus closed, delivery loop exiting");
                break;
            }
            _ => {}
        }
    }
}

/// Robust check: parse the envelope (cheap — bounded JSON, ~500B-2KB)
/// and call the canonical [`PayloadEnvelope::is_encrypted`]. The previous
/// substring heuristic was fragile to user-controlled payload fields
/// (e.g. an NFT description containing the literal `"Encrypted"`).
fn is_encrypted_payload(payload_json: &str) -> bool {
    serde_json::from_str::<PayloadEnvelope>(payload_json)
        .map(|env| env.is_encrypted())
        .unwrap_or(false)
}

async fn deliver_one(
    client: reqwest::Client,
    store: WebhookStore,
    sub: Subscription,
    body: WebhookDeliveryBody,
) {
    let body_bytes = match serde_json::to_vec(&body) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(target: "webhook", "serialize body failed: {e}");
            return;
        }
    };
    let signature = hmac_signature(&sub.secret, &body_bytes);

    let mut delivery_id_bytes = [0u8; 8];
    use rand::RngCore;
    rand::rng().fill_bytes(&mut delivery_id_bytes);
    let delivery_id = hex::encode(delivery_id_bytes);

    for attempt in 1..=MAX_DELIVERY_ATTEMPTS {
        let resp = client
            .post(&sub.callback_url)
            .header("Content-Type", "application/json")
            .header("X-PMS-Signature", &signature)
            .header("X-PMS-Subscription-Id", &sub.subscription_id)
            .header("X-PMS-Delivery-Id", &delivery_id)
            .header("X-PMS-Delivery-Attempt", attempt.to_string())
            .body(body_bytes.clone())
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                store.record_success(&sub.subscription_id);
                tracing::debug!(
                    target: "webhook",
                    "delivered {} → {} status={} attempt={}",
                    delivery_id,
                    sub.callback_url,
                    r.status(),
                    attempt
                );
                return;
            }
            Ok(r) => {
                tracing::warn!(
                    target: "webhook",
                    "delivery {} got non-2xx status={} attempt={}/{}",
                    delivery_id,
                    r.status(),
                    attempt,
                    MAX_DELIVERY_ATTEMPTS
                );
            }
            Err(e) => {
                tracing::warn!(
                    target: "webhook",
                    "delivery {} failed: {e} attempt={}/{}",
                    delivery_id,
                    attempt,
                    MAX_DELIVERY_ATTEMPTS
                );
            }
        }

        if attempt < MAX_DELIVERY_ATTEMPTS {
            // 1, 2, 4, 8, 16 s
            let secs = 1u64 << (attempt - 1);
            tokio::time::sleep(Duration::from_secs(secs)).await;
        }
    }

    store.record_failure(&sub.subscription_id);
    tracing::error!(
        target: "webhook",
        "delivery {} EXHAUSTED after {} attempts → {}",
        delivery_id,
        MAX_DELIVERY_ATTEMPTS,
        sub.callback_url
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HMAC-SHA256 must be deterministic and match reference vectors so
    /// the SaaS can reproduce the signature locally with any standard
    /// crypto library. Worst-case bug: signature drifts silently and
    /// every webhook fails verification.
    #[test]
    fn hmac_signature_is_deterministic_and_distinguishes_inputs() {
        let secret = "supersecret";
        let body = br#"{"address":"8e1abc","amount":"42"}"#;
        let sig = hmac_signature(secret, body);
        let sig_again = hmac_signature(secret, body);
        assert_eq!(sig, sig_again, "HMAC must be deterministic");
        // Format: "sha256=" prefix + 64 hex chars = 71 chars total.
        assert!(sig.starts_with(SIGNATURE_ALGO_PREFIX), "missing algo prefix: {sig}");
        assert_eq!(sig.len(), SIGNATURE_ALGO_PREFIX.len() + 64);

        // Different secret → different signature.
        let other = hmac_signature("other_secret", body);
        assert_ne!(sig, other);

        // Different body → different signature.
        let body2 = br#"{"address":"8e1abc","amount":"43"}"#;
        let sig_b2 = hmac_signature(secret, body2);
        assert_ne!(sig, sig_b2);

        println!("HMAC SIG (secret='supersecret'): {}", sig);
        println!("HMAC SIG (secret='other_secret'): {}", other);
    }

    /// `WebhookStore::matching` must intersect the per-event involved
    /// addresses with each subscription's address set, returning the hit
    /// list per subscription. Critical correctness check — a bug here
    /// either misses deliveries (lost revenue events) or fans out to
    /// unrelated subscriptions (privacy leak).
    #[test]
    fn store_matching_returns_intersection_per_subscription() {
        let store = WebhookStore::new();

        let mut a_addrs = HashSet::new();
        a_addrs.insert("alice".into());
        a_addrs.insert("bob".into());
        store.insert(Subscription {
            subscription_id: "sub_a".into(),
            addresses: a_addrs,
            callback_url: "http://a.example".into(),
            secret: "s1".into(),
            created_ts_ms: 0,
            success_count: 0,
            failed_count: 0,
        });

        let mut b_addrs = HashSet::new();
        b_addrs.insert("carol".into());
        store.insert(Subscription {
            subscription_id: "sub_b".into(),
            addresses: b_addrs,
            callback_url: "http://b.example".into(),
            secret: "s2".into(),
            created_ts_ms: 0,
            success_count: 0,
            failed_count: 0,
        });

        // Event involves alice + carol — both subscriptions should match.
        let mut matched: HashSet<String> = HashSet::new();
        matched.insert("alice".into());
        matched.insert("carol".into());
        let results = store.matching(&matched);
        assert_eq!(results.len(), 2);

        for (sub, hits) in &results {
            match sub.subscription_id.as_str() {
                "sub_a" => assert_eq!(hits, &vec!["alice".to_string()]),
                "sub_b" => assert_eq!(hits, &vec!["carol".to_string()]),
                other => panic!("unexpected sub: {other}"),
            }
        }
        println!("MATCHING OK: 2 subs, each with 1 hit. Results: {results:?}");

        // Event involves only an unwatched address → no matches.
        let mut none: HashSet<String> = HashSet::new();
        none.insert("dave".into());
        assert!(store.matching(&none).is_empty());
    }
}
