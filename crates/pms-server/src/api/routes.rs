// pms-server/src/api/routes — Router construction (ledger-scoped, admin, full API).

use super::ledger_dispatch::dynamic_ledger_handler;
use super::middleware::{require_admin_token, require_api_key, require_local_or_admin, require_writable, track_latency};
use super::state::{sync_all_dag_size_metrics, sync_dag_size_metric, sync_dag_size_metric_for, AppState};
use crate::admin::{
    admin_compact, admin_get_config, admin_ping, admin_purge_activity,
    admin_purge_compliance_log, admin_rebuild_tips, admin_reindex_activity,
    admin_reindex_activity_items, admin_rocksdb_stats, admin_update_config,
};
use crate::api_fn::activity::{
    get_wallet_activity, stream_multi_address_activity, stream_wallet_activity,
};
use crate::api_fn::blocks::{blocks_range, get_block_by_id, submit_block};
use crate::api_fn::bridge::{
    admin_bridge_disable, admin_bridge_enable, admin_bridge_transfer, bridge_status,
    list_bridge_links,
};
use crate::api_fn::compliance::{
    admin_compliance_log, admin_freeze, admin_list_frozen, admin_reverse, admin_seize,
    admin_shadow_balance, admin_unfreeze,
};
use crate::api_fn::contracts::{
    get_contract, list_contracts, register_contract, simulate_contract_handler,
    toggle_contract, update_contract,
};
use crate::api_fn::coordinator::get_coordinator_info;
use crate::api_fn::gas_pool::{admin_gas_pool_deposit, admin_gas_pool_withdraw, get_gas_pool};
use crate::api_fn::version::get_version;
use crate::api_fn::dag::{get_dag_status, get_tips};
use crate::api_fn::estimate_fee::estimate_fee;
use crate::api_fn::transaction_lookup::get_transaction_by_block_id;
use crate::api_fn::history::{get_encrypted_history, get_plain_history, get_wallet_history};
use crate::api_fn::ledger::{
    admin_create_ledger, admin_get_ledger, admin_list_ledgers, list_ledgers,
    transfer_ledger_ownership,
};
use crate::api_fn::milestone::{distribute_fees, get_fee_pool_status};
use crate::api_fn::nft::{
    burn_nft, burn_nft_batch_simple, burn_nft_simple, get_nft, get_nfts_by_owner, mint_nft,
    prepare_nft_transfer,
};
use crate::api_fn::nodes::{connect_peer, list_nodes, list_peers, node_heartbeat, register_node};
use crate::api_fn::stream_blocks::stream_blocks;
use crate::api_fn::supply::get_circulating_supply;
use crate::api_fn::token::{admin_create_token, admin_mint_token, get_token, list_tokens};
use crate::api_fn::transaction::{prepare_tx, wallet_send_tx};
use crate::api_fn::wallet::{balance_by_address, wallet_balance};
use crate::api_fn::wallet_factory::{
    faucet_mint, wallet_create, wallet_restore_mnemonic, wallet_restore_private_key,
    wallet_send_simple,
};
use crate::api_keys::ApiKeyCreateRequest;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{
    Router,
    middleware,
    routing::{any, get, post},
};
use pms_config::Settings;
use serde_json::json;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::time::sleep;
use tower_governor::GovernorLayer;
use tower_governor::errors::GovernorError;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::{KeyExtractor, SmartIpKeyExtractor};
use tower_http::{
    catch_panic::CatchPanicLayer,
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    services::ServeDir,
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

/// Construit les routes ledger-scoped (celles qui dépendent de l'adapter/store d'un ledger).
///
/// Retourne `(public, auth_read, auth_write)` :
///   - **public** — endpoints anonymes (pas d'API key, ex. `/v1/supply`, `/v1/version`).
///   - **auth_read** — endpoints lecture-seule qui exigent l'API key (history, activity, balance, NFT lookups, draft tx).
///     **Continuent de servir** quand l'engine est en read-only mode — c'est exactement le scénario
///     où on veut que les utilisateurs puissent encore voir leurs balances pendant qu'on rétablit
///     les écritures.
///   - **auth_write** — endpoints qui produisent des blocs (submit/block, tx/send, send-simple,
///     nft/mint, nft/burn*). Gated derrière `require_writable` en plus de l'API key, donc ils
///     renvoient 503 quand le resource guard a armé le read-only mode.
pub(super) fn build_ledger_scoped_routes() -> (Router<AppState>, Router<AppState>, Router<AppState>) {
    // Endpoint: /submit/block (Main ingestion) — produces blocks, write-gated
    let submit = Router::new().route("/submit/block", post(submit_block));

    // Read-only wallet endpoints (balance, history, draft-tx prepare, key creation).
    // None of these mutate state on the engine: prepare returns an unsigned tx,
    // create derives a fresh random key.
    //
    // AUDIT H-5 (v0.9.1): the `restore/{mnemonic,private-key}` endpoints — which
    // accept a user's PRE-EXISTING secret in the request body and echo it back —
    // were moved OUT of this API-key-gated read bucket into the admin router
    // (`/admin/wallet/restore/*`, gated by `require_local_or_admin`). They are
    // custodial-by-design key-derivation helpers; restricting them to the
    // operator credential shrinks the surface where a long-term user secret
    // crosses the trust boundary (TLS-terminating gateway, server logs).
    let wallet_read = Router::new()
        .route("/wallet/balance", post(wallet_balance))
        .route("/wallet/history", post(get_wallet_history))
        .route("/v1/balance", post(balance_by_address))
        .route("/v1/tx/prepare", post(prepare_tx))
        // Royalty change — returns the canonical message the CURRENT beneficiary
        // must sign to authorize a change (no block; non-custodial signing path).
        .route("/v1/royalty/prepare", post(crate::api_fn::market::royalty_prepare))
        .route("/v1/wallet/create", post(wallet_create));

    // Write-producing wallet endpoints — gated.
    let wallet_write = Router::new()
        .route("/wallet/tx/send", post(wallet_send_tx))
        .route("/v1/wallet/send-simple", post(wallet_send_simple))
        // Token burn (plan §3.1, voie B) — destroys tokens, reduces supply,
        // fires OnTokenBurn contracts.
        .route(
            "/v1/wallet/token/burn",
            post(crate::api_fn::token_burn::wallet_burn_token),
        )
        // Marketplace settlement (protocole 2.7) — atomic sale/resale with
        // consensus-enforced resale royalty (any token, any ledger, royalty in
        // the payment asset). Produces one Coordinator-signed MarketSettle block.
        .route(
            "/v1/market/settle",
            post(crate::api_fn::market::market_settle),
        )
        // Royalty change (protocole 2.7) — authorized by the CURRENT beneficiary's
        // co-signature (no admin token). Produces a RoyaltyUpdate block.
        .route(
            "/v1/royalty/update",
            post(crate::api_fn::market::royalty_update),
        );

    let blocks = Router::new().route("/blocks/stream", get(stream_blocks));

    let supply = Router::new()
        .route("/v1/supply", get(get_circulating_supply))
        // Preuve de réserves ancrée (protocole 2.6) — dernier snapshot
        .route(
            "/v1/reserves/latest",
            get(crate::api_fn::reserves::get_latest_reserves),
        )
        .route("/v1/fee_pool", get(get_fee_pool_status))
        // Gouvernance (plan §4) — PUBLIC : l'annonce des changements timelockés
        // (droit de sortie informée) + l'historique enacted/cancelled.
        .route(
            "/v1/governance/pending",
            get(crate::api_fn::governance::list_pending),
        )
        .route(
            "/v1/governance/history",
            get(crate::api_fn::governance::list_history),
        )
        // Journal d'audit : TOUS les blocs DAG de gouvernance (proposal/enact/cancel).
        .route(
            "/v1/governance/blocks",
            get(crate::api_fn::governance::list_blocks),
        )
        // Semi-fongibles (catalogue public) — classes + détail + par collection.
        .route("/v1/sft/classes", get(crate::api_fn::sft::list_sft_classes))
        .route("/v1/sft/classes/{asset_id}", get(crate::api_fn::sft::get_sft_class))
        .route(
            "/v1/sft/collections/{collection}",
            get(crate::api_fn::sft::list_sft_collection),
        );

    let token_routes = Router::new()
        .route("/v1/tokens", get(list_tokens))
        .route("/v1/tokens/{asset_id}", get(get_token));

    let history = Router::new()
        .route("/v1/history/encrypted", get(get_encrypted_history))
        .route("/v1/history/plain", get(get_plain_history));

    let dag_routes = Router::new()
        .route("/v1/dag/tips", post(get_tips))
        .route("/v1/dag/status", get(get_dag_status))
        .route("/v1/config", get(crate::api_fn::config::get_config))
        .route("/v1/blocks/{id}", get(get_block_by_id))
        .route("/v1/blocks/range", get(blocks_range))
        .route("/v1/transaction/{block_id}", get(get_transaction_by_block_id))
        .route("/v1/estimate-fee", post(estimate_fee));

    // NFT read endpoints (lookup, owner list, transfer prepare, utxos)
    let nft_read = Router::new()
        .route("/v1/nft/{token_id}", get(get_nft))
        .route("/v1/wallet/{address}/nfts", get(get_nfts_by_owner))
        .route("/v1/nft/transfer/prepare", post(prepare_nft_transfer))
        .route(
            "/v1/wallet/{address}/utxos",
            get(crate::api_fn::wallet::get_utxos_by_address),
        );

    // NFT write endpoints — produce blocks, gated.
    let nft_write = Router::new()
        .route("/v1/nft/mint", post(mint_nft))
        .route("/v1/nft/burn", post(burn_nft))
        .route("/v1/nft/burn-simple", post(burn_nft_simple))
        .route("/v1/nft/burn-batch-simple", post(burn_nft_batch_simple));

    let coordinator_routes = Router::new().route("/v1/coordinator/info", get(get_coordinator_info));

    let version_routes = Router::new().route("/v1/version", get(get_version));

    let activity_routes = Router::new()
        .route("/v1/wallet/{address}/activity", get(get_wallet_activity))
        .route(
            "/v1/wallet/{address}/activity/stream",
            get(stream_wallet_activity),
        )
        .route(
            "/v1/activity/stream",
            get(stream_multi_address_activity),
        );

    (
        // Public read-only routes (no API key required)
        Router::new()
            .merge(supply)
            .merge(coordinator_routes)
            .merge(version_routes)
            .merge(dag_routes)
            .merge(token_routes),
        // Authenticated read-only routes (require API key, NOT write-gated)
        Router::new()
            .merge(wallet_read)
            .merge(blocks)
            .merge(history)
            .merge(nft_read)
            .merge(activity_routes),
        // Authenticated write routes (require API key AND writable engine)
        Router::new()
            .merge(submit)
            .merge(wallet_write)
            .merge(nft_write),
    )
}

// ═══════════════════════════════════════════════════════════════════════
// Admin API Key CRUD Endpoints
// ═══════════════════════════════════════════════════════════════════════

/// POST /admin/api-keys — Crée une nouvelle clé API.
/// Retourne la clé en clair UNE SEULE FOIS.
async fn admin_create_api_key(
    State(state): State<AppState>,
    Json(req): Json<ApiKeyCreateRequest>,
) -> impl IntoResponse {
    let mut store = state.api_key_store.write().await;
    match store.create_key(req.label, req.scopes) {
        Ok(resp) => (StatusCode::CREATED, Json(json!(resp))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))).into_response(),
    }
}

