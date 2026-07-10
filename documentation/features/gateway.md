---
tags: [feature, infrastructure]
created: 2026-03-14
updated: 2026-07-10
version: v0.30.2
---

# Gateway (Proxy Public)

## Resume

Le Gateway est le point d'entree public unique du reseau PMS. C'est un reverse proxy Axum autonome (`pms-gateway`) qui se place entre les clients externes (SDK, dashboard, navigateurs) et le Engine interne (coordinateur). Il assure cinq responsabilites :

1. **Proxy transparent** : toute requete HTTP (GET, POST, PUT, PATCH, DELETE) est relayee au Engine via un fallback catch-all. Les nouveaux endpoints Engine sont automatiquement disponibles sans modification du Gateway.
2. **Rate limiting per-IP + protection DoS au bord** : limitation de debit par adresse IP via `tower-governor` (token bucket RPS + burst), plus (v0.30.2) un `TimeoutLayer` (→ 408 sur requete lente) et un `ConcurrencyLimitLayer` (plafond de requetes in-flight), en miroir de la pile du Engine. La construction du router est extraite dans `build_app()` (testable via `oneshot`).
3. **Streaming SSE** : deux endpoints de streaming temps reel (`/blocks/stream` et `/v1/wallet/{address}/activity/stream`) sont proxifies en mode streaming (non buffered) pour maintenir la connexion SSE ouverte. Sains sous Timeout/Concurrency : le handler retourne des l'arrivee des headers amont, donc ni le timeout ni le plafond ne bornent le flux vivant.
4. **Forward d'authentification + IP client** : les headers `Authorization`, `X-API-Key` **et `X-Forwarded-For` / `X-Real-IP`** (v0.30.2, whitelist stricte `FORWARDED_HEADERS`) sont transmis au Engine. Le forward de l'IP client permet au rate limiter du Engine de key par vrai client (voir prerequis Caddy en section Configuration). Aucun header ambiant (Cookie, etc.) n'est proxifie.
5. **TLS termination** : support natif HTTPS via `axum-server` + `rustls` (certificats PEM configurables). En production, un Caddy en amont gere Let's Encrypt et proxifie vers le Gateway en TLS interne.
6. **Health monitoring** (v0.5.0) : background health checker qui poll tous les services d'infrastructure (Engine, Prometheus, Simulator, Caddy) toutes les 20s et cache le resultat. Endpoint `GET /services/status` pour le dashboard. Voir [[service-monitoring]].

Le Gateway sert egalement le dashboard admin en fichiers statiques (SPA React) quand `DASHBOARD_PATH` est configure.

## Dates

| | Date |
|---|---|
| Creee | 2026-02-13 |
| Derniere mise a jour | 2026-03-14 |
| Version d'introduction | v0.1.0 |

Le refactoring majeur (v0.3.0) a remplace ~100 routes explicites par un fallback catch-all, reduisant le code a 7 routes explicites (5 internes + 2 SSE).

## Architecture

### Flux de requetes en production

```
Internet
  |
  v
Caddy (ports 80/443, Let's Encrypt, HTTPS termination)
  |
  v
Gateway (port 8443, TLS interne auto-signe)
  |  - Rate limiting per-IP (tower-governor)
  |  - CORS (tower-http)
  |  - Body size limit (tower-http)
  |  - Forward headers (Authorization, X-API-Key)
  |
  v
Engine (port 8080, TLS interne, reseau Docker pms-internal)
  |  - Validation API Key (middleware require_api_key)
  |  - Traitement metier (DAG, transactions, NFTs, etc.)
```

### Reseaux Docker

- **`pms-internal`** : reseau Docker bridge interne (non expose). Connecte Engine, Gateway, Prometheus.
- **`pms-public`** : reseau Docker bridge expose. Connecte Gateway et Caddy.

Le Engine n'est **jamais** expose directement a Internet. Seul le Gateway a acces au Engine via le reseau interne.

### Strategie de routage : fallback catch-all

