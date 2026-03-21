---
tags: [feature, infrastructure]
created: 2026-03-14
updated: 2026-03-21
version: v0.6.0
---

# Server / Engine (Serveur Axum)

## Resume

Le Server/Engine est le coeur operationnel du noeud PMS. Il coordonne deux sous-systemes :

1. **Serveur HTTP (Axum)** : API REST publique, admin, interne et per-ledger. Gere l'ingestion de blocs, les wallets, les NFTs, le bridge cross-ledger, la compliance, les tokens custom, les contrats, le gas pool, et les metriques Prometheus. Le serveur supporte TLS (rustls), rate limiting (tower-governor), API key authentication (SHA-256 hash + constant-time comparison), et CORS.

2. **Serveur P2P (TCP/TLS)** : Reseau gossip de blocs entre noeuds. Protocole JSONL (JSON Lines) sur TCP ou TLS avec handshake, Inv/GetBlock/Blocks/Tips, token bucket rate limiting par pair, gestion d'orphelins, et broadcast batche (10ms flush interval).

Le `AppState` centralise toutes les ressources partagees (DAG adapter, RocksDB store, fee pool, node registry, API key store, TPS tracker, activity cache, treasury wallets, multi-ledger manager) et est injecte dans chaque handler Axum via `State<AppState>`.

Les taches de fond incluent : distribution periodique des fees, inflation mint programmee, maintenance RocksDB (flush WAL, compaction, stats), synchronisation P2P (orphan retry, GetTips broadcast), et watchdog API.

## Dates

| | Date |
|---|---|
| Creee | 2025-12 (premiere version du serveur) |
| Derniere mise a jour | 2026-03-14 (branche `feature/economics`) |
| Version d'introduction | v0.1.0 |

## Crates et Fichiers

| Crate | Fichier | Role |
|-------|---------|------|
| `bin` | `bin/src/main.rs` | Point d'entree : bootstrap, config, RocksDB init, multi-ledger, P2P server, API launch, peer connection, periodic sync |
| `pms-server` | `crates/pms-server/src/lib.rs` | Re-exports publics (`Server`, `resolve_admin_token`) et declaration des modules |
| `pms-server` | `crates/pms-server/src/server/mod.rs` | Struct `Server` P2P : constructors, shared state |
| `pms-server` | `crates/pms-server/src/server/peer.rs` | Peer handling (handshake, message dispatch) |
| `pms-server` | `crates/pms-server/src/server/broadcast.rs` | Broadcast/unicast, batched Inv worker |
| `pms-server` | `crates/pms-server/src/server/listener.rs` | TCP/TLS listener, `run()` main loop |
| `pms-server` | `crates/pms-server/src/server/sync.rs` | Sync, `connect_to_peer()`, orphan retry |
| `pms-server` | `crates/pms-server/src/server/blocks.rs` | Block processing (`process_incoming_blocks`) |
| `pms-server` | `crates/pms-server/src/api/mod.rs` | Re-exports publics du module API |
| `pms-server` | `crates/pms-server/src/api/state.rs` | `AppState` struct et constructors |
| `pms-server` | `crates/pms-server/src/api/routes.rs` | `build_api_router()`, route assembly |
| `pms-server` | `crates/pms-server/src/api/serve.rs` | `serve_api()`, HTTP/HTTPS bind |
| `pms-server` | `crates/pms-server/src/api/middleware.rs` | Middlewares (admin, API key, rate limit) |
| `pms-server` | `crates/pms-server/src/api/tasks.rs` | Background tasks (fee distributor, inflation mint) |
| `pms-server` | `crates/pms-server/src/api/ledger_dispatch.rs` | `dynamic_ledger_handler()` multi-ledger routing |
| `pms-server` | `crates/pms-server/src/internal_api.rs` | API interne (Gateway) : health, tips, UTXOs, block, submit_block, metrics, config |
| `pms-server` | `crates/pms-server/src/admin.rs` | Handlers admin : ping, compact, reindex-activity, runtime config hot-swap (GET/POST) |
| `pms-server` | `crates/pms-server/src/api_keys.rs` | Gestion des cles API SDK : hash SHA-256, scopes, CRUD, fichier JSON persistant |
| `pms-server` | `crates/pms-server/src/fee_pool.rs` | `FeePool` : accumulation des fees en memoire, calcul des shares proportionnels, burn refunds |
| `pms-server` | `crates/pms-server/src/fee_distribution/mod.rs` | Distribution periodique des fees : Mint blocks, N-way split, fee burn, inflation |
| `pms-server` | `crates/pms-server/src/node_registry.rs` | Registre dynamique des noeuds : TTL 24h, heartbeat, block counts, reward distribution |
| `pms-server` | `crates/pms-server/src/metrics.rs` | Metriques Prometheus : `pms_blocks_total`, `pms_blocks_persisted_total`, `pms_blocks_rejected_total` |
| `pms-server` | `crates/pms-server/src/stats.rs` | Statistiques atomiques P2P : persist ok/dup/err, gossip ok/reject/err |
| `pms-server` | `crates/pms-server/src/limits.rs` | Constantes de limites P2P : rate limiting, orphan bounds, batch sizes, timeouts |
| `pms-server` | `crates/pms-server/src/rate.rs` | Token bucket rate limiter par pair P2P |
| `pms-server` | `crates/pms-server/src/tls.rs` | Chargement TLS (rustls) : server config, client config (mutual TLS), ALPN h2/http1.1 |
| `pms-server` | `crates/pms-server/src/helper.rs` | `resolve_admin_token()` (env:VAR support), `is_admin_authorized()` (constant-time comparison) |
| `pms-server` | `crates/pms-server/src/contract_engine.rs` | Moteur d'evaluation des smart contracts declaratifs |
| `pms-config` | `crates/pms-config/src/config.rs` | Structures de configuration : `ServerConfig`, `Settings`, `Limits`, `Auth`, `FeesSettings`, `LedgerDef`, etc. |