/// GET /admin/api-keys — Liste toutes les clés (sans les hashes).
async fn admin_list_api_keys(State(state): State<AppState>) -> impl IntoResponse {
    let store = state.api_key_store.read().await;
    Json(json!({ "keys": store.list_keys() }))
}

/// DELETE /admin/api-keys/{id} — Révoque (soft-delete) une clé.
async fn admin_revoke_api_key(
    State(state): State<AppState>,
    axum::extract::Path(key_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    let mut store = state.api_key_store.write().await;
    match store.revoke_key(&key_id) {
        Ok(()) => Json(json!({ "status": "revoked", "id": key_id })).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({"error": e}))).into_response(),
    }
}

/// Builds per-ledger admin routes (token management, faucet, NFT mint).
/// Uses admin-token-only middleware (no ConnectInfo needed inside oneshot).
///
/// **Security**: NFT minting on custom ledgers requires admin auth to prevent
/// unauthorized NFT creation that could exploit smart contracts (e.g. spoofing
/// nft_type to trigger contract refunds).
///
/// All four routes here produce blocks (token create/mint, faucet, NFT mint),
/// so the whole router is wrapped with `require_writable` in addition to the
/// admin-token gate — operators get a clean 503 instead of fighting the
/// resource guard during memory pressure.
pub(super) fn build_ledger_admin_routes(state: AppState) -> Router {
    Router::new()
        .route("/admin/tokens/create", post(admin_create_token))
        .route("/admin/tokens/mint", post(admin_mint_token))
        .route("/admin/faucet", post(faucet_mint))
        .route("/admin/nft/mint", post(mint_nft)) // NFT mint admin-only on custom ledgers
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_writable,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_admin_token,
        ))
        .with_state(state)
}