Le Gateway utilise deux categories de routes :

#### 1. Routes explicites (7 routes)

Ces routes necessitent un traitement special car elles utilisent l'API interne du Engine (`/internal/*`) avec des payloads types (serialisation/deserialisation JSON explicite) ou necessitent un proxy streaming :

| Route Gateway | Methode | API Engine | Raison |
|---------------|---------|------------|--------|
| `/healthz` | GET | `/internal/health` | Reponse typee `HealthResp` |
| `/v1/tips` | GET | `/internal/tips` | Reponse typee `TipsResp` |
| `/v1/utxos/{address}` | GET | `/internal/utxos/{address}` | Reponse typee `UtxosResp` |
| `/v1/blocks/{id}` | GET | `/internal/block/{id}` | Reponse typee `BlockResp` |
| `/submit/block` | POST | `/internal/submit_block` | Requete typee `SubmitBlockReq` / reponse `SubmitBlockResp` |
| `/v1/config` | GET | `/internal/config` | Reponse typee `ConfigResp` |
| `/blocks/stream` | GET | `/blocks/stream` | Proxy streaming SSE (non buffered) |
| `/v1/wallet/{address}/activity/stream` | GET | idem | Proxy streaming SSE (non buffered) |

Note : `/livez` est un health check minimaliste (retourne `"ok"` en texte brut) sans rate limiting, utilise par les healthchecks Docker.

#### 2. Fallback catch-all (toutes les autres routes)

Toute requete non matchee par les routes explicites est capturee par `proxy_fallback()` :
- Le path et la query string sont preserves tels quels (`OriginalUri`).
- La methode HTTP (GET, POST, PUT, PATCH, DELETE) est relayee au Engine.
- Les headers `Authorization` et `X-API-Key` sont forward.
- Le body est attache uniquement s'il est non vide (avec `Content-Type: application/json`).
- La reponse du Engine (status code, headers, body) est retransmise au client.
- En cas d'erreur de connexion au Engine, le Gateway retourne `502 Bad Gateway`.

Ce design garantit que chaque nouvel endpoint ajoute au Engine est automatiquement accessible via le Gateway sans aucune modification du code Gateway.

### Proxy streaming (SSE)

Les endpoints SSE necessitent un traitement different du fallback standard :
- Le fallback buffered (`proxy_fallback`) lit la reponse entiere en memoire avant de la retransmettre. Incompatible avec les flux infinis SSE.
- Le proxy stream (`proxy_stream`) convertit le `bytes_stream()` de `reqwest` en un `axum::body::Body::from_stream()`, permettant un relay chunk-par-chunk sans buffering.
- Le `Content-Type` par defaut est `text/event-stream` (SSE standard).

## Configuration

Le Gateway est configure exclusivement par variables d'environnement (pas de fichier TOML). Cela evite de dependre du crate `pms-config` qui reference la section `[rocks]` (RocksDB, inutile pour le Gateway).

### Variables d'environnement

| Variable | Type | Defaut | Description |
|----------|------|--------|-------------|
| `UPSTREAM_URL` | String | `http://127.0.0.1:8080` | URL du Engine interne. Alias : `ENGINE_URL` |
| `LISTEN_ADDR` | String | `0.0.0.0:8443` | Adresse d'ecoute du Gateway |
| `RATE_LIMIT_RPS` | u64 | `1000` | Requetes/seconde par IP (secure-by-default depuis v0.30.2 ; etait 10000) |
| `BURST_SIZE` | u32 | `2000` | Taille du burst (token bucket) par IP |
| `MAX_BODY_BYTES` | usize | `10485760` (10 MB) | Limite de taille du body HTTP |
| `REQUEST_TIMEOUT_MS` | u64 | `30000` | (v0.30.2) Timeout par requete au bord public → 408. Ne coupe pas les flux SSE |
| `MAX_CONCURRENT` | usize | `512` | (v0.30.2) Plafond de requetes concurrentes in-flight (2× le plafond engine) |
| `TLS_CERT` | String | (aucun) | Chemin vers le certificat PEM. Si absent, HTTP plain |
| `TLS_KEY` | String | (aucun) | Chemin vers la cle privee PEM |
| `CORS_ALLOWED_ORIGINS` | String | (vide) | Origines CORS autorisees, separees par virgule. Si vide ou `*`, mode permissif |
| `DASHBOARD_PATH` | String | (aucun) | Chemin vers les fichiers statiques du dashboard (SPA React) |
| `RUST_LOG` | String | `info` | Niveau de log (`tracing-subscriber` avec `EnvFilter`) |