### Sous-modules API (`crates/pms-server/src/api_fn/`)

| Fichier | Role |
|---------|------|
| `mod.rs` | Declaration de tous les sous-modules API |
| `blocks.rs` | `submit_block`, `get_block_by_id` -- ingestion et lecture de blocs |
| `transaction.rs` | `prepare_tx`, `wallet_send_tx` -- preparation et envoi de transactions UTXO |
| `tx_helpers/mod.rs` | `EffectiveFees`, `resolve_effective_fees()`, `forge_and_sign_block()`, `persist_and_broadcast()`, `select_utxos()`, `create_reward_block()`, fee policy loading, dynamic fee multiplier, gas consumption |
| `wallet.rs` | `wallet_balance`, `balance_by_address`, `get_utxos_by_address` |
| `wallet_factory.rs` | `wallet_create`, `wallet_restore_mnemonic`, `wallet_restore_private_key`, `wallet_send_simple`, `faucet_mint` |
| `nft.rs` | `mint_nft`, `burn_nft`, `burn_nft_simple`, `burn_nft_batch_simple`, `get_nft`, `get_nfts_by_owner`, `prepare_nft_transfer` |
| `supply.rs` | `get_circulating_supply` |
| `milestone.rs` | `distribute_fees`, `get_fee_pool_status` |
| `ledger.rs` | `admin_create_ledger`, `admin_get_ledger`, `admin_list_ledgers`, `list_ledgers` |
| `bridge.rs` | `admin_bridge_enable`, `admin_bridge_disable`, `admin_bridge_transfer`, `bridge_status`, `list_bridge_links` |
| `compliance.rs` | `admin_freeze`, `admin_unfreeze`, `admin_seize`, `admin_reverse`, `admin_list_frozen`, `admin_compliance_log`, `admin_shadow_balance` |
| `contracts.rs` | `register_contract`, `list_contracts`, `get_contract`, `toggle_contract` |
| `gas_pool.rs` | `admin_gas_pool_deposit`, `admin_gas_pool_withdraw`, `get_gas_pool` |
| `token.rs` | `admin_create_token`, `admin_mint_token`, `get_token`, `list_tokens` |
| `nodes.rs` | `register_node`, `list_nodes`, `list_peers`, `connect_peer`, `node_heartbeat` |
| `history.rs` | `get_encrypted_history`, `get_plain_history`, `get_wallet_history` |
| `activity/mod.rs` | `get_wallet_activity`, `stream_wallet_activity` (SSE), `ActivityCache` (LRU in-memory) |
| `stream_blocks.rs` | `stream_blocks` (SSE live block stream) |
| `dag.rs` | `get_tips` |
| `config.rs` | `get_config` (public config endpoint) |
| `coordinator.rs` | `get_coordinator_info` |
| `version.rs` | `get_version` -- expose `API_VERSION`, software version, DAG version, schema version, protocol version |

## Fonctions Cles

### Bootstrap (`bin/src/main.rs`)