async fn debug_slow(State(_state): State<AppState>) -> impl IntoResponse {
    sleep(Duration::from_secs(5)).await;
    "slow-ok"
}

/// Clé de rate limit dérivée du header `X-API-Key` (quota par identité, v0.30.3).
///
/// Le rate limit per-IP (`SmartIpKeyExtractor`) ne stoppe pas une clé valide
/// abusée depuis plusieurs IPs (botnet). Ce quota per-key complète : il borne le
/// débit d'écriture d'UNE clé, quelle que soit l'IP. Appliqué UNIQUEMENT aux
/// routes write API-key-gated (cf. [`per_api_key_governor_layer`]).
///
/// Requêtes sans header → une clé sentinelle partagée : elles seront de toute
/// façon rejetées 401 par `require_api_key` (hors Dev), et le bucket sentinelle
/// borne un flood keyless résiduel. La clé est le HASH du header, pas sa valeur
/// en clair, pour ne pas retenir de secret en RAM du rate limiter.
#[derive(Clone)]
pub(super) struct ApiKeyKeyExtractor;

impl KeyExtractor for ApiKeyKeyExtractor {
    type Key = u64;

    fn extract<T>(&self, req: &http::Request<T>) -> Result<Self::Key, GovernorError> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        match req.headers().get("x-api-key").and_then(|v| v.to_str().ok()) {
            Some(k) => k.hash(&mut h),
            None => "\0__no_api_key__".hash(&mut h),
        }
        Ok(h.finish())
    }
}