### Valeurs de production typiques

```yaml
# docker-compose.yml (production, Engine avec TLS interne)
UPSTREAM_URL: https://pms-engine:8080
LISTEN_ADDR: 0.0.0.0:8443
TLS_CERT: /app/tls/cert.pem
TLS_KEY: /app/tls/key.pem
DASHBOARD_PATH: /app/dashboard
RATE_LIMIT_RPS: 1000
BURST_SIZE: 2000
```

```yaml
# docker-compose.testnet.yml (testnet, simule config prod avec HTTPS)
UPSTREAM_URL: https://pms-engine:8080   # HTTPS auto-signe (v0.4.3: danger_accept_invalid_certs)
RATE_LIMIT_RPS: 500     # 5 spammers partagent l'IP docker du simulateur
BURST_SIZE: 1000
REQUEST_TIMEOUT_MS: 30000
MAX_CONCURRENT: 512
```

### ⚠️ Prerequis Caddy — X-Forwarded-For fiable (v0.30.2)

Depuis v0.30.2, le Gateway **forwarde `X-Forwarded-For` / `X-Real-IP` au Engine**
pour que le rate limiter per-IP du Engine (`SmartIpKeyExtractor`) key sur le vrai
client et non sur l'IP du Gateway (sinon bucket global, faille DoS). Ce keying
n'est fiable **que si Caddy ecrase** `X-Forwarded-For` avec l'adresse reelle du
peer, sinon un client peut pre-poser un XFF falsifie et faire tourner sa cle de
rate limit :

```caddyfile
testnet.pms-network.com {
    reverse_proxy https://pms-gateway:8443 {
        transport http { tls tls_insecure_skip_verify }
        header_up X-Forwarded-For {remote_host}   # ← ECRASE (pas append)
    }
}
```

Sans cette ligne, Caddy **ajoute** l'IP client a un XFF eventuellement falsifie ;
`SmartIpKeyExtractor` prenant la valeur de gauche, le client controle sa cle.
Le Gateway n'etant joignable que via Caddy (public) et le simulateur (interne,
de confiance), forwarder le XFF est sinon sur.

> **Note (v0.4.3)** : Le Gateway accepte les certificats auto-signes via `danger_accept_invalid_certs(true)` quand `UPSTREAM_URL` commence par `https://`. L'option `api_tls_enabled` dans `[client]` (voir [[config-system]]) permet de desactiver le TLS API independamment du P2P si necessaire (default: `true`).

### Note sur tower-governor

`tower-governor` v0.8 utilise `per_second(N)` avec la semantique "periode de N secondes" (et non "N requetes par seconde"). Le Gateway contourne cette ambiguite en utilisant `per_nanosecond()` avec une conversion explicite :

```rust
let period_ns = 1_000_000_000u64 / settings.rate_limit_rps.max(1);
GovernorConfigBuilder::default()
    .per_nanosecond(period_ns)
    .burst_size(settings.burst_size)
    .key_extractor(PeerIpKeyExtractor)
```

Le `PeerIpKeyExtractor` isole le rate limiting par adresse IP source.

## CORS

Le layer CORS est construit dynamiquement via `build_cors_layer()` :

