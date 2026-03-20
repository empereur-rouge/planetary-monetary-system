---
tags: [feature]
created: 2026-02-15
updated: 2026-03-20
version: v0.5.18
---

# Simulator / Game Engine

## Résumé

Le simulateur (`pms-simulator`) est un outil standalone de stress-testing et de simulation multi-agents pour le réseau DAG-PMS. Il génère du trafic réaliste en déployant des centaines d'agents autonomes qui exécutent des transactions PMS, brûlent des [[nft-system|NFTs]] (cubes), échangent un token secondaire (Edenite / EDN), et communiquent entre eux via un système de messagerie P2P interne.

Le simulateur existe pour :
- **Stress-tester** le moteur PMS en conditions proches de la production (250+ agents, centaines de TX/s).
- **Valider** la scalabilité du DAG, du système UTXO, de la [[fee-distribution]], et du stockage RocksDB sous charge soutenue.
- **Simuler** des profils utilisateurs réalistes (traders rapides, whales, mineurs de cubes, observateurs passifs, agent coordinateur).
- **Fournir un game engine** Edenite qui exerce les APIs [[nft-system|NFT]] (mint, burn, burn-batch) et [[multi-ledger]] (création de ledger secondaire, création et mint de token custom).
- **Offrir une visibilité** en temps réel via un TUI ratatui et/ou un dashboard web WebSocket.

Le simulateur est un binaire Rust indépendant (pas un workspace member du DAG-PMS principal) qui communique exclusivement via les APIs HTTP du gateway.

## Dates

| | Date |
|---|---|
| Créée | 2026-02-15 |
| Dernière mise à jour | 2026-03-19 |
| Version d'introduction | v0.1.0 |

**Commits clés :**

| Date | Commit | Description |
|------|--------|-------------|
| 2026-02-15 | `129f73a` | Introduction du simulator + game engine Edenite |
| 2026-02-16 | `e29896f` | Testnet deployment stack avec simulator |
| 2026-03-10 | `41fc717` | Fix OOM crash sur VPS : bounded channels, cube_registry cleanup, memory limits |
| 2026-03-11 | `63d4d64` | Ajout de l'agent coordinator (wallet du nœud) |

## Architecture

### Vue d'ensemble

```
+──────────────────────────────────────────────────────────────────+
│                        pms-simulator                             │
│                                                                  │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐   │
│  │ RandomAgent   │  │ SmartAgent   │  │ ObserverAgent        │   │
│  │ (N instances) │  │ (Gemini AI)  │  │ (monitoring passif)  │   │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────────────┘   │
│         │                  │                  │                   │
│  ┌──────┴──────┐  ┌──────┴───────┐          │                   │
│  │ Coordinator │  │  Funder      │          │                   │
│  │ Agent       │  │  (bootstrap) │          │                   │
│  └──────┬──────┘  └──────────────┘          │                   │
│         │                                    │                   │
│  ┌──────┴────────────────────────────────────┴───────────────┐  │
│  │                    AgentContext (shared)                    │  │
│  │  ┌────────────┐ ┌────────────┐ ┌──────────────────────┐   │  │
│  │  │ DagClient  │ │ CommsRouter│ │ GameEngine (Edenite) │   │  │
│  │  │ (HTTP+TLS) │ │ (P2P local)│ │ Arc<RwLock<>>        │   │  │
│  │  └─────┬──────┘ └─────┬──────┘ └──────────────────────┘   │  │
│  │        │               │                                    │  │
│  │  ┌─────┴──────┐ ┌─────┴──────┐ ┌──────────────────────┐   │  │
│  │  │ MetricsTx  │ │ PeerReg    │ │ CancellationToken    │   │  │
│  │  │ (mpsc)     │ │ Arc<RwLock>│ │ (graceful shutdown)  │   │  │
│  │  └────────────┘ └────────────┘ └──────────────────────┘   │  │
│  └───────────────────────────────────────────────────────────┘  │
│                                                                  │
│  ┌───────────────┐  ┌───────────────┐  ┌───────────────┐       │
│  │ MetricAggr    │  │ TUI (ratatui) │  │ Web Dashboard │       │
│  │ (1s window)   │  │ (terminal)    │  │ (Axum + WS)   │       │
│  └───────────────┘  └───────────────┘  └───────────────┘       │
+──────────────────────────────────────────────────────────────────+
                              │
                              │ HTTPS (reqwest)
                              ▼
                    ┌──────────────────┐
                    │   pms-gateway    │
                    │   (port 8443)    │
                    └──────────────────┘
```

### Séquence de démarrage