| Fonction | Description |
|----------|-------------|
| `main()` | Point d'entree Tokio : charge `.env`, init logging (JSON + tracing), parse CLI args, charge `Settings`, init `Wallet` (node identity), TLS check, RocksDB init, `LedgerManager::bootstrap()`, cree `Server::new()`, connecte aux known peers, lance periodic sync, internal API, dashboard auto-launch |
| `init_logging()` | Init `tracing-subscriber` avec `EnvFilter` (RUST_LOG), format JSON, `tracing-log` bridge. Once-guard pour eviter double-init |
| `print_instructions()` | Affiche les endpoints P2P et health dans la console avec `owo-colors` |

### Serveur P2P (`crates/pms-server/src/server/`)

| Fonction | Description |
|----------|-------------|
| `Server::new()` | Construit le serveur P2P avec adapter, network_id, wallet, peers config, ledger manager. Spawn le broadcast worker (10ms batch flush) |
| `Server::api_only()` | Variante sans P2P pour les routes per-ledger. Partage optionnellement le broadcast channel du serveur principal |
| `Server::run()` | Boucle principale : spawn background sync (2s orphan retry, 10s GetTips), lance API HTTP, spawn RocksDB maintenance, bind P2P listener (TCP ou TLS) |
| `Server::listen()` / `Server::listen_tls()` | Accepte les connexions entrantes TCP/TLS et spawn `handle_new_peer` |
| `Server::handle_new_peer_from_io()` | Init d'un pair : channel mpsc, token bucket, envoi Hello, spawn tache lecture (parsing JSONL, handshake, dispatch messages) et tache ecriture (BufWriter 5ms flush) |
| `Server::process_incoming_blocks()` | Traitement d'un lot de blocs : verification parents (multi-ledger aware), orphan queue, persist_block, broadcast Inv, deblocage recursif d'orphelins |
| `Server::broadcast()` / `Server::broadcast_except()` | Diffusion a tous les pairs inbound. Serialise une fois, partage `Arc<str>` |
| `Server::enqueue_broadcast()` | Ajoute un block ID a la file de broadcast (batche par le worker) |
| `Server::trigger_sync()` | Cleanup inflight + broadcast GetTips a tous les pairs |
| `Server::connect_to_peer()` | Connexion sortante : DNS lookup, TCP connect, TLS handshake optionnel (SNI IP ou DNS) |

### API HTTP (`crates/pms-server/src/api/`)

| Fonction | Description |
|----------|-------------|
| `serve_api()` | Initialise l'AppState complet, spawn les taches de fond (fee distributor, inflation mint), construit le router, bind HTTP ou HTTPS (conditionne par `api_tls_enabled`). Fichier : `api/serve.rs` |
| `build_api_router()` | Assemble toutes les routes (public, auth, admin, internal, per-ledger, debug, dashboard) avec les layers globaux. Fichier : `api/routes.rs` |
| `build_ledger_scoped_routes()` | Construit les routes qui dependent du contexte ledger (wallet, blocks, NFT, supply, etc.). Retourne (public, auth). Fichier : `api/routes.rs` |
| `build_ledger_admin_routes()` | Routes admin per-ledger (token create/mint, faucet) avec middleware admin-token-only. Fichier : `api/routes.rs` |
| `dynamic_ledger_handler()` | Handler generique `/l/{ledger_id}/{*rest}` : resout le ledger, construit AppState per-ledger, forward via `oneshot`. Fichier : `api/ledger_dispatch.rs` |
| `spawn_fee_distributor_task()` | Lance la tache periodique de distribution des fees (defaut: 600s). Appelle `perform_fee_distribution()`. Fichier : `api/tasks.rs` |
| `spawn_inflation_mint_task()` | Lance la tache periodique d'inflation mint (defaut: 86400s / 24h). Appelle `perform_daily_inflation_mint()`. Fichier : `api/tasks.rs` |

### Middlewares

| Middleware | Description |
|------------|-------------|
| `require_local_or_admin` | Protege les routes admin : localhost toujours autorise, sinon check IP allowlist + Bearer token |
| `require_admin_token` | Variante token-only pour les routes per-ledger admin (pas de ConnectInfo dans oneshot) |
| `require_api_key` | Protege les routes publiques authentifiees : admin bypass, mode dev (store vide = passe tout), sinon verifie `X-API-Key` header avec hash SHA-256 + scopes |

### API Interne (`crates/pms-server/src/internal_api.rs`)