- **Mode permissif** (`CORS_ALLOWED_ORIGINS` vide ou contient `*`) : `CorsLayer::permissive()`. Un avertissement est logue. Deconseille en production.
- **Mode restreint** : seules les origines listees sont autorisees, avec :
  - Methodes : GET, POST, OPTIONS
  - Headers : `Authorization`, `Content-Type`
  - Credentials : `true`

## TLS

Le Gateway supporte deux modes :

1. **HTTPS** : si `TLS_CERT` et `TLS_KEY` sont definis, le Gateway utilise `axum-server` avec `RustlsConfig` (ring provider). Les certificats auto-signes sont acceptes pour la communication interne (le client `reqwest` est configure avec `danger_accept_invalid_certs(true)`).
2. **HTTP plain** : si les variables TLS sont absentes, le Gateway demarre en HTTP (mode dev/test).

En production, la chaine TLS est :
- Caddy gere les certificats Let's Encrypt (HTTPS public).
- Le Gateway utilise un certificat auto-signe interne.
- Le Engine utilise un certificat auto-signe interne.
- La communication Gateway -> Engine est en HTTPS interne (auto-signe accepte).

## Dashboard statique

Quand `DASHBOARD_PATH` est configure, le Gateway sert les fichiers statiques du dashboard admin sous `/dashboard/` via `tower-http::services::ServeDir`. Un `not_found_service` pointe vers `index.html` pour supporter le routing SPA (React Router).

Le Dockerfile multi-stage (`Dockerfile.gateway`) :
1. Build le frontend React (`pms-dashboard/`) avec Node.js 20.
2. Build le binaire Rust Gateway.
3. Copie les deux artefacts dans l'image runtime (`debian:trixie-slim`).

## Client HTTP interne (`EngineClient`)

Le struct `EngineClient` encapsule un client `reqwest` configure pour la communication avec le Engine :

- **Timeout** : 30 secondes par requete, 5 secondes pour la connexion.
- **Certificats** : `danger_accept_invalid_certs(true)` pour accepter les certificats auto-signes internes.
- **Initialisation stricte (v0.4.3)** : `EngineClient::new()` **panic** si le client HTTP ne peut pas etre cree (au lieu de retourner silencieusement un client par defaut via `unwrap_or_else`). L'URL upstream est loguee au demarrage pour faciliter le diagnostic.
- **Trois modes d'operation** :
  1. `get<T>()` / `post<B, T>()` : requetes typees avec deserialisation JSON automatique.
  2. `proxy_request()` : proxy generique pour le fallback catch-all (forward brut du body et des headers).
  3. `proxy_stream()` : proxy streaming pour les endpoints SSE (conversion `bytes_stream` -> `Body::from_stream`).

### Forward des headers d'authentification

Les deux methodes de proxy (`proxy_request` et `proxy_stream`) forwardent explicitement :
- `Authorization` : pour le bypass admin (Bearer token).
- `X-API-Key` : pour l'authentification SDK.

Le Gateway ne valide aucune cle -- c'est le Engine qui applique le middleware `require_api_key()`.

## Crates et Fichiers