1. Charger le fichier TOML principal + fichiers agents externes (`agent_files`)
2. Résoudre les secrets (`env:VAR_NAME` dans les champs config)
3. Créer le `DagClient` HTTP + health check du gateway (`GET /livez`)
4. Créer le `GeminiClient` si des agents `smart` sont configurés (API key ou OAuth browser flow)
5. Démarrer le pipeline de métriques (channel `mpsc` 4096 + `run_aggregator`)
6. Démarrer le `CommsRouter` (channel `mpsc` 2048 pour le chat log global) + web dashboard (broadcast 256)
7. Construire le wallet du coordinator à partir de la config (hex -> base64)
8. Créer les wallets agents en parallèle via `POST /v1/wallet/create` (semaphore 50 concurrent)
9. Setup `GameEngine` : créer [[multi-ledger|ledger]] `eden` + token `edenite` (idempotent, ignore 409)
10. `Funder` : faucet PMS en parallèle (semaphore 30) + mint cubes [[nft-system|NFT]] en parallèle (semaphore 30)
11. Spawner chaque agent comme tokio task (1 task par agent, jitter aléatoire pour éviter thundering herd)
12. Timer de durée optionnel (`duration_secs`, 0 = infini)
13. Memory watchdog : log RSS toutes les 30s, shutdown gracieux si RSS > 400 MB
14. Forward des messages chat vers WebSocket broadcast + TUI
15. TUI ratatui ou mode headless (Ctrl+C / SIGTERM)
16. Arrêt : `CancellationToken` -> await all handles

### Channels et backpressure

| Channel | Capacité | Usage |
|---------|----------|-------|
| `metrics_tx` / `metrics_rx` | `mpsc(4096)` | Pipeline de métriques agents -> aggregator |
| `chat_log_tx` / `chat_log_rx` | `mpsc(2048)` | Chat global pour TUI display |
| `ws_tx` / `ws_rx` | `broadcast(256)` | WebSocket fan-out vers clients web |
| `tui_chat_tx` / `tui_chat_rx` | `mpsc(512)` | Forward chat vers TUI (drop si full) |
| Agent inbox (par agent) | `mpsc(256)` | Messages P2P par agent (`AGENT_INBOX_CAPACITY`) |

Tous les channels utilisent `try_send()` (non-bloquant) pour éviter que les agents lents ne bloquent les agents rapides.

## Configuration

### Fichier TOML principal

Le simulateur se configure via un fichier TOML passé en argument CLI (`--config`). Les agents peuvent être définis inline (`[[agents]]`) ou dans des fichiers externes via `agent_files`.

Trois profils de config sont fournis :

| Fichier | Environnement | Agents | TUI | Web |
|---------|---------------|--------|-----|-----|
| `simulator.dev.toml` | Local | ~252 (via `agents_dev.toml`) | oui | oui (9090) |
| `simulator.docker.toml` | Docker | ~250 (via `agents_docker.toml`) | non | oui (9090) |
| `simulator.testnet.toml` | VPS testnet | ~98 (via `agents_testnet.toml`) | non | oui (9090) |

#### Sections du TOML

**Root :**

| Champ | Type | Default | Description |
|-------|------|---------|-------------|
| `agent_files` | `string[]` | `[]` | Fichiers TOML d'agents (chemin relatif au config). Doit être placé avant la première section `[...]`. |

**`[server]` :**

| Champ | Type | Default | Description |
|-------|------|---------|-------------|
| `url` | string | **requis** | URL du gateway (ex: `https://127.0.0.1:8443`) |
| `admin_token` | string? | `null` | Bearer token pour les endpoints admin. Supporte `"env:VAR"`. |
| `api_key` | string? | `null` | [[api-key-authentication|API key]] PMS (header `X-API-Key`). Supporte `"env:VAR"`. |
| `ledger_id` | string? | `null` | Préfixe [[multi-ledger|ledger]] pour multi-ledger |
| `accept_invalid_certs` | bool | `true` | Accepter les certificats TLS auto-signés |

**`[gemini]` (optionnel, requis si agents `smart`) :**

| Champ | Type | Default | Description |
|-------|------|---------|-------------|
| `api_key` | string? | `null` | Clé API Gemini. Supporte `"env:GEMINI_API_KEY"`. |
| `client_id` | string? | `null` | OAuth client ID (mode navigateur) |
| `client_secret` | string? | `null` | OAuth client secret |
| `model` | string | `"gemini-2.0-flash"` | Modèle Gemini |

**`[simulation]` :**

| Champ | Type | Default | Description |
|-------|------|---------|-------------|
| `duration_secs` | u64 | `0` | Durée en secondes (`0` = infini) |
| `base_tick_ms` | u64 | `1000` | Tick de base |
| `faucet_amount` | string | `"50.00"` | PMS distribués par agent au démarrage |

**`[simulation.game]` (optionnel, active le game engine Edenite) :**

| Champ | Type | Default | Description |
|-------|------|---------|-------------|
| `ledger_id` | string | **requis** | ID du [[multi-ledger|ledger]] de jeu (ex: `"eden"`) |
| `network_id` | string | **requis** | Network ID du [[multi-ledger|ledger]] de jeu |
| `symbol` | string? | `null` | Symbole du token natif (ex: `"EDN"`) |
| `divisor` | f64? | `19300000000` | Diviseur de la formule de reward Edenite |

**`[coordinator]` (optionnel, requis si agents `coordinator`) :**