| Fonction | Description |
|----------|-------------|
| `internal_health` | `GET /internal/health` -- status + block count |
| `internal_tips` | `GET /internal/tips` -- top 64 tips du DAG |
| `internal_utxos` | `GET /internal/utxos/{address}` -- UTXOs d'une adresse |
| `internal_block` | `GET /internal/block/{id}` -- lecture d'un bloc par ID |
| `internal_submit_block` | `POST /internal/submit_block` -- ingestion de bloc (utilise par le Gateway) |
| `internal_config` | `GET /internal/config` -- RuntimeConfig actuelle |
| `internal_metrics` | `GET /internal/metrics` -- Prometheus text format complet |
| `serve_internal_api()` | Bind HTTP simple (pas de TLS) sur l'adresse interne configuree |

## Architecture

### AppState

L'`AppState` est le conteneur central partage entre tous les handlers Axum. Il est `Clone` (tous les champs sont `Arc` ou `Copy`) et injecte via `State<AppState>`.

```
AppState {
    srv: Arc<Server>               // Serveur P2P (adapter, peers, broadcast)
    _cfg: Arc<ServerConfig>         // Config reseau (bind_addr, api_addr, TLS, auth)
    _ready: Arc<AtomicBool>         // Flag readiness pour /healthz
    stats: Arc<Stats>               // Compteurs atomiques P2P
    store: Arc<RocksStore>          // Stockage RocksDB (blocs, UTXOs, NFTs, etc.)
    admin_token: Option<String>     // Token admin resolu (supporte env:VAR)
    node_wallet: Arc<Wallet>        // Wallet d'identite du noeud (secp256k1)
    settings: Arc<Settings>         // Config applicative complete (TOML)
    allowed_networks: Vec<IpNetwork> // Whitelist IP admin (CIDR)
    treasury_wallets: TreasuryWallets // Wallets treasury verifies par signature coordinator
    node_registry: SharedNodeRegistry // Registre dynamique des noeuds (Arc<RwLock>)
    fee_pool: SharedFeePool          // Pool de fees en memoire (Arc<RwLock>)
    ledger_mgr: Option<Arc<LedgerManager>> // Multi-ledger manager
    ledger_id: String                // ID du ledger courant ("main" par defaut)
    effective_fees: Arc<EffectiveFees> // Config de fees resolue pour ce ledger
    api_key_store: SharedApiKeyStore // Store des cles API (Arc<RwLock>)
    activity_cache: Arc<ActivityCache> // Cache LRU pour les reponses activity (10K entries, 30s TTL)
    tps_tracker: Arc<TpsTracker>     // Tracker TPS pour les frais dynamiques (fenetre 60s)
}
```

### Stack de Middleware (Layers Globaux)

Les layers sont appliques en ordre inverse (le dernier `.layer()` est execute en premier) :

| # | Layer | Description |
|---|-------|-------------|
| 0 | `CatchPanicLayer` | Capture les panics dans les handlers, retourne 500 au lieu de tuer le serveur |
| 1 | `TraceLayer` | Logging trace HTTP (requetes/reponses) via tracing |
| 1.5 | `CorsLayer` | CORS permissif : `allow_origin(Any)`, `allow_methods(Any)`, `allow_headers(Any)` |
| 2 | `ConcurrencyLimitLayer(256)` | Limite a 256 requetes concurrentes |
| 3 | `RequestBodyLimitLayer` | Limite la taille du body (configurable, defaut `max_body_bytes`) |
| 4 | `TimeoutLayer` | Timeout par requete (configurable, `request_timeout_ms`) |
| 5 | `GovernorLayer` | Rate limiting par IP (`SmartIpKeyExtractor`) : `rate_limit_rps` / `burst` |

### Organisation des Routes

Le router est divise en groupes logiques :