| Crate | Fichier | Role |
|-------|---------|------|
| `pms-gateway` | `crates/pms-gateway/Cargo.toml` | Dependances : `axum`, `axum-server` (TLS), `tower-governor` (rate limit), `tower-http` (CORS, body limit, trace, static files), `reqwest` (client HTTP), `pms-wire` (types `WireBlock`) |
| `pms-gateway` | `crates/pms-gateway/src/main.rs` | Point d'entree : configuration via env vars, construction du router Axum (health + API + dashboard), layers (governor, CORS, body limit, trace), demarrage HTTP ou HTTPS |
| `pms-gateway` | `crates/pms-gateway/src/routes.rs` | Handlers : 5 routes typees (`healthz`, `get_tips`, `get_utxos`, `get_block`, `submit_block`, `get_config`), fallback catch-all (`proxy_fallback`), proxy streaming (`proxy_stream`) |
| `pms-gateway` | `crates/pms-gateway/src/client.rs` | `EngineClient` : client HTTP vers le Engine avec 3 modes (type, proxy, stream) |
| `pms-wire` | `crates/pms-wire/src/types.rs` | `WireBlock` : type partage entre Gateway et Engine pour la soumission de blocs |
| `pms-server` | `crates/pms-server/src/internal_api.rs` | API interne du Engine : 7 endpoints `/internal/*` consommes par le Gateway |
| `pms-server` | `crates/pms-server/src/api.rs` | Middleware `require_api_key()` qui valide les cles API forwardees par le Gateway |
| -- | `Dockerfile.gateway` | Image Docker multi-stage (frontend React + binaire Gateway) |
| -- | `Caddyfile.prod` | Configuration Caddy : reverse proxy vers Gateway avec headers de securite |
| -- | `docker-compose.yml` | Orchestration production : Engine, Gateway, Caddy, Prometheus |
| -- | `docker-compose.vps.yml` | Architecture VPS 4 processus : Engine, Gateway, Prometheus, Caddy |

## Fonctions Cles

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `GatewaySettings::from_env()` | `crates/pms-gateway/src/main.rs` | Charge la configuration depuis les variables d'environnement (pas de fichier TOML) |
| `build_cors_layer()` | `crates/pms-gateway/src/main.rs` | Construit le layer CORS : permissif si aucune origine configuree, restrictif sinon |
| `main()` | `crates/pms-gateway/src/main.rs` | Point d'entree : init rustls, init tracing, construit router Axum, demarre HTTP ou HTTPS |
| `healthz()` | `crates/pms-gateway/src/routes.rs` | Health check : appelle `/internal/health` sur le Engine, retourne `503` si Engine injoignable |
| `get_tips()` | `crates/pms-gateway/src/routes.rs` | Recupere les tips du DAG via `/internal/tips` |
| `get_utxos()` | `crates/pms-gateway/src/routes.rs` | Recupere les UTXOs d'une adresse via `/internal/utxos/{address}` |
| `get_block()` | `crates/pms-gateway/src/routes.rs` | Recupere un bloc par ID via `/internal/block/{id}` — retourne `404` si absent |
| `submit_block()` | `crates/pms-gateway/src/routes.rs` | Soumet un bloc au Engine via `/internal/submit_block` — retourne `201` (inserted), `409` (duplicate), `400` (rejected) |
| `get_config()` | `crates/pms-gateway/src/routes.rs` | Recupere la configuration des fees via `/internal/config` |
| `proxy_fallback()` | `crates/pms-gateway/src/routes.rs` | Catch-all : proxifie toute requete non matchee vers le Engine en preservant methode, path, query, headers et body |
| `proxy_stream()` | `crates/pms-gateway/src/routes.rs` | Proxy streaming : relaye les flux SSE chunk-par-chunk via `Body::from_stream()` |
| `EngineClient::new()` | `crates/pms-gateway/src/client.rs` | Cree le client HTTP avec timeout 30s et acceptation des certificats auto-signes. Panic si la creation echoue (v0.4.3). Logue l'URL upstream au demarrage |
| `EngineClient::get()` | `crates/pms-gateway/src/client.rs` | Requete GET typee avec deserialisation JSON |
| `EngineClient::post()` | `crates/pms-gateway/src/client.rs` | Requete POST typee avec serialisation/deserialisation JSON |
| `EngineClient::proxy_request()` | `crates/pms-gateway/src/client.rs` | Proxy generique : forward methode, path, headers auth, body brut |
| `EngineClient::proxy_stream()` | `crates/pms-gateway/src/client.rs` | Proxy streaming : forward GET avec headers auth, retourne `Body` streame |
| `internal_routes()` | `crates/pms-server/src/internal_api.rs` | Enregistre les 7 routes `/internal/*` consommees par le Gateway |

## Types partages

### Structs de reponse (Gateway-side)