/// Construit la config du quota par API-key. Réutilisée par `build_api_router`
/// (prod) ET les tests — le wiring `.route_layer(GovernorLayer::new(cfg))` reste
/// au call-site (le type `RespBody` du layer y est inféré). Retourne la config
/// portant l'`ApiKeyKeyExtractor` + la conversion rps→période (même footgun
/// `per_nanosecond` que le governor per-IP).
pub(super) fn per_api_key_governor_config(
    rps: u32,
    burst: u32,
) -> Box<
    tower_governor::governor::GovernorConfig<ApiKeyKeyExtractor, governor::middleware::NoOpMiddleware>,
> {
    let period_ns = 1_000_000_000u64 / (rps as u64).max(1);
    Box::new(
        GovernorConfigBuilder::default()
            .per_nanosecond(period_ns)
            .burst_size(burst)
            .key_extractor(ApiKeyKeyExtractor)
            .finish()
            .expect("per-API-key GovernorConfig: invalid api_key_rate_rps or api_key_burst"),
    )
}

/// Construit le Router HTTP complet (public + admin + debug) avec les layers de sécurité.
/// Utilisable depuis le serveur **et** depuis les tests.
pub fn build_api_router(state: AppState, settings: &Settings) -> Router {
    let (per_key_rps, per_key_burst) = settings.limits.effective_api_key_limits();
    tracing::info!(
        "🔒 Engine Rate Limit: {} rps / burst {} (per-IP), {} rps / burst {} (per-API-key, routes write)",
        settings.limits.rate_limit_rps,
        settings.limits.burst,
        per_key_rps,
        per_key_burst
    );

    // NOTE: per_second(N) in tower-governor 0.8 means "period of N seconds"
    // (NOT "N requests per second"). Use per_nanosecond for correct rps conversion.
    // ⚠️ DUAL-LAYER : conversion + key_extractor identiques au gateway
    // (`pms-gateway/src/main.rs::build_app`). Garder les deux synchronisés.
    let period_ns = 1_000_000_000u64 / (settings.limits.rate_limit_rps as u64).max(1);
    let governor_conf = Box::new(
        GovernorConfigBuilder::default()
            .per_nanosecond(period_ns)
            .burst_size(settings.limits.burst as u32)
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .expect("GovernorConfig: invalid rate_limit_rps or burst"),
    );

    // Endpoint: /livez (Check process UP)
    let livez = Router::new().route("/livez", get(|| async { "ok" }));
    // Alias legacy
    let live = Router::new().route("/live", get(|| async { "ok" }));

    // Endpoint: /healthz (enriched checks — see api_fn::healthz, v0.7.4).
    // `/livez` stays trivial (just `200 ok` if the process is alive) so
    // a Kubernetes liveness probe doesn't restart the pod when the
    // persist queue spikes. `/healthz` is the smart one and may return
    // 503 with a JSON breakdown when one of the checks trips.
    let healthz = Router::new().route(
        "/healthz",
        get(crate::api_fn::healthz::enriched_healthz),
    );
    // Alias legacy
    let ready = {
        let r = state._ready.clone();
        Router::new().route(
            "/ready",
            get(move || async move {
                if r.load(Ordering::Relaxed) {
                    "ready"
                } else {
                    "starting"
                }
            }),
        )
    };

    // Endpoint: /metrics (Protected, per-ledger for dashboard compatibility)
    // /metrics        → default ledger metrics (label-free, dashboard-compatible)
    // /metrics/all    → full Prometheus format with labels (ops/Grafana)
    // /l/{id}/metrics → specific ledger metrics (label-free, dashboard-compatible)
    let metrics = Router::new()
        .route(
            "/metrics",
            get(|State(st): State<AppState>| async move {
                // Sync DAG size gauge with actual in-memory count (reflects pruning)
                sync_dag_size_metric(&st);
                crate::metrics::render_for_ledger(&st.ledger_id)
            }),
        )
        .route(
            "/metrics/all",
            get(|State(st): State<AppState>| async move {
                sync_all_dag_size_metrics(&st);
                crate::metrics::render()
            }),
        )
        .route(
            "/l/{ledger_id}/metrics",
            get(
                |State(st): State<AppState>,
                 axum::extract::Path(ledger_id): axum::extract::Path<String>| async move {
                    sync_dag_size_metric_for(&st, &ledger_id);
                    crate::metrics::render_for_ledger(&ledger_id)
                },
            ),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_local_or_admin,
        ));

    // Endpoint: /admin/* (Protected)
    //
    // The admin sub-router intentionally DOES NOT apply a CorsLayer. A browser
    // refusing to send cross-origin requests without CORS headers is itself a
    // defense-in-depth barrier against CSRF attacks targeting an operator who
    // happens to have an admin session cookie / localStorage token (audit
    // finding H-auth-E). Operator tooling (curl, CLI scripts, Postman) is not
    // a browser and therefore not affected.
    //
    // Split into two sub-routers for the read-only mode gate (v0.7.23):
    //
    //   - `admin_writable` — endpoints that produce blocks (distribute_fees,
    //     tokens create/mint, ledgers create / transfer-ownership, bridge
    //     transfer, faucet, compliance freeze/seize/reverse/unfreeze).
    //     Wrapped with `require_writable` so they 503 when the resource
    //     guard has armed read-only mode.
    //
    //   - `admin_recovery` — read endpoints, config updates, and operator
    //     recovery operations (compact, reindex, rebuild-tips, purge,
    //     consolidate-utxos, api-keys CRUD, contracts CRUD/toggle, gas-pool
    //     deposit/withdraw, read-only arm/disarm/status). NOT write-gated:
    //     these are how the operator gets out of read-only in the first
    //     place, so blocking them defeats the purpose.
    //
    // Both share `require_local_or_admin` (auth) applied to the merged
    // router below.
    let admin_writable = Router::new()
        .route("/admin/distribute_fees", post(distribute_fees))
        // Admin Token API - Create and Mint custom tokens
        .route("/admin/tokens/create", post(admin_create_token))
        .route("/admin/tokens/mint", post(admin_mint_token))
        // Admin SFT API - semi-fongibles (créer une classe + mint) — produit des blocs
        .route("/admin/sft/classes", post(crate::api_fn::sft::admin_create_sft_class))
        .route("/admin/sft/mint", post(crate::api_fn::sft::admin_mint_sft))
        // Admin Ledger API - block-producing operations only
        .route("/admin/ledgers/create", post(admin_create_ledger))
        .route("/admin/ledgers/{ledger_id}/transfer-ownership", post(transfer_ledger_ownership))
        // Admin Bridge API - cross-ledger transfer (produces lock + release blocks)
        .route("/admin/bridge/transfer", post(admin_bridge_transfer))
        // Admin Faucet - Mint native PMS (dev/testnet)
        .route("/admin/faucet", post(faucet_mint))
        // On-ramp fiat→PMS (voie A, plan §3.1) — mint natif sous budget partagé
        .route("/admin/onramp", post(crate::api_fn::onramp::admin_onramp))
        // Gouvernance timelock (plan §4) — propose/enact/cancel produisent des blocs
        .route("/admin/governance/propose", post(crate::api_fn::governance::admin_propose))
        .route("/admin/governance/enact/{proposal_id}", post(crate::api_fn::governance::admin_enact))
        .route("/admin/governance/cancel/{proposal_id}", post(crate::api_fn::governance::admin_cancel))
        // Hot-swap config — DÉSORMAIS via gouvernance (forge un GovernanceProposal,
        // + enact instantané si resserrage). Produit un bloc → admin_writable
        // (gated read-only). GET reste en admin_recovery (lecture). Le timelock
        // rend de toute façon les desserrages non-instantanés, donc ce n'est pas
        // un outil de recovery read-only (≠ règle d'avant le rewire P2c).
        .route("/admin/config", post(admin_update_config))
        // Admin Compliance API — write-producing operations
        .route("/admin/compliance/freeze", post(admin_freeze))
        .route("/admin/compliance/unfreeze", post(admin_unfreeze))
        .route("/admin/compliance/seize", post(admin_seize))
        .route("/admin/compliance/reverse", post(admin_reverse))
        // Preuve de réserves (protocole 2.6) — produit un bloc ReserveSnapshot
        .route(
            "/admin/reserves/snapshot",
            post(crate::api_fn::reserves::admin_snapshot_reserves),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_writable,
        ));

    let admin_recovery = Router::new()
        .route("/admin/ping", get(admin_ping))
        .route("/admin/compact", post(admin_compact))
        // Custodial key-derivation helpers (audit H-5, v0.9.1). No DAG block
        // produced → admin_recovery category. They take a user's existing
        // secret (mnemonic / private key) in the body and echo back the
        // derived address + keys; gating them behind the operator credential
        // keeps long-term user secrets off the API-key surface.
        .route("/admin/wallet/restore/mnemonic", post(wallet_restore_mnemonic))
        .route(
            "/admin/wallet/restore/private-key",
            post(wallet_restore_private_key),
        )
        // Admin Config API - lecture seule (GET). Le POST est passé en
        // admin_writable (rewire gouvernance P2c : il forge un bloc).
        .route("/admin/config", get(admin_get_config))
        // Admin Ledger API - read endpoints
        .route("/admin/ledgers", get(admin_list_ledgers))
        .route("/admin/ledgers/{ledger_id}", get(admin_get_ledger))
        // Admin Bridge API - control plane (CF writes only, no blocks)
        .route("/admin/bridge/enable", post(admin_bridge_enable))
        .route("/admin/bridge/disable", post(admin_bridge_disable))
        // Admin Compliance API - read endpoints
        .route("/admin/compliance/frozen", get(admin_list_frozen))
        .route("/admin/compliance/log", get(admin_compliance_log))
        .route(
            "/admin/compliance/shadow_balance",
            get(admin_shadow_balance),
        )
        // Admin Maintenance - Reindex activity, UTXO consolidation
        .route("/admin/reindex-activity", post(admin_reindex_activity))
        .route("/admin/reindex-activity-items", post(admin_reindex_activity_items))
        .route("/admin/consolidate-utxos", post(crate::api_fn::consolidation::admin_consolidate_utxos))
        // H3 curatif (item 6, v0.7.4): re-derive missing tips from
        // children_count when the `tips` CF has drifted (e.g. after a
        // crash between append_block_atomic and add_tip).
        .route("/admin/rebuild-tips", post(admin_rebuild_tips))
        // Audit follow-up to v0.7.4: bounded retention for activity
        // CFs (auto via [health].activity_retention_days OR manual via
        // this endpoint) and operator-only manual purge of the
        // regulatory compliance log.
        .route("/admin/purge-activity", post(admin_purge_activity))
        .route("/admin/purge-compliance-log", post(admin_purge_compliance_log))
        // Diagnostic: RocksDB property snapshot for write-stall analysis.
        // Used by the TPS-degradation profile test; safe to poll from
        // an ops dashboard.
        .route("/admin/rocksdb-stats", get(admin_rocksdb_stats))
        // Admin API Key CRUD endpoints
        .route("/admin/api-keys", post(admin_create_api_key))
        .route("/admin/api-keys", get(admin_list_api_keys))
        .route(
            "/admin/api-keys/{key_id}",
            axum::routing::delete(admin_revoke_api_key),
        )
        // Admin Contract API - Declarative smart contracts (CF writes, no blocks)
        .route("/admin/contracts", post(register_contract))
        .route("/admin/contracts", get(list_contracts))
        .route("/admin/contracts/simulate", post(simulate_contract_handler))
        .route("/admin/contracts/{contract_id}", get(get_contract).put(update_contract))
        .route(
            "/admin/contracts/{contract_id}/toggle",
            post(toggle_contract),
        )
        // Admin Gas Pool API - Per-ledger gas pool management (CF writes)
        .route("/admin/gas-pool/deposit", post(admin_gas_pool_deposit))
        .route("/admin/gas-pool/withdraw", post(admin_gas_pool_withdraw))
        // Preuve de réserves — verify recompute l'état disque, ne produit
        // AUCUN bloc → admin_recovery (diagnostiquable même en read-only).
        .route(
            "/admin/reserves/verify",
            post(crate::api_fn::reserves::admin_verify_reserves),
        )
        // Read-only mode operator controls (v0.7.23)
        .route("/admin/read-only/status", get(crate::admin::admin_read_only_status))
        .route("/admin/read-only/arm", post(crate::admin::admin_read_only_arm))
        .route("/admin/read-only/disarm", post(crate::admin::admin_read_only_disarm))
        // Memory profile endpoint (v0.7.29) — read-only cgroup snapshot
        // for forensic analysis of memory growth without docker exec.
        // Sits in admin_recovery (not gated) so it remains queryable
        // when the engine is in read-only mode for memory pressure
        // (the time you most need it).
        .route(
            "/admin/memory-profile",
            get(crate::api_fn::memory_profile::admin_memory_profile),
        )
        // Webhook subscriptions (Phase 4 — payment-rail integration).
        // CF write only (in-memory DashMap, no DAG block) → admin_recovery
        // by the rule in CLAUDE.md (writable iff produces a block).
        .route(
            "/admin/webhooks",
            post(crate::api_fn::webhooks::subscribe_webhook)
                .get(crate::api_fn::webhooks::list_webhooks),
        )
        .route(
            "/admin/webhooks/{id}",
            axum::routing::delete(crate::api_fn::webhooks::unsubscribe_webhook),
        );

    let admin = Router::new()
        .merge(admin_writable)
        .merge(admin_recovery)
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_local_or_admin,
        ));

    // Node registry endpoints for distributed TX processing (global, not per-ledger).
    // READS stay anonymous (SDKs use `/v1/nodes` for client-side node discovery).
    let node_read_routes = Router::new()
        .route("/v1/nodes", get(list_nodes))
        .route("/v1/peers", get(list_peers));
    // SELF-AUTHENTICATING writes: `register`/`heartbeat` authorize INSIDE the
    // handler — admin token OR a proof-of-possession signature of `node_pk`
    // (v0.30.1). No route_layer, so a genuine peer node (non-loopback, no admin
    // token) can self-register by signing, while an anonymous caller with no
    // signature gets 401. The signature binds api_url + wallet_address + network
    // + a fresh, monotonic ts (anti-replay), so an attacker cannot overwrite
    // another node's entry without its key.
    let node_selfauth_routes = Router::new()
        .route("/v1/register", post(register_node))
        .route("/v1/heartbeat", post(node_heartbeat));
    // OPERATOR-ONLY write: `peers/connect` triggers an OUTBOUND connection to a
    // caller-chosen host (SSRF + P2P-poisoning primitive) — there is no peer
    // self-service reason to expose it, so it stays behind `require_local_or_admin`.
    let node_admin_routes = Router::new()
        .route("/v1/peers/connect", post(connect_peer))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_local_or_admin,
        ));

    // Multi-ledger endpoints (global)
    let ledger_routes = Router::new().route("/v1/ledgers", get(list_ledgers));

    // Gas pool endpoints (public, read-only)
    let gas_pool_routes = Router::new()
        .route("/v1/gas-pool/{ledger_id}", get(get_gas_pool));

    // Bridge endpoints (public, read-only)
    let bridge_routes = Router::new()
        .route("/v1/bridge/links", get(list_bridge_links))
        .route("/v1/bridge/status/{lock_block_id}", get(bridge_status));

    let debug = Router::new().route("/debug/slow", get(debug_slow));

    // Endpoint: /dashboard (Static Files)
    let dashboard = Router::new().nest_service("/dashboard", ServeDir::new("pms-dashboard/dist"));

    // Ledger-scoped routes (default ledger). Three buckets:
    //   - public          — anonymous reads (supply, version, dag tips, etc.)
    //   - auth_read       — API-key-gated reads (balance, history, NFT lookup, draft tx)
    //   - auth_write      — API-key-gated AND read-only-gated writes (submit/block,
    //                       wallet send, NFT mint/burn). 503 with `error: read_only`
    //                       when the resource guard has armed read-only mode.
    let (public_ledger_routes, auth_ledger_read_routes, auth_ledger_write_routes) =
        build_ledger_scoped_routes();
    let auth_ledger_read_routes = auth_ledger_read_routes.route_layer(
        middleware::from_fn_with_state(state.clone(), require_api_key),
    );
    let auth_ledger_write_routes = auth_ledger_write_routes
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_writable,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_api_key,
        ))
        // Quota PAR API-KEY (v0.30.3), outermost → rejette un flood d'écriture
        // d'une même clé le plus tôt possible, indépendamment de l'IP. Complète
        // le governor per-IP global (qui ne stoppe pas une clé abusée depuis
        // plusieurs IPs). Défaut = limite per-IP (cf. Limits::effective_api_key_limits).
        .route_layer(GovernorLayer::new({
            let (rps, burst) = settings.limits.effective_api_key_limits();
            per_api_key_governor_config(rps, burst)
        }));

    // Dynamic per-ledger routing: /l/{ledger_id}/{*rest}
    // Resolves the ledger at request time from LedgerManager, so newly created
    // ledgers become available immediately without restart.
    let per_ledger_router: Router<AppState> =
        Router::new().route("/l/{ledger_id}/{*rest}", any(dynamic_ledger_handler));

    // NOTE: `/internal/*` is intentionally NOT merged here. It is served on a
    // dedicated, trusted port via `serve_internal_api` (see `bin/src/main.rs`,
    // `[client].internal_api_addr`) for the gateway only. Merging it onto the
    // PUBLIC router previously exposed unauthenticated UTXO enumeration
    // (`/internal/utxos/{addr}`) and config disclosure (`/internal/config`) —
    // the same "no auth layer on a state/data route" class as the `/v1/register`
    // hijack. Removed from the public surface (security review, v0.30.1).

    // Combine all
    Router::new()
        .merge(livez)
        .merge(live)
        .merge(healthz)
        .merge(ready)
        .merge(metrics)
        .merge(admin)
        .merge(public_ledger_routes)
        .merge(auth_ledger_read_routes)
        .merge(auth_ledger_write_routes)
        .merge(node_read_routes)
        .merge(node_selfauth_routes)
        .merge(node_admin_routes)
        .merge(ledger_routes)
        .merge(bridge_routes)
        .merge(gas_pool_routes)
        .merge(per_ledger_router)
        .merge(debug)
        .merge(dashboard)
        .with_state(state)
        // GLOBAL LAYERS (Reverse Order: Bottom executed first)
        // 5. Rate Limit
        .layer(GovernorLayer::new(governor_conf))
        // 4. Request Timeout (10-15s)
        .layer(TimeoutLayer::new(Duration::from_millis(
            settings.limits.request_timeout_ms as u64,
        )))
        // 3. Body Limit
        .layer(RequestBodyLimitLayer::new(
            settings.limits.max_body_bytes as usize,
        ))
        // 2. Concurrency Limit (256)
        .layer(tower::limit::ConcurrencyLimitLayer::new(256))
        // 1.5 CORS — origin stays open so the game frontend / SDK can talk
        // to the engine from any domain, but methods + headers are pinned to
        // what the API actually uses. The admin sub-router adds no CORS
        // layer of its own, so a browser can't preflight `/admin/*`: any
        // cross-origin admin request is rejected by the browser before it
        // reaches the auth middleware. See audit finding H-auth-E and the
        // `documentation/trust-model.md` section on operator tooling.
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([
                    axum::http::Method::GET,
                    axum::http::Method::POST,
                    axum::http::Method::PUT,
                    axum::http::Method::DELETE,
                    axum::http::Method::OPTIONS,
                ])
                .allow_headers([
                    axum::http::header::AUTHORIZATION,
                    axum::http::header::CONTENT_TYPE,
                    axum::http::header::ACCEPT,
                    axum::http::HeaderName::from_static("x-api-key"),
                    axum::http::HeaderName::from_static("x-admin-token"),
                ]),
        )
        // 1.5 API latency histogram (records after response, before tracing)
        .layer(middleware::from_fn(track_latency))
        // 1. Tracing (Top)
        .layer(TraceLayer::new_for_http())
        // 0. Catch panics in handlers → 500 instead of killing the server
        .layer(CatchPanicLayer::new())
}