```
/livez, /live                    -> Health check (processus UP)
/healthz, /ready                 -> Readiness check (DB + ready flag)
/metrics                         -> Metriques Prometheus (admin-protege)
/metrics/all                     -> Toutes les metriques avec labels
/l/{ledger_id}/metrics           -> Metriques per-ledger

/admin/*                         -> Routes admin (require_local_or_admin)
  /admin/ping                    -> Test token admin
  /admin/compact                 -> Flush WAL + compaction RocksDB
  /admin/config                  -> GET/POST RuntimeConfig hot-swap
  /admin/tokens/create           -> Creation de token custom
  /admin/tokens/mint             -> Mint de token custom
  /admin/ledgers                 -> Liste/creation/detail de ledgers
  /admin/bridge/*                -> Bridge cross-ledger
  /admin/faucet                  -> Mint PMS natif (dev/testnet)
  /admin/compliance/*            -> Freeze, seize, reverse, shadow balance
  /admin/reindex-activity        -> Reindexation des indexes d'activite
  /admin/reindex-activity-items  -> Reindexation des items d'activite
  /admin/api-keys                -> CRUD cles API SDK
  /admin/contracts               -> CRUD smart contracts declaratifs
  /admin/gas-pool/*              -> Deposit/withdraw gas pool
  /admin/distribute_fees         -> Distribution manuelle des fees

/internal/*                      -> API interne (pour Gateway, pas de middleware auth)
  /internal/health               -> Status + block count
  /internal/tips                 -> Top 64 tips
  /internal/utxos/{address}      -> UTXOs par adresse
  /internal/block/{id}           -> Bloc par ID
  /internal/submit_block         -> Ingestion de bloc
  /internal/metrics              -> Prometheus full
  /internal/config               -> RuntimeConfig

Routes publiques (pas d'API key) :
  /v1/version                    -> Toutes les versions (software, DAG, schema, P2P, API)
  /v1/supply                     -> Supply circulante
  /v1/fee_pool                   -> Status du pool de fees
  /v1/coordinator/info           -> Infos coordinator
  /v1/dag/tips                   -> Tips du DAG
  /v1/config                     -> Config publique
  /v1/blocks/{id}                -> Lecture d'un bloc
  /v1/tokens                     -> Liste des tokens
  /v1/tokens/{asset_id}          -> Detail d'un token
  /v1/ledgers                    -> Liste des ledgers disponibles
  /v1/gas-pool/{ledger_id}       -> Status gas pool d'un ledger
  /v1/bridge/links               -> Liens bridge actifs
  /v1/bridge/status/{lock_id}    -> Status d'un transfert bridge

Routes authentifiees (require_api_key) :
  /submit/block                  -> Ingestion de bloc
  /wallet/tx/send                -> Envoi de transaction
  /wallet/balance                -> Balance d'un wallet
  /wallet/history                -> Historique d'un wallet
  /v1/balance                    -> Balance par adresse
  /v1/tx/prepare                 -> Preparation de transaction
  /v1/wallet/create              -> Creation de wallet
  /v1/wallet/restore/mnemonic    -> Restauration par mnemonic
  /v1/wallet/restore/private-key -> Restauration par cle privee
  /v1/wallet/send-simple         -> Envoi simplifie
  /v1/nft/*                      -> NFT mint, burn, transfer, query
  /v1/wallet/{addr}/nfts         -> NFTs par proprietaire
  /v1/wallet/{addr}/utxos        -> UTXOs par adresse
  /v1/wallet/{addr}/activity     -> Activite d'un wallet
  /v1/wallet/{addr}/activity/stream -> Stream SSE d'activite
  /v1/history/encrypted          -> Historique chiffre
  /v1/history/plain              -> Historique en clair
  /blocks/stream                 -> Stream SSE de blocs

Routes per-ledger dynamiques :
  /l/{ledger_id}/{*rest}         -> Toutes les routes ci-dessus, scopees au ledger
  /l/{ledger_id}/metrics         -> Metriques du ledger specifique

Routes globales (non scopees au ledger) :
  /v1/register                   -> Enregistrement d'un noeud
  /v1/nodes                      -> Liste des noeuds actifs
  /v1/peers                      -> Liste des pairs P2P connectes
  /v1/peers/connect              -> Connexion a un pair
  /v1/heartbeat                  -> Heartbeat d'un noeud

Autres :
  /dashboard/*                   -> Fichiers statiques (pms-dashboard/dist)
  /debug/slow                    -> Endpoint de debug (sleep 5s)
```

### API Interne vs Externe

Le serveur expose **deux APIs HTTP** distinctes :

1. **API Externe** (`api_addr`, ex: `0.0.0.0:8080`) : API complete avec middlewares (rate limit, API keys, admin auth, CORS, TLS). Utilisee par les clients SDK, le dashboard, et les operateurs.

2. **API Interne** (`internal_api_addr`, ex: `0.0.0.0:3000`) : API simplifiee sans middlewares d'authentification, sans TLS. Utilisee exclusivement par le [[gateway]] pour la communication engine-gateway. Les routes sont prefixees `/internal/`.

L'API interne est lancee uniquement si `internal_api_addr` est configure dans le TOML :
```toml
[client]
internal_api_addr = "0.0.0.0:3000"
```