| Champ | Type | Description |
|-------|------|-------------|
| `private_key_hex` | string | Clé privée hex (64 chars). Supporte `"env:VAR"`. |
| `address` | string | Adresse du coordinator. Supporte `"env:VAR"`. |

**`[tui]` :**

| Champ | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `true` | Activer le dashboard terminal (ratatui) |
| `refresh_ms` | u64 | `250` | Fréquence de rafraîchissement (ms) |

**`[web]` :**

| Champ | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `false` | Activer le dashboard web (WebSocket) |
| `port` | u16 | `9090` | Port du serveur web |

### Docker Compose

Le fichier `docker-compose.simulator.yml` déploie le simulateur dans un container Docker avec :

```bash
# Lancer le simulateur en Docker
docker compose -f docker-compose.simulator.yml up --build -d

# Logs
docker logs -f pms-simulator

# Réseau custom
PMS_NETWORK=my-network docker compose -f docker-compose.simulator.yml up --build -d

# Port custom
SIMULATOR_PORT=3000 docker compose -f docker-compose.simulator.yml up --build -d
```

**Volumes montés :**

| Local | Container | Description |
|-------|-----------|-------------|
| `tools/simulator/simulator.docker.toml` | `/app/config/simulator.toml` | Config principale |
| `tools/simulator/agents_docker.toml` | `/app/config/agents_docker.toml` | Définitions d'agents |
| Cache Gemini OAuth | `/root/.cache/pms-simulator` | Token OAuth (si smart agents) |

**Variables d'environnement Docker :**

| Variable | Default | Description |
|----------|---------|-------------|
| `PMS_NETWORK` | `dag-pms_pms-public` | Nom du réseau Docker externe |
| `SIMULATOR_PORT` | `9090` | Port exposé pour le dashboard web |
| `GEMINI_TOKEN_CACHE` | `~/Library/Caches/pms-simulator` | Chemin local du cache OAuth Gemini |

**Variables d'environnement pour le testnet :**

| Variable | Description |
|----------|-------------|
| `PMS_ADMIN_TOKEN` | Bearer token pour les endpoints admin |
| `PMS_API_KEY` | [[api-key-authentication|API key]] PMS (header `X-API-Key`) |
| `PMS_COORDINATOR_KEY` | Clé privée hex du coordinator |
| `PMS_COORDINATOR_ADDR` | Adresse du coordinator |

## Crates et Fichiers