| Struct | Fichier | Champs |
|--------|---------|--------|
| `HealthResp` | `routes.rs` | `status: String`, `block_count: usize` |
| `TipsResp` | `routes.rs` | `tips: Vec<String>` |
| `UtxoItem` | `routes.rs` | `txid: String`, `index: u32`, `amount: String`, `address: String` |
| `UtxosResp` | `routes.rs` | `utxos: Vec<UtxoItem>` |
| `BlockResp` | `routes.rs` | `id: String`, `parents: Vec<String>`, `payload_json: Option<String>`, `nonce: u64` |
| `SubmitBlockReq` | `routes.rs` | `block: WireBlock` |
| `SubmitBlockResp` | `routes.rs` | `status: String`, `id: String`, `reason: Option<String>` |
| `ConfigResp` | `routes.rs` | `fee_rate_bps: u32`, `base_fee: String`, `coordinator_fee_bps: u32`, `treasury_fee_bps: u32` |
| `GatewaySettings` | `main.rs` | Configuration complete (voir section Configuration) |
| `GatewayState` | `main.rs` | `engine_client: Arc<EngineClient>`, `settings: Arc<GatewaySettings>` |

### API interne du Engine (7 routes)

| Route | Handler Engine | Description |
|-------|----------------|-------------|
| `GET /internal/health` | `internal_health()` | Retourne le statut et le nombre de blocs |
| `GET /internal/tips` | `internal_tips()` | Retourne les 64 derniers tips du DAG |
| `GET /internal/utxos/{address}` | `internal_utxos()` | Retourne les UTXOs non depenses d'une adresse |
| `GET /internal/block/{id}` | `internal_block()` | Retourne un bloc par ID |
| `POST /internal/submit_block` | `internal_submit_block()` | Soumet un WireBlock pour validation et persistance |
| `GET /internal/metrics` | `internal_metrics()` | Retourne les metriques Prometheus (texte) |
| `GET /internal/config` | `internal_config()` | Retourne les parametres de fees depuis RuntimeConfig |

## Layers Axum (ordre d'application)

Les layers sont appliques dans l'ordre inverse de declaration (le dernier declare est execute en premier) :

```
Requete entrante
  |
  v
TraceLayer (logging HTTP)
  |
  v
CorsLayer (validation CORS)
  |
  v
RequestBodyLimitLayer (taille max body)
  |
  v
GovernorLayer (rate limiting per-IP)
  |
  v
Router (match route ou fallback)
```

Les routes de health (`/livez`, `/healthz`) sont exclues du rate limiting car elles sont dans un router separe merge avant l'application des layers.

## Gestion des erreurs

**Logging des erreurs reqwest (v0.4.3)** : les erreurs de connexion au Engine sont loguees avec le format `{e:?}` (Debug) au lieu de `{e}` (Display). Cela expose la chaine complete des erreurs (`source()`, cause interne `hyper`/`h2`/`io`) ce qui facilite le diagnostic en production (ex: distinction entre DNS failure, connection refused, TLS handshake error, timeout).

Chaque handler retourne un code HTTP adapte en cas d'erreur de communication avec le Engine :

| Situation | Code HTTP Gateway | Body |
|-----------|-------------------|------|
| Engine injoignable (health) | `503 Service Unavailable` | `{"status": "engine_unreachable", "block_count": 0}` |
| Engine injoignable (tips) | `502 Bad Gateway` | `{"tips": []}` |
| Engine injoignable (utxos) | `502 Bad Gateway` | `{"utxos": []}` |
| Engine injoignable (block) | `502 Bad Gateway` | `null` |
| Engine injoignable (submit) | `502 Bad Gateway` | `{"status": "gateway_error", "id": "", "reason": "Engine unreachable"}` |
| Engine injoignable (config) | `502 Bad Gateway` | `{"fee_rate_bps": 0, "base_fee": "0", ...}` |
| Engine injoignable (fallback) | `502 Bad Gateway` | `"Gateway error: {detail}"` (text/plain) |
| Engine injoignable (stream) | `502 Bad Gateway` | `"Gateway error: {detail}"` (text/plain) |
| Bloc soumis et accepte | `201 Created` | `{"status": "inserted", "id": "...", "reason": null}` |
| Bloc deja existant | `409 Conflict` | `{"status": "duplicate", ...}` |
| Bloc rejete (invalide) | `400 Bad Request` | `{"status": "rejected", "reason": "..."}` |
| Bloc non trouve | `404 Not Found` | `null` |