### Multi-Ledger Routing

Le handler `dynamic_ledger_handler()` permet de router dynamiquement les requetes vers n'importe quel ledger :

1. Extrait `{ledger_id}` et `{rest}` du path `/l/{ledger_id}/{*rest}`
2. Resout l'instance de ledger depuis le `LedgerManager`
3. Construit un `AppState` per-ledger avec le store, l'adapter, et les fees resolues du ledger
4. Cree un `Server::api_only()` pour ce ledger (partage le broadcast channel du serveur principal)
5. Reconstruit le path de la requete sans le prefix `/l/{ledger_id}`
6. Forward la requete via `router.oneshot(request)`

Les ledgers crees dynamiquement (via `POST /admin/ledgers/create`) sont accessibles immediatement sans restart grace a la resolution au moment de la requete.

### Taches de Fond

| Tache | Intervalle | Description |
|-------|-----------|-------------|
| Fee Distribution | `distribution_interval_sec` (defaut: 600s) | Distribue les fees accumulees via bloc Mint |
| Inflation Mint | `daily_inflation_interval_sec` (defaut: 86400s) | Mint quotidien d'inflation programmee |
| RocksDB Compaction | 6h | `compact_all()` rate-limited (200ms entre CFs) |
| RocksDB WAL Flush | 10min | `flush_wal()` pour durabilite |
| RocksDB Stats | 30min | Log des statistiques internes RocksDB |
| P2P Orphan Retry | 2s | Relance les GetBlock pour les parents manquants d'orphelins |
| P2P GetTips Broadcast | 10s | Diffusion GetTips pour synchronisation |
| P2P Stats Log | 10s | Log des compteurs persist/gossip |
| Periodic Sync | 5s | `trigger_sync()` : cleanup inflight + GetTips |
| API Watchdog | continu | Detecte crash/panic de l'API et exit le process |

### Protocole P2P

Le protocole P2P utilise des messages JSON Lines (un JSON par ligne) sur TCP ou TLS :

| Message | Direction | Description |
|---------|-----------|-------------|
| `Hello` | bidirectionnel | Handshake : proto=1, node_id, nonce, ping_ms |
| `HelloAck` | reponse | Confirmation/rejet du handshake |
| `Ping` | sortant | Keepalive (rate-limited par token bucket) |
| `Pong` | reponse | Reponse au Ping |
| `Block` | entrant | Bloc complet recu d'un pair |
| `Blocks` | reponse | Lot de blocs (reponse a GetBlock/GetBlocks) |
| `Inv` | sortant | Annonce d'IDs de blocs disponibles (batche) |
| `GetBlock` | sortant | Demande d'un bloc specifique |
| `GetBlocks` | sortant | Demande de plusieurs blocs |
| `GetTips` | bidirectionnel | Demande des tips du DAG (limit configurable) |
| `Tips` | reponse | Liste des tips du DAG |

Le broadcast est batche par le `spawn_broadcast_worker()` : les IDs sont accumules pendant 10ms puis envoyes en un seul message `Inv`. La serialisation est faite une seule fois et partagee via `Arc<str>` a tous les pairs (O(1) clone par pair).

### Limites P2P

| Constante | Valeur | Description |
|-----------|--------|-------------|
| `MAX_LINE_BYTES` | 10 MiB | Taille max d'un message JSONL |
| `PER_PEER_Q_CAP` | 10 000 | Taille de la file de sortie par pair |
| `RATE_MSGS_PER_SEC` | 10 000 | Rate limit messages/sec par pair |
| `RATE_BURST` | 20 000 | Burst du token bucket |
| `MAX_PARSE_ERRORS` | 8 | Erreurs de parsing avant kick |
| `HANDSHAKE_TIMEOUT_MS` | 1 500 | Timeout handshake |
| `MAX_BLOCKS_BATCH` | 512 | Taille max d'un batch de blocs |
| `MAX_INFLIGHT_GETBLOCK` | 100 000 | Requetes GetBlock en vol max |
| `INFLIGHT_TTL_MS` | 10 000 | TTL des requetes en vol |
| `MAX_ORPHANS` | 10 000 | Orphelins en memoire max |
| `MAX_PARENT_DEPS` | 20 000 | Dependances parent-enfant max |
| `SEEN_CAPACITY` | 10 000 | Capacite du cache LRU Inv |

## Configuration

### Section `[client]`