#[cfg(test)]
mod api_key_quota_tests {
    use super::{per_api_key_governor_config, ApiKeyKeyExtractor};
    use axum::body::Body;
    use axum::routing::post;
    use axum::Router;
    use http::{Request, StatusCode};
    use tower::ServiceExt; // oneshot
    use tower_governor::key_extractor::KeyExtractor;
    use tower_governor::GovernorLayer;

    fn req_with_key(key: Option<&str>) -> Request<Body> {
        let mut b = Request::builder().method("POST").uri("/w");
        if let Some(k) = key {
            b = b.header("x-api-key", k);
        }
        b.body(Body::empty()).unwrap()
    }

    /// L'extracteur dérive une clé DISTINCTE par valeur de `X-API-Key`, STABLE
    /// pour une même valeur, et une sentinelle partagée quand le header manque.
    #[test]
    fn extractor_keys_per_api_key_value() {
        let ex = ApiKeyKeyExtractor;
        let a1 = ex.extract(&req_with_key(Some("keyA"))).unwrap();
        let a2 = ex.extract(&req_with_key(Some("keyA"))).unwrap();
        let b = ex.extract(&req_with_key(Some("keyB"))).unwrap();
        let none1 = ex.extract(&req_with_key(None)).unwrap();
        let none2 = ex.extract(&req_with_key(None)).unwrap();
        println!("keyA→{a1} (stable {a2}), keyB→{b}, no-key→{none1} (stable {none2})");
        assert_eq!(a1, a2, "même clé → même bucket");
        assert_ne!(a1, b, "clés différentes → buckets différents (isolation)");
        assert_eq!(none1, none2, "no-key → sentinelle stable");
        assert_ne!(a1, none1, "une clé réelle ≠ la sentinelle no-key");
    }