Le simulateur est un crate standalone dans `tools/simulator/` (pas un workspace member du projet principal).

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-simulator` | `tools/simulator/Cargo.toml` | Manifeste du crate (v0.1.1, edition 2021) |
| `pms-simulator` | `tools/simulator/src/main.rs` | Point d'entrée : CLI, orchestration du lifecycle complet (chargement config, création wallets, spawn agents, TUI/headless, shutdown) |
| `pms-simulator` | `tools/simulator/src/config.rs` | Structures de configuration TOML (`SimConfig`, `AgentDef`, `AgentBehavior`, `GameConfig`, `AgentGameConfig`, `CoordinatorConfig`) + résolution des secrets `env:VAR` |
| `pms-simulator` | `tools/simulator/src/client.rs` | Client HTTP typé (`DagClient`) : wallet, send, faucet, balance, tips, supply, ledger/token admin, NFT mint/burn. Retry automatique sur 429 avec backoff exponentiel. |
| `pms-simulator` | `tools/simulator/src/game.rs` | Game engine Edenite : `GameEngine` struct, `CubeAttributes`, formule de reward, mint/burn de cubes [[nft-system|NFT]], `cube_registry` (HashMap local) |
| `pms-simulator` | `tools/simulator/src/agent/mod.rs` | Trait `Agent` (name, wallet, tick), `AgentContext` (shared state), `AgentHandle`, `spawn_agent()` avec jitter anti-thundering-herd |
| `pms-simulator` | `tools/simulator/src/agent/random.rs` | `RandomAgent` : transactions PMS aléatoires + game loop (burn cubes, send EDN, re-mint). Auto-refuel via faucet quand PMS < 10. |
| `pms-simulator` | `tools/simulator/src/agent/smart.rs` | `SmartAgent` : piloté par Gemini AI. Construit un contexte (solde, peers, messages, historique) et exécute les directives Gemini (Send, Wait, Observe, Message). Auto-diagnostic sur erreurs. |
| `pms-simulator` | `tools/simulator/src/agent/observer.rs` | `ObserverAgent` : agent passif. Alterne entre tips, supply, et balances. Alimente le pipeline de métriques sans transacter. |
| `pms-simulator` | `tools/simulator/src/agent/coordinator.rs` | `CoordinatorAgent` : utilise le wallet du nœud coordinateur. Envoie des PMS aléatoires aux peers. Pas de refuel (financé par les fees réseau), pas de game loop. |
| `pms-simulator` | `tools/simulator/src/agent/funder.rs` | `Funder` : bootstrap -- faucet PMS et mint cubes [[nft-system|NFT]] en parallèle (semaphore 30 concurrent). Pré-génère les specs de cubes (rand) avant les appels async. |
| `pms-simulator` | `tools/simulator/src/gemini.rs` | `GeminiClient` : intégration Gemini API (API key ou OAuth browser flow). `AgentDirective` enum (Send, Wait, Observe, Message). Cache de tokens OAuth local. Retry 429 avec backoff. |
| `pms-simulator` | `tools/simulator/src/comms/mod.rs` | `CommsRouter` : routeur de messages P2P local. Inbox par agent (`mpsc(256)`), broadcast, log global. `try_send()` partout pour éviter le blocage. |
| `pms-simulator` | `tools/simulator/src/comms/types.rs` | `AgentMessage` enum : `Text`, `TxNotification`, `Info`, `Error` (avec self-diagnosis). Sérialisable JSON pour WebSocket. |
| `pms-simulator` | `tools/simulator/src/metrics/mod.rs` | `MetricEvent` enum (TransactionSent, AgentError, AgentFunded, TipsCount, SupplyUpdate, BalanceUpdate, GeminiDecision). `MetricsSnapshot` pour le TUI. |
| `pms-simulator` | `tools/simulator/src/metrics/aggregator.rs` | `run_aggregator()` : consomme les `MetricEvent`, calcule TPS (fenêtre glissante 10s), latences p50/p95/p99, maintient le `SharedMetrics` (Arc<Mutex>). |
| `pms-simulator` | `tools/simulator/src/tui/mod.rs` | `TuiApp` : boucle principale ratatui, gestion clavier (q / Ctrl+C), drain du chat, rendu périodique. |
| `pms-simulator` | `tools/simulator/src/tui/dashboard.rs` | `render()` : layout ratatui 5 zones (header, sparklines TPS + barres latence, DAG status + table agents, chat P2P, event log). |
| `pms-simulator` | `tools/simulator/src/web.rs` | Dashboard web Axum : `GET /` (page HTML inline avec CSS dark + JS WebSocket), `GET /ws` (WebSocket fan-out). Affiche les messages agents en temps réel. |
| `pms-simulator` | `tools/simulator/src/types.rs` | Types de requêtes/réponses pour l'API PMS : `WalletInfo`, `SendSimpleRequest`, `BalanceRequest`, `FaucetRequest`, `CreateLedgerRequest`, `CreateTokenRequest`, `MintTokenRequest`, `MintNftRequest`, `BurnNftSimpleRequest`, `BurnNftBatchSimpleRequest`, `SendResponse` (avec `transfer_fee` v0.5.17), etc. |
| `pms-simulator` | `tools/simulator/src/error.rs` | `SimError` enum (Http, ServerError, Gemini, InsufficientBalance, Config, Bootstrap, Json, Other). `SimResult<T>` type alias. |
| -- | `tools/simulator/simulator.dev.toml` | Config pour développement local (gateway localhost:8443, TUI active, web 9090) |
| -- | `tools/simulator/simulator.docker.toml` | Config pour Docker (gateway via DNS interne `pms-gateway:8443`, TUI désactivé) |
| -- | `tools/simulator/simulator.testnet.toml` | Config pour testnet VPS (secrets via env vars, coordinator activé) |
| -- | `tools/simulator/agents_dev.toml` | Profils agents dev : ~252 agents (100 user, 50 fast, 50 miner, 10 whale, 30 sniper, 10 saver, 2 obs, 1 coordinator) |
| -- | `tools/simulator/agents_docker.toml` | Profils agents Docker : ~250 agents (identique à dev, sans coordinator) |
| -- | `tools/simulator/agents_testnet.toml` | Profils agents testnet : ~98 agents (charge réduite pour VPS) |
| -- | `Dockerfile.simulator` | Build multi-stage : `rust:1-slim` (build) -> `debian:trixie-slim` (runtime). Healthcheck sur port 9090. |
| -- | `docker-compose.simulator.yml` | Service Docker : volumes config, réseau externe `dag-pms_pms-public`, port 9090 |

## Agent Types

Le simulateur définit **4 types d'agents** via l'enum `AgentBehavior`, déployables en **7 profils** distincts via la configuration TOML :

### 1. Random (`AgentBehavior::Random`)

Agent principal pour la génération de trafic. Exécute deux boucles à chaque tick :
- **Boucle PMS** : envoie `sends_per_tick` transactions PMS séquentielles à des peers aléatoires selon `send_probability`. Chaque send dépend de l'UTXO du précédent (chaîne séquentielle par wallet). `sends_per_tick` permet de multiplier le débit PMS sans ajouter d'agents.
- **Boucle Game** (si `[agents.game]` configuré) : burn cubes → gagner EDN → envoyer EDN → re-mint. Lock-free v0.5.17 : write lock tenu uniquement pour les opérations de registre (HashMap insert/remove), jamais pendant les appels HTTP. `burn_cooldown_ticks` empêche le remint immédiat après burn pour laisser `fee_distribution` livrer l'EDN. `edn_sends_per_tick` (v0.5.18) multiplie le nombre d'envois EDN séquentiels en Phase 2 (même pattern que PMS `sends_per_tick`).

Auto-refuel via faucet quand le solde PMS tombe sous 10 PMS (constante `LOW_BALANCE_THRESHOLD`).

**Pourquoi `sends_per_tick` :** Eden agents génèrent ~2000 TPS car chaque `game_tick` mint 80-120 NFT cubes (80-120 blocs). PMS agents ne généraient que ~200 TPS (1 TX par tick par agent). `sends_per_tick` comble cette disparité. Le moteur supporte ~10 000 TPS (prouvé par `test_pms_throughput_benchmark`).

**Pourquoi `edn_sends_per_tick` (v0.5.18) :** Phase 2 (envoi EDN) ne se déclenchait presque jamais car `burn_cooldown_ticks` (10s) < `distribution_interval_sec` (30s) → l'agent remintait AVANT de recevoir ses EDN. Fix : aligner cooldown > distribution interval (10s testnet), et multiplier les envois EDN par tick quand Phase 2 se déclenche.

**Profils déployés :**

| Profil | Prefix | Count (dev) | Interval | PMS sends/tick | Game | EDN sends/tick | Cooldown |
|--------|--------|-------------|----------|---------------|------|---------------|----------|
| **Casual Users** | `user` | 100 | 1000ms | 5 | oui (5 cubes, 10-50% EDN) | 5 | 15 |
| **Fast Traders** | `fast` | 50 | 500ms | 15 | non | — | — |
| **Miners** | `miner` | 50 | 1000ms | 1 | oui (10 cubes, 5-20% EDN) | 10 | 15 |
| **Whales** | `whale` | 10 | 5000ms | 3 | oui (20 cubes, 30-80% EDN) | 3 | 5 |
| **Snipers** | `sniper` | 30 | 200ms | 20 | non | — | — |
| **Savers** | `saver` | 10 | 10000ms | 5 | oui (3 cubes, 40-90% EDN) | 5 | 3 |

**Calcul TPS théorique (dev, sends_per_tick) :** users 150 + fast 1200 + snipers 3000 + whales 4.8 + savers 2 = **~4357 PMS tx/s**
**Calcul TPS théorique (dev, edn_sends_per_tick) :** users 500 + miners 500 + whales 6 + savers 5 = **~1011 EDN tx/s** (était ~3/s avant fix)

### 2. Smart (`AgentBehavior::Smart`)

Agent piloté par l'IA Google Gemini. Toutes les `ai_interval` ticks, il envoie son contexte complet (solde, liste des peers, messages reçus, 10 dernières actions) à Gemini et exécute la directive JSON retournée.

**Directives Gemini :**

| Directive | Description |
|-----------|-------------|
| `Send` | Envoyer X PMS à un agent nommé (Gemini choisit destinataire, montant, raison) |
| `Wait` | Ne rien faire (attendre que le solde remonte, par exemple) |
| `Observe` | Interroger le réseau : tips, supply, ou solde |
| `Message` | Envoyer un message texte à un autre agent (affiché dans TUI et web) |

En cas d'erreur, l'agent smart effectue un auto-diagnostic et broadcast un message `Error` avec explication.

### 3. Observer (`AgentBehavior::Observer`)

Agent passif qui ne fait aucune transaction. Alterne entre 3 requêtes de monitoring à chaque tick (rotation cyclique) :

| Tick % 3 | Requête | Métrique émise |
|----------|---------|----------------|
| 0 | `POST /v1/dag/tips` (max 10) | `TipsCount` |
| 1 | `GET /v1/supply` | `SupplyUpdate` (circulating + UTXO count) |
| 2 | `POST /v1/balance` (pour chaque peer) | `BalanceUpdate` par agent |

Alimente le pipeline de métriques pour le TUI et le dashboard web.

### 4. Coordinator (`AgentBehavior::Coordinator`)

Agent qui utilise le wallet du nœud coordinateur (configuré via `[coordinator]`). Envoie des PMS aléatoires aux peers.

| Propriété | Valeur |
|-----------|--------|
| Financement | Pas de faucet -- financé par les fees réseau accumulés |
| Game loop | Non |
| Inbox / messages P2P | Non |
| Send probability | Configurable (default 100%) |
| Montants | Configurable (default 0.1-1.0 PMS) |
| Intervalle | Configurable (default 60s pour testnet) |

## Fonctions Clés

### Orchestration (`main.rs`)

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `main()` | `tools/simulator/src/main.rs` | Point d'entrée async : charge config, crée wallets, setup game engine, spawn agents, TUI/headless, shutdown gracieux |
| `read_rss_bytes()` | `tools/simulator/src/main.rs` | Lit le RSS du processus via `/proc/self/statm` (Linux) ou `task_info` (macOS) pour le memory watchdog |

### Agents (`agent/`)

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `spawn_agent()` | `tools/simulator/src/agent/mod.rs` | Spawne un agent comme tokio task avec jitter aléatoire (0..interval_ms) pour éviter le thundering herd |
| `Agent::tick()` | `tools/simulator/src/agent/mod.rs` | Trait method exécutée à chaque interval_ms. Les erreurs sont loguées mais pas fatales. |
| `RandomAgent::tick()` PMS loop | `tools/simulator/src/agent/random.rs` | Boucle `sends_per_tick` itérations : chaque itération décide d'envoyer (probabilité), choisit un peer aléatoire, et envoie. Les sends sont séquentiels (dépendance UTXO par wallet). Break on error (UTXO exhaustion). v0.5.17 : `sends_per_tick` multiplie le débit PMS sans ajouter d'agents. |
| `RandomAgent::game_tick()` | `tools/simulator/src/agent/random.rs` | Game loop (lock-free v0.5.17) : Phase 1 (batch burn cubes → EDN, write lock only for registry drain), Phase 2 (send EDN ×`edn_sends_per_tick` à peers, no lock — cached client, v0.5.18), Phase 3 (re-mint cubes, write lock only for registry insert). `burn_cooldown_ticks` pauses reminting after burn to allow fee_distribution to deliver EDN. |
| `RandomAgent::refuel()` | `tools/simulator/src/agent/random.rs` | Auto-refuel 50 PMS via `POST /admin/faucet` quand solde < 10 PMS |
| `SmartAgent::build_context()` | `tools/simulator/src/agent/smart.rs` | Construit la string de contexte pour Gemini (solde, peers, messages, historique) |
| `SmartAgent::execute_directive()` | `tools/simulator/src/agent/smart.rs` | Exécute la directive Gemini courante (Send, Wait, Observe, Message) |
| `SmartAgent::diagnose_error()` | `tools/simulator/src/agent/smart.rs` | Auto-diagnostic en cas d'erreur : construit une explication avec solde, dernière directive, messages en attente |
| `Funder::fund_all_with_cubes()` | `tools/simulator/src/agent/funder.rs` | Bootstrap : faucet PMS en parallèle (sem 30), puis mint cubes [[nft-system|NFT]] en parallèle (sem 30). Retourne `HashMap<agent_name, Vec<cube_token_id>>`. |

### Game Engine (`game.rs`)

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `GameEngine::setup()` | `tools/simulator/src/game.rs` | Crée le [[multi-ledger|ledger]] `eden` + token `edenite` via API admin. Idempotent (ignore 409 CONFLICT). |
| `GameEngine::mint_cube()` | `tools/simulator/src/game.rs` | Mint un cube [[nft-system|NFT]] avec attributs aléatoires (weight, size, density) via `POST /v1/nft/mint`. Enregistre dans `cube_registry`. |
| `GameEngine::burn_cube_for_edenite()` | `tools/simulator/src/game.rs` | Burn un cube [[nft-system|NFT]] via `POST /v1/nft/burn-simple`, calcule le reward, mint EDN via `POST /admin/tokens/mint`. Retire du `cube_registry`. |
| `GameEngine::burn_cubes_for_edenite()` | `tools/simulator/src/game.rs` | Batch burn de N cubes en un seul appel (`POST /v1/nft/burn-batch-simple`), mint le total EDN. |
| `GameEngine::send_edenite()` | `tools/simulator/src/game.rs` | Envoie de l'EDN à une adresse via `POST /v1/wallet/send-simple` (UTXO transfer avec asset_id). Diagnostic logging v0.5.17 : pre-send (montant, destinataire, asset) + post-send (block_id, gas_fee, transfer_fee). |
| `GameEngine::register_cube()` | `tools/simulator/src/game.rs` | Enregistre un cube pré-mint dans le registre local (utilisé par le `Funder` en bootstrap). |
| `GameEngine::drain_cubes()` | `tools/simulator/src/game.rs` | (v0.5.17) Lock-free helper : retire les cubes du registre et retourne leurs attributs + total EDN attendu. Appeler sous write lock, puis drop lock avant HTTP. |
| `GameEngine::restore_cubes()` | `tools/simulator/src/game.rs` | (v0.5.17) Lock-free helper : remet les cubes dans le registre en cas d'échec du burn. |
| `GameEngine::execute_burn_batch()` | `tools/simulator/src/game.rs` | (v0.5.17) Static : exécute le batch burn HTTP sans lock. |
| `GameEngine::generate_mint_specs()` | `tools/simulator/src/game.rs` | (v0.5.17) Static : génère les specs de cubes (pure RNG, pas de lock). |
| `GameEngine::execute_mints_parallel()` | `tools/simulator/src/game.rs` | (v0.5.17) Static : exécute les mints HTTP en parallèle (semaphore 30) sans lock. Retourne `(minted, ok, err)`. |
| `GameEngine::register_minted()` | `tools/simulator/src/game.rs` | (v0.5.17) Lock-free helper : enregistre les cubes mintés dans le registre. Appeler sous write lock bref. |
| `CubeAttributes::edenite_reward()` | `tools/simulator/src/game.rs` | Calcule le reward : `(weight * size * density) / divisor`. Diviseur par défaut : `19,300,000,000`. |

### Client HTTP (`client.rs`)

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `DagClient::new()` | `tools/simulator/src/client.rs` | Crée le client reqwest avec timeout 15s et gestion TLS |
| `DagClient::with_ledger()` | `tools/simulator/src/client.rs` | Clone le client avec un préfixe [[multi-ledger|ledger]] (`/l/{ledger_id}`) |
| `DagClient::send_with_retry()` | `tools/simulator/src/client.rs` | Retry automatique sur 429 (Too Many Requests) avec backoff exponentiel : 500ms, 1s, 2s, 4s, 8s (max 5 tentatives, cap 10s) |
| `DagClient::health_check()` | `tools/simulator/src/client.rs` | `GET /livez` -- vérifie que le gateway est accessible |

### Communications (`comms/`)

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `CommsRouter::register()` | `tools/simulator/src/comms/mod.rs` | Enregistre un agent et retourne son inbox (`mpsc::Receiver`, capacité 256) |
| `CommsRouter::send_to()` | `tools/simulator/src/comms/mod.rs` | Envoie un message à un agent spécifique + log global. `try_send()` non-bloquant. |
| `CommsRouter::broadcast()` | `tools/simulator/src/comms/mod.rs` | Broadcast un message à tous les agents sauf l'émetteur. `try_send()` non-bloquant. |

### Métriques (`metrics/`)

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `run_aggregator()` | `tools/simulator/src/metrics/aggregator.rs` | Boucle async : consomme `MetricEvent`, calcule TPS (fenêtre 10s), latences p50/p95/p99 (sur les 1000 dernières), maintient `SharedMetrics` |
| `create_shared_metrics()` | `tools/simulator/src/metrics/aggregator.rs` | Crée le `Arc<Mutex<MetricsSnapshot>>` partagé entre aggregator et TUI |

### Gemini AI (`gemini.rs`)

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `GeminiClient::with_api_key()` | `tools/simulator/src/gemini.rs` | Crée un client Gemini avec API key directe |
| `GeminiClient::with_oauth()` | `tools/simulator/src/gemini.rs` | Crée un client Gemini avec OAuth browser flow. Charge/refresh les tokens en cache (`~/.cache/pms-simulator/gemini_tokens.json`). |
| `GeminiClient::decide()` | `tools/simulator/src/gemini.rs` | Appelle Gemini `generateContent` avec system prompt + contexte. Force `response_mime_type: application/json`. Parse la réponse en `AgentDirective`. Retry 429 avec backoff (2, 4, 8, 16, 32s). |
| `oauth_browser_flow()` | `tools/simulator/src/gemini.rs` | OAuth Authorization Code flow complet : ouvre le navigateur, attend le redirect sur localhost:18492, échange le code pour des tokens. |

### TUI & Web

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `TuiApp::run()` | `tools/simulator/src/tui/mod.rs` | Boucle ratatui : drain chat, render dashboard, gestion clavier (q / Ctrl+C) |
| `dashboard::render()` | `tools/simulator/src/tui/dashboard.rs` | Layout 5 zones : header (TPS, TX total, errors), sparkline TPS + barchart latence, DAG status + table agents, chat P2P, event log |
| `run_web_server()` | `tools/simulator/src/web.rs` | Serveur Axum : page HTML dark mode + WebSocket `/ws` pour messages en temps réel |

## Memory Safety

### Problématique

Le simulateur a subi un crash OOM sur VPS (commit `41fc717`, 2026-03-10) avec 250+ agents générant des milliers de messages par seconde. Les causes identifiées :

1. **Channels non bornés** : les channels `mpsc` illimités accumulaient des messages plus vite que le consommateur ne les traitait.
2. **`cube_registry` non nettoyé** : le `HashMap` dans `GameEngine` croissait indéfiniment car les cubes brûlés étaient retirés mais les re-mints s'accumulaient.
3. **Absence de limite mémoire** : pas de watchdog RSS, le processus consommait toute la RAM du VPS.

### Protections implémentées

| Mécanisme | Implémentation | Fichier |
|-----------|----------------|---------|
| **Bounded metrics channel** | `mpsc::channel(4096)` | `tools/simulator/src/main.rs` (ligne 94) |
| **Bounded chat log channel** | `mpsc::channel(2048)` | `tools/simulator/src/main.rs` (ligne 102) |
| **Bounded WebSocket broadcast** | `broadcast::channel(256)` | `tools/simulator/src/main.rs` (ligne 106) |
| **Bounded TUI chat channel** | `mpsc::channel(512)` | `tools/simulator/src/main.rs` (ligne 390) |
| **Bounded agent inbox** | `mpsc::channel(256)` (constante `AGENT_INBOX_CAPACITY`) | `tools/simulator/src/comms/mod.rs` (ligne 9) |
| **Non-blocking sends** | `try_send()` partout (drop si channel plein, pas de blocage) | `tools/simulator/src/comms/mod.rs`, `tools/simulator/src/agent/random.rs` |
| **Memory watchdog** | Log RSS toutes les 30s, shutdown gracieux si RSS > 400 MB | `tools/simulator/src/main.rs` (lignes 361-386) |
| **cube_registry cleanup** | `HashMap::remove()` lors du burn, pas d'accumulation | `tools/simulator/src/game.rs` (lignes 194, 247) |
| **DOM limit (web dashboard)** | Max 500 messages dans le DOM (JS `removeChild`) | `tools/simulator/src/web.rs` (INDEX_HTML, ligne 230) |
| **Chat log cap (TUI)** | Max 50 messages en mémoire | `tools/simulator/src/tui/mod.rs` (ligne 45) |
| **Event log cap (aggregator)** | Max 50 events récents | `tools/simulator/src/metrics/aggregator.rs` (lignes 44, 53) |
| **Latency window cap** | Max 1000 latences en mémoire | `tools/simulator/src/metrics/aggregator.rs` (ligne 33) |
| **TPS history cap** | Max 120 points (2 min à 1 snapshot/s) | `tools/simulator/src/metrics/aggregator.rs` (ligne 87) |
| **Docker memory limits** | Container avec `mem_limit` dans docker-compose | `docker-compose.simulator.yml` |

### Backpressure strategy

Tous les canaux de communication utilisent `try_send()` au lieu de `.send().await`. Cela signifie que si un consommateur est plus lent que le producteur, les messages sont **silencieusement droppés** plutôt que de bloquer l'émetteur. C'est un choix délibéré : la perte de quelques messages de métriques ou de chat est acceptable, mais bloquer un agent qui transacte ne l'est pas.

## Interactions

### Endpoints API appelés

Le simulateur communique exclusivement avec le gateway PMS via HTTPS :

| Méthode | Endpoint | Usage | Appelant |
|---------|----------|-------|----------|
| GET | `/livez` | Health check au démarrage | `main.rs` |
| POST | `/v1/wallet/create` | Créer [[wallet-factory|wallet]] par agent (bootstrap) | `main.rs` |
| POST | `/v1/wallet/send-simple` | Envoyer PMS (Random, Smart, Coordinator) | `random.rs`, `smart.rs`, `coordinator.rs` |
| POST | `/v1/balance` | Solde PMS d'une adresse | `random.rs`, `smart.rs`, `coordinator.rs`, `observer.rs` |
| POST | `/v1/dag/tips` | Tips du DAG (max 10) | `observer.rs`, `smart.rs` |
| GET | `/v1/supply` | Supply circulante + UTXO count | `observer.rs`, `smart.rs` |
| POST | `/admin/faucet` | Faucet PMS (bootstrap + refuel) | `funder.rs`, `random.rs` |
| POST | `/admin/ledgers/create` | Créer le [[multi-ledger|ledger]] `eden` (game engine setup) | `game.rs` |
| POST | `/admin/tokens/create` | Créer le token `edenite` (game engine setup) | `game.rs` |
| POST | `/admin/tokens/mint` | Mint EDN (reward après burn, ou send EDN) | `game.rs` |
| POST | `/v1/nft/mint` | Mint cube [[nft-system|NFT]] (bootstrap + re-mint) | `game.rs`, `funder.rs` |
| POST | `/v1/nft/burn-simple` | Burn 1 cube [[nft-system|NFT]] | `game.rs` |
| POST | `/v1/nft/burn-batch-simple` | Burn N cubes [[nft-system|NFT]] en un bloc | `game.rs` |

### Coordinator wallet

L'agent `Coordinator` utilise le wallet du nœud (la paire de clés qui signe les blocs en production). Cela permet de :
- Injecter des PMS dans la circulation depuis les fees accumulés par le nœud.
- Tester le send depuis le [[wallet-factory|wallet]] coordinateur en conditions réelles.

Les credentials sont résolues depuis des variables d'environnement (`env:PMS_COORDINATOR_KEY`, `env:PMS_COORDINATOR_ADDR`) pour ne jamais les commiter dans le code.

### Deployment testnet

Le script `scripts/deploy-testnet.sh` déploie le simulateur aux côtés de l'engine et du gateway. Le fichier `simulator.testnet.toml` est configuré pour :
- Se connecter au gateway via DNS Docker interne (`pms-gateway:8443`).
- Utiliser une charge réduite (~98 agents au lieu de ~252).
- Activer le coordinator agent (wallet du nœud).
- Désactiver le TUI (pas de terminal en Docker).
- Exposer le dashboard web sur le port 9090.

### Gemini AI (smart agents)

L'intégration Gemini supporte deux modes d'authentification :
1. **API key** : `gemini.api_key = "env:GEMINI_API_KEY"` -- simple et directe.
2. **OAuth browser flow** : `gemini.client_id` + `gemini.client_secret` -- ouvre le navigateur pour Google login, cache le token dans `~/.cache/pms-simulator/gemini_tokens.json`, refresh automatique à l'expiration.

Le modèle par défaut est `gemini-2.0-flash`. La température est fixée à 0.7 avec un max de 256 tokens de sortie, et le format de réponse est forcé en `application/json`.