| Champ | Type | Description |
|-------|------|-------------|
| `bind_addr` | `String` | Adresse du listener P2P (ex: `"0.0.0.0:8050"`) |
| `api_addr` | `String` | Adresse de l'API HTTP (ex: `"0.0.0.0:8080"`) |
| `allow_insecure_tls` | `bool` | Autorise TLS insecure en dev/testnet (defaut: false) |
| `api_tls_enabled` | `bool` | Active/desactive TLS sur l'API HTTP independamment du P2P (defaut: true). Voir [[config-system#Separation TLS API / P2P (v0.4.3)]] |
| `internal_api_addr` | `Option<String>` | Adresse de l'API interne pour Gateway (ex: `"0.0.0.0:3000"`) |

### Section `[auth]`

| Champ | Type | Description |
|-------|------|-------------|
| `require_signed_submit` | `bool` | Exige des blocs signes |
| `admin_api_token` | `Option<String>` | Token admin (supporte `"env:VAR_NAME"`) |
| `allowed_ips` | `Vec<String>` | Whitelist IP/CIDR pour admin. Vide = tout autorise avec token |
| `api_keys_file` | `Option<String>` | Chemin fichier JSON des cles API SDK |

### Section `[limits]`

| Champ | Type | Defaut | Description |
|-------|------|--------|-------------|
| `max_body_bytes` | `usize` | 262144 | Taille max du body HTTP |
| `request_timeout_ms` | `u64` | 4000 | Timeout par requete |
| `rate_limit_rps` | `u32` | 20 | Rate limit HTTP (requetes/sec par IP) |
| `burst` | `u32` | 40 | Burst du rate limiter HTTP |

### Section `[tls]`