    /// Bout-en-bout via le VRAI wiring (`per_api_key_governor_config` +
    /// `GovernorLayer`, ceux de la prod) : une clé qui dépasse son burst prend
    /// 429, tandis qu'une AUTRE clé garde un bucket frais (isolation per-key).
    #[tokio::test(flavor = "multi_thread")]
    async fn per_key_quota_throttles_one_key_and_isolates_others() {
        let burst = 3u32;
        let cfg = per_api_key_governor_config(2, burst); // 2 rps, burst 3
        let app = Router::new()
            .route("/w", post(|| async { StatusCode::OK }))
            .route_layer(GovernorLayer::new(cfg));

        // Clé A : on dépasse le burst → au moins un 429.
        let mut a_statuses = Vec::new();
        for _ in 0..(burst + 3) {
            let res = app.clone().oneshot(req_with_key(Some("keyA"))).await.unwrap();
            a_statuses.push(res.status());
        }
        let a_429 = a_statuses.iter().filter(|s| **s == StatusCode::TOO_MANY_REQUESTS).count();
        let a_ok = a_statuses.iter().filter(|s| **s == StatusCode::OK).count();
        println!("keyA: {a_ok} OK, {a_429} × 429 sur {} requêtes (burst {burst})", burst + 3);
        assert!(a_429 >= 1, "keyA doit se faire throttler (429) au-delà du burst");
        assert!(a_ok >= 1, "keyA doit passer au moins le burst initial");

        // Clé B : bucket frais (isolation) → la 1ʳᵉ requête n'est PAS 429.
        let res_b = app.clone().oneshot(req_with_key(Some("keyB"))).await.unwrap();
        println!("keyB (bucket frais): {}", res_b.status());
        assert_ne!(
            res_b.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "keyB (clé distincte) ne doit PAS hériter du throttle de keyA"
        );
    }
}