## Dependances crate

```toml
[dependencies]
axum = "0.8.4"               # Framework HTTP
axum-server = "0.8"          # Serveur HTTPS (rustls)
rustls = "0.23"              # TLS (ring provider)
tokio = "1"                  # Runtime async
tower-http = "0.6.6"         # Layers : CORS, body limit, trace, static files
tower_governor = "0.8.0"     # Rate limiting per-IP (token bucket)
tower = "0.5.2"              # Middleware tower
reqwest = "0.12"             # Client HTTP (rustls-tls, streaming)
serde = "1"                  # Serialisation
serde_json = "1"             # JSON
anyhow = "1"                 # Gestion d'erreurs
tracing = "0.1"              # Logging structure
tracing-subscriber = "0.3"   # Configuration logging (EnvFilter)
dotenvy = "0.15"             # Chargement .env
http = "1"                   # Types HTTP
pms-wire = { path = "..." }  # Types partages (WireBlock)
```

## Deploiement

### Docker (production)

```bash
# Build l'image Gateway (inclut le dashboard)
docker build -f Dockerfile.gateway -t pms-gateway:latest .

# Lancer la stack complete
docker compose up -d
```

L'image Docker `pms-gateway` fait ~100 MB (binaire Rust + fichiers statiques React) sur base `debian:trixie-slim`.

### Architecture VPS (4 processus)

| Processus | Port | Reseau | Role |
|-----------|------|--------|------|
| Engine | 8080 (interne) | `pms-internal` | Coordinateur DAG, API metier |
| Gateway | 8443 (interne) | `pms-internal` + `pms-public` | Reverse proxy, rate limiting, dashboard |
| Caddy | 80/443 (public) | `pms-public` | TLS termination, Let's Encrypt |
| Prometheus | 9090 (localhost) | `pms-internal` | Metriques |

## Securite

- **Isolation reseau** : le Engine n'est jamais expose a Internet. Seul le Gateway y accede via le reseau Docker interne.
- **Rate limiting per-IP** : `tower-governor` avec token bucket. Protege contre le DDoS et l'abus d'API.
- **Body size limit** : `RequestBodyLimitLayer` empeche les requetes avec un body excessif (defaut 10 MB).
- **Forward selectif des headers** : seuls `Authorization` et `X-API-Key` sont forward. Les autres headers sensibles ne sont pas relayed.
- **TLS interne** : communication Gateway -> Engine en HTTPS meme sur le reseau interne Docker.
- **CORS configurable** : mode permissif deconseille en production. Le mode restrictif limite les origines, methodes, et headers.
- **Healthcheck Docker** : `/livez` retourne `"ok"` sans rate limiting pour permettre les healthchecks Docker frequents.
- **Headers de securite (Caddy)** : `Strict-Transport-Security`, `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`.

## Interactions

- [[api-key-authentication]] : le Gateway forward transparentement le header `X-API-Key` vers le Engine. La validation est faite cote Engine par le middleware `require_api_key()`.
- [[multi-ledger]] : les routes multi-ledger (`/l/{ledger_id}/v1/...`) sont proxifiees par le fallback catch-all sans configuration specifique.
- [[activity-system]] : le streaming SSE d'activite wallet est proxifie via `proxy_stream()`.
- [[fee-distribution]] : l'endpoint `/v1/config` expose les parametres de fees via le handler type `get_config()`.
- [[economics]] : les nouveaux endpoints economics (gas pool, supply, etc.) sont automatiquement disponibles via le fallback catch-all.