| Champ | Type | Description |
|-------|------|-------------|
| `cert_pem` | `String` | Chemin du certificat PEM |
| `key_pem` | `String` | Chemin de la cle privee PEM (PKCS#8 ou EC SEC1) |
| `ca_pem` | `Option<String>` | Chemin du CA root pour mutual TLS (client P2P) |
| `whitelist_fp256` | `Vec<String>` | Empreintes SHA-256 autorisees (optionnel) |

### Section `[network]`

| Champ | Type | Description |
|-------|------|-------------|
| `mode` | `"dev"` / `"testnet"` / `"mainnet"` | Mode reseau. Mainnet = TLS obligatoire, pas de fallback |
| `network_id` | `String` | Identifiant du reseau (ex: `"pms-dev"`) |
| `protocol_version` | `u32` | Version du protocole P2P |
| `symbol` | `Option<String>` | Symbole du token natif (defaut: `"PMS"`) |

### Section `[p2p]`

| Champ | Type | Description |
|-------|------|-------------|
| `known_peers` | `String` | Liste des pairs initiaux (comma-separated) |
| `bind_addr` | `Option<String>` | Adresse P2P override |
| `allowed_peer_ips` | `Vec<String>` | IPs autorisees pour les connexions P2P |
| `strict_whitelist` | `bool` | Si true, rejete les connexions hors whitelist |

## Securite

### Authentification Admin

Trois niveaux de protection pour les routes admin :

1. **Localhost bypass** : Les connexions depuis `127.0.0.1` / `::1` sont toujours autorisees
2. **IP allowlist** : Si `allowed_ips` est configure, seules les IPs/CIDR listees passent
3. **Bearer token** : Verifie via `Authorization: Bearer <token>` ou `X-Admin-Token: <token>`

La comparaison du token utilise une **verification constant-time** (`subtle::ConstantTimeEq`) pour prevenir les timing attacks.

### Authentification API Key

Les routes publiques authentifiees sont protegees par le middleware `require_api_key` :

- **Admin bypass** : Un Bearer admin token valide donne acces a toutes les routes
- **Mode dev** : Si aucune cle n'est configuree (store vide), tout passe (backward-compatible)
- **Verification** : Header `X-API-Key: pk_live_...` verifie par hash SHA-256 (constant-time)
- **Scopes** : Chaque cle a des scopes (`"*"`, `"wallet"`, `"nft"`, `"dag"`, etc.) qui determinent les endpoints accessibles

### TLS

- **Mainnet** : TLS obligatoire pour HTTP et P2P. Crash si les fichiers cert/key manquent
- **Dev/Testnet** : Fallback en HTTP/TCP clair si les fichiers TLS manquent
- **ALPN** : Supporte h2 (HTTP/2) et http/1.1
- **Mutual TLS** : Support pour le P2P client avec CA custom
- **Separation API / P2P (v0.4.3)** : le champ `api_tls_enabled` (defaut: `true`) dans `[client]` permet de desactiver TLS sur l'API HTTP tout en gardant le P2P en TLS. Utile en deploiement Docker ou le Gateway communique avec l'Engine via un reseau interne non expose (`pms-internal`). `serve_api()` dans `api/serve.rs` verifie `api_tls_enabled` avant d'appliquer TLS sur le listener HTTP

## Metriques Prometheus

| Metrique | Type | Labels | Description |
|----------|------|--------|-------------|
| `pms_blocks_total` | Gauge | `ledger_id` | Taille du DAG en memoire (syncee a chaque `/metrics` fetch) |
| `pms_blocks_persisted_total` | Counter | `ledger_id` | Blocs valides et persistes |
| `pms_blocks_rejected_total` | Counter | `ledger_id` | Blocs rejetes lors de la persistance |

Trois endpoints de metriques :
- `GET /metrics` : Metriques du ledger par defaut (format dashboard, sans labels)
- `GET /metrics/all` : Toutes les metriques Prometheus avec labels (ops/Grafana)
- `GET /l/{ledger_id}/metrics` : Metriques d'un ledger specifique

## Statistiques P2P

La struct `Stats` maintient des compteurs atomiques (`AtomicU64`) loggues toutes les 10 secondes :

| Compteur | Description |
|----------|-------------|
| `persisted_ok` | Blocs persistes avec succes |
| `persisted_dup` | Blocs dupliques ignores |
| `persisted_err` | Erreurs de persistance |
| `gossip_out_ok` | Messages gossip envoyes avec succes |
| `gossip_out_rej` | Messages gossip rejetes |
| `gossip_out_err` | Erreurs d'envoi gossip |

## Versionnage

Le endpoint `GET /v1/version` expose toutes les versions du noeud :

| Champ | Source | Description |
|-------|--------|-------------|
| `software_version` | `bin/Cargo.toml` (`CARGO_PKG_VERSION`) | Version SemVer du logiciel |
| `dag_version` | RocksDB (`get_dag_version()`) | Version du protocole DAG |
| `schema_version` | RocksDB (`get_version()`) | Version du schema RocksDB (entier) |
| `protocol_version` | `settings.network.protocol_version` | Version du protocole P2P |
| `api_version` | `API_VERSION` constante (actuellement `2`) | Version de l'API REST |

## Interactions

- **[[gateway]]** : Communique avec l'engine via l'API interne (`/internal/*`). Le gateway sert de proxy HTTP pour les clients externes.
- **[[storage-rocksdb]]** : L'`AppState` contient un `Arc<RocksStore>` pour toutes les operations de persistance (blocs, UTXOs, NFTs, config, contracts, gas pools, subscriptions, activity indexes).
- **[[fee-distribution]]** : Le serveur spawn la tache `spawn_fee_distributor_task()` et expose `POST /admin/distribute_fees` pour la distribution manuelle.
- **[[multi-ledger]]** : Le `LedgerManager` est bootstrap au demarrage et integre dans l'`AppState`. Les routes `/l/{ledger_id}/*` permettent d'acceder a chaque ledger independamment.
- **[[economics]]** : Le `TpsTracker` est utilise pour les frais dynamiques. Les gas pools et subscriptions sont verifies a chaque transaction sur les ledgers custom.
- **[[smart-contracts]]** : Le `contract_engine` est appele depuis les handlers NFT burn pour evaluer les contrats declaratifs.
- **[[bridge]]** : Les routes admin bridge (`/admin/bridge/*`) et les routes publiques (`/v1/bridge/*`) gerent les transferts cross-ledger.
- **[[compliance]]** : Les routes admin compliance (`/admin/compliance/*`) permettent le freeze, seize, reverse et shadow balance des wallets.
- **[[nft-system]]** : Les routes NFT (`/v1/nft/*`) gerent le mint, burn, transfer et query des NFTs avec support de fees et contrats.
- **[[activity-system]]** : L'`ActivityCache` (LRU, 10K entries, 30s TTL) accelere les requetes d'activite. Les streams SSE permettent le suivi en temps reel.
- **[[api-key-authentication]]** : Le middleware `require_api_key` protege les routes authentifiees. Le CRUD admin permet de gerer les cles via `/admin/api-keys`.
- **[[wallet-factory]]** : Les routes wallet factory (`/v1/wallet/*`) permettent la creation, restauration et envoi simplifie depuis le serveur.
