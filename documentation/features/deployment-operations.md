---
tags: [feature, infrastructure, ops]
created: 2026-03-14
updated: 2026-03-15
version: v0.4.3
---

# Deployment & Operations

## Resume

L'infrastructure de deploiement du moteur bancaire crypto DAG-PMS repose sur une architecture **4 processus** conteneurisee : **Engine** (coordinateur DAG), **Gateway** (proxy public), **Caddy** (terminaison TLS / Let's Encrypt) et **Prometheus** (metriques). Un cinquieme processus optionnel, le **Simulator**, est present sur le testnet pour generer du trafic realiste avec ~97 agents autonomes.

Le deploiement cible un VPS Linux unique (`/opt/pms`) et utilise Docker Compose pour orchestrer les services. Deux strategies de build coexistent :

- **Production (`docker-compose.yml`)** : build Docker sur le VPS via `docker compose build`.
- **Testnet (`docker-compose.testnet.yml`)** : cross-compilation locale (`linux/amd64` via `docker buildx`) puis transfert des images compressees via SCP + `docker load`. Aucun build Rust ne se fait sur le VPS testnet.

La securisation repose sur un schema TLS a deux niveaux : certificats auto-signes internes (Engine <-> Gateway) et Let's Encrypt automatique en facade (Caddy). L'Engine n'est jamais expose publiquement.

## Dates

| | Date |
|---|---|
| Creee | 2026-02-13 |
| Derniere mise a jour | 2026-03-14 |
| Version d'introduction | v0.1.0 |

## Architecture de Deploiement

### Topologie 4 processus (Production & Testnet)

```
Internet
   |
   v
Caddy (ports 80/443, Let's Encrypt automatique)
   |  reverse_proxy HTTPS
   v
Gateway (port 8443, TLS auto-signe interne)
   |  proxy HTTPS (danger_accept_invalid_certs pour cert auto-signe)
   v
Engine (port 8080, reseau interne uniquement, HTTPS auto-signe)
   |  API : HTTPS (default, configurable via api_tls_enabled)
   |  P2P : toujours TLS (independant de api_tls_enabled)
   v
RocksDB (volume Docker persistant)

Prometheus (port 9091, localhost uniquement)
   |  scrape HTTP ou HTTPS (insecure_skip_verify)
   +---> Engine :8080/metrics
   +---> Gateway :8443/metrics
```

> **Note (v0.4.3)** : Le champ `api_tls_enabled` (defaut: `true`) dans `[client]` de la config TOML permet de desactiver TLS sur l'API HTTP de l'Engine tout en gardant le P2P en TLS. Quand desactive, le Gateway doit utiliser `http://` dans `UPSTREAM_URL` et Prometheus doit utiliser `scheme: http`. Voir [[config-system#Separation TLS API / P2P (v0.4.3)]].

### Isolation reseau

Docker Compose definit deux reseaux :

| Reseau | Type | Services connectes |
|---|---|---|
| `pms-internal` | `bridge`, `internal: true` | Engine, Gateway, Prometheus |
| `pms-public` | `bridge` | Gateway, Caddy |

L'Engine est **isole du reseau public** : seul le Gateway peut le joindre via le reseau interne. Le Gateway est le **seul point d'entree public** du systeme.

### Ports exposes

| Service | Port | Visibilite | Protocole |
|---|---|---|---|
| Caddy | 80, 443 | Public (Internet) | HTTP/HTTPS (Let's Encrypt) |
| Gateway | 8443 | Interne Docker (expose via Caddy) | HTTPS (auto-signe) |
| Engine | 8080 | Interne Docker uniquement | HTTPS (auto-signe) |
| Prometheus | 9091 | `127.0.0.1` uniquement | HTTP |
| Simulator (testnet) | 9090 | Public | HTTP (dashboard web) |

## Docker Compose Configurations

Le projet definit **7 fichiers Docker Compose** pour differents environnements :

| Fichier | Environnement | Services | Usage |
|---|---|---|---|
| `docker-compose.yml` | **Production** | Engine, Gateway, Caddy, Prometheus | Deploiement mainnet sur VPS |
| `docker-compose.testnet.yml` | **Testnet** | Engine, Gateway, Caddy, Prometheus, Simulator | Deploiement testnet avec stress-test |
| `docker-compose.vps.yml` | **VPS Multi-Process** | Engine, Gateway, Prometheus, Caddy, (Promtail) | Architecture VPS avec log shipping optionnel |
| `docker-compose.test.yml` | **Test local** | Engine, Gateway, Prometheus | Tests Docker locaux (genere dynamiquement par `docker_test.sh`) |
| `docker-compose.e2e-prod.yml` | **E2E Prod Sim** | Engine, Gateway | Simulation E2E de production (pas de Caddy) |
| `docker-compose.bench.yml` | **Benchmark** | Engine (node unique) | Benchmark TPS mono-noeud |
| `docker-compose.simulator.yml` | **Simulator standalone** | Simulator | Simulateur IA autonome, se connecte a un reseau existant |

### Production (`docker-compose.yml`)

Services :
- **pms-engine** : Image `pms-node:latest`, utilisateur `pms`, volume `rocksdb_data`, config `config.prod.toml`
- **pms-gateway** : Image `pms-gateway:latest`, depends_on engine healthy, rate limit 1000 RPS / 2000 burst
- **caddy** : Image `caddy:2-alpine`, depends_on gateway healthy, volumes `caddy_data` et `caddy_config`
- **prometheus** : Image `prom/prometheus:v2.45.0`, bind `127.0.0.1:9091`, retention par defaut

### Testnet (`docker-compose.testnet.yml`)

Services supplementaires par rapport a la production :
- **pms-simulator** : Image `pms-simulator:testnet`, ~97 agents, dashboard web port 9090
- Limites memoire explicites : Engine 4 Go, Gateway/Prometheus/Simulator 512 Mo
- Retention Prometheus : 3 jours, 1 Go max
- Images pre-buildees localement (pas de `build:` dans le compose, uniquement `image:`)
- **(v0.4.3)** Le testnet simule la config prod avec HTTPS de bout en bout. Le Gateway utilise `danger_accept_invalid_certs(true)` pour accepter le certificat auto-signe de l'Engine. Le script de deploiement nettoie les anciennes images Docker avant `docker load` pour prevenir le bloat containerd.

## Dockerfiles

| Fichier | Cible | Base builder | Base runtime | Binaires produits |
|---|---|---|---|---|
| `Dockerfile` | Engine + tools-cli | `rustlang/rust:nightly` | `debian:trixie-slim` | `pms-node`, `tools-cli` |
| `Dockerfile.gateway` | Gateway + Dashboard | `rustlang/rust:nightly` + `node:20-alpine` | `debian:trixie-slim` | `pms-gateway` + fichiers statiques SPA React |
| `Dockerfile.testnet` | Engine (alpine) | `rust:1.89-alpine` | `alpine:3.20` | `pms-node`, `tools-cli` |
| `Dockerfile.simulator` | Simulator | `rust:1-slim` | `debian:trixie-slim` | `pms-simulator` |

### Build multi-stage (`Dockerfile` principal)

Le Dockerfile principal utilise **3 stages** avec cache de dependances :

1. **frontend-builder** (`node:20-alpine`) : compile le dashboard React (`pms-dashboard/`)
2. **builder** (`rustlang/rust:nightly`) : strategie de cache Cargo en 5 etapes :
   - (A) Copie des `Cargo.toml` + `Cargo.lock` uniquement
   - (B) Creation de fichiers source fictifs (`lib.rs` / `main.rs` vides)
   - (C) Build des dependances (layer cachee tant que les manifests ne changent pas)
   - (D) Copie du code source reel + `touch` pour invalider le cache
   - (E) Build final (recompile uniquement le code du projet)
3. **runtime** (`debian:trixie-slim`) : image minimale avec `ca-certificates`, `libssl-dev`, `curl`

Le binaire `tools-cli` est inclus dans l'image Engine pour permettre l'initialisation du coordinateur via `docker compose run`.

## Scripts de Deploiement

### Scripts principaux

| Script | Usage | Description |
|---|---|---|
| `scripts/deploy.sh` | `./deploy.sh <VPS_IP> [USER]` | Deploiement interactif complet production (git pull, build, init coordinateur, backup cles) |
| `scripts/deploy-testnet.sh` | `./deploy-testnet.sh <VPS_IP> [USER] [SSH_KEY]` | Deploiement testnet avec build local cross-compile + transfert SCP |
| `scripts/upgrade-testnet.sh` | `./upgrade-testnet.sh <VPS_IP> [USER] [SSH_KEY]` | Upgrade testnet sans toucher aux donnees (rebuild + restart selectif) |
| `scripts/setup_production.sh` | `./setup_production.sh <DOMAIN>` | Setup initial production : genere token admin, cles, Caddyfile, docker-compose.prod.yml |

### Scripts de test et debug

| Script | Usage | Description |
|---|---|---|
| `scripts/docker_test.sh` | `./docker_test.sh {setup\|down\|clean\|status}` | Stack de test locale 4 services (gen TLS, wallets, coordinator, treasury) |
| `scripts/verify_vps.sh` | `./verify_vps.sh {clean\|setup\|verify\|func\|all}` | Verification VPS : connectivite, rate limiting, API POST, metriques Prometheus |
| `scripts/run_e2e_prod.sh` | `./run_e2e_prod.sh [--verbose]` | Simulation E2E production via `docker-compose.e2e-prod.yml` |
| `scripts/benchmark_local.sh` | `./benchmark_local.sh [--docker] [--clean]` | Benchmark TPS mono-noeud (local ou Docker) avec rapport Markdown |
| `scripts/setup_test_cluster.sh` | `./setup_test_cluster.sh` | Setup cluster de test 3 noeuds (TLS, cles, docker compose up) |
| `scripts/verify_crash_recovery.sh` | `./verify_crash_recovery.sh` | Test de recovery apres crash : inject data, kill, restart, verification |

### Scripts de debug

| Script | Usage | Description |
|---|---|---|
| `scripts/debug_check_health.sh` | `./debug_check_health.sh <VPS_IP>` | Diagnostic sante VPS : restart, curl interne, logs |
| `scripts/debug_logs.sh` | `./debug_logs.sh <VPS_IP>` | Recupere les 100 premieres lignes de logs du noeud VPS |
| `scripts/check_caddy_logs.sh` | `./check_caddy_logs.sh <VPS_IP> [USER]` | Recupere les 100 dernieres lignes de logs Caddy sur le VPS |

### Script utilitaire

| Script | Usage | Description |
|---|---|---|
| `scripts/generate_treasury_wallets.sh` | `./generate_treasury_wallets.sh [num] [output_dir] [json_path]` | Genere N wallets treasury avec adresses bech32m reelles via `tools-cli` |

## Workflow de Deploiement

### Production (`deploy.sh`)

Le script `deploy.sh` est un deploiement **interactif** en 6 phases :

1. **Configuration** : saisie/generation du token admin (64 chars hex via `openssl rand -hex 32`)
2. **Selection des actions** : git pull, build, clean reset, init coordinateur
3. **Phase Git** (SSH) : `git pull` sur `/opt/pms` (le repo est clone sur le VPS)
4. **Phase Deploy** (SSH) :
   - Generation des secrets si absents (`node.key`, `api-keys.json`, TLS, Prometheus config)
   - Generation dynamique du `Caddyfile.prod` (Let's Encrypt si domaine, `tls internal` si IP)
   - Pre-flight check : verification de 11 fichiers critiques
   - Build Docker sur le VPS (`docker compose build pms-engine && build pms-gateway`)
   - Init coordinateur optionnel via `tools-cli gen-coordinator` + `treasury-generate` + `treasury-sign`
   - `docker compose up -d --force-recreate`
   - Health check (30s timeout pour Engine et Gateway)
   - Creation automatique d'une cle API SDK via `POST /admin/api-keys`
5. **Backup securise** : SCP des cles (coordinator.json, coordinator.key, treasury-wallets.json, sdk-api-key.json) vers la machine locale, export JSON avec dialog macOS natif (`osascript`)
6. **Verification connectivite** : `curl -sk https://DOMAIN/livez`

### Testnet (`deploy-testnet.sh`)

Le deploiement testnet differe en utilisant le **build local cross-compile** :

1. Token admin + selection des actions
2. **Build local** (`docker buildx --platform linux/amd64`) : Engine, Gateway, Simulator
3. Compression (`gzip`) et transfert SCP vers le VPS
4. **Remote** : `docker load`, setup secrets, Caddyfile.testnet, coordinator init, start services
5. Creation cle API SDK + restart simulator avec les credentials
6. Backup securise + verification connectivite + data integrity check (block_count, utxo_count)

### Upgrade Testnet (`upgrade-testnet.sh`)

Upgrade **non destructif** qui preserve :
- Donnees RocksDB (blockchain state, UTXO set)
- Wallets coordinateur et treasury
- Cles API, certificats TLS, identite noeud
- Donnees Prometheus et certificats Caddy

Le script :
1. Selectionne les composants a upgrader (Engine, Gateway, Simulator, config)
2. Build local cross-compile des images selectionnees
3. Transfert config avec preservation des valeurs VPS-specifiques (`coordinator_public_key`, `wallet_addresses`, `treasury_addresses`, `signer_pubkeys`)
4. Recupere le token admin depuis le conteneur en cours (`docker inspect`)
5. `docker compose up -d --force-recreate` uniquement sur les services selectionnes
6. Health checks + data integrity check

## Architecture TLS

### Schema a deux niveaux

```
Client (Internet)
   |
   | HTTPS (Let's Encrypt, certificat valide)
   v
Caddy (port 443)
   |
   | HTTPS (auto-signe, tls_insecure_skip_verify)
   v
Gateway (port 8443)
   |
   | HTTPS (auto-signe, SANs: pms-engine, pms-gateway, localhost, 127.0.0.1)
   v
Engine (port 8080)
```

### Certificats auto-signes internes

Generes automatiquement par les scripts de deploiement :

```bash
openssl req -x509 -newkey rsa:4096 -keyout secrets/tls/key.pem \
    -out secrets/tls/cert.pem -days 365 -nodes \
    -subj "/CN=pms-node" \
    -addext "subjectAltName=IP:127.0.0.1,DNS:localhost,DNS:pms-engine,DNS:pms-gateway"
```

Les SANs incluent les noms DNS Docker internes (`pms-engine`, `pms-gateway`) pour permettre la verification TLS entre conteneurs.

### Certificats Let's Encrypt (Caddy)

Caddy gere automatiquement l'obtention et le renouvellement des certificats Let's Encrypt :

- **Production** : domaine `pms-network.com` + `www.pms-network.com`
- **Testnet** : domaine `testnet.pms-network.com`

Le `Caddyfile.prod` est genere dynamiquement par `deploy.sh` :
- Si le domaine est une IP : `tls internal` (certificat auto-signe)
- Si le domaine est un nom : `tls admin@DOMAIN` (Let's Encrypt automatique)

### Fichiers TLS

| Fichier | Emplacement | Description |
|---|---|---|
| `secrets/tls/cert.pem` | VPS `/opt/pms/secrets/tls/` | Certificat auto-signe (Engine + Gateway) |
| `secrets/tls/key.pem` | VPS `/opt/pms/secrets/tls/` | Cle privee auto-signee |
| `Caddyfile.prod` | VPS `/opt/pms/` | Configuration Caddy (genere par deploy.sh) |
| `Caddyfile.testnet` | VPS `/opt/pms/` | Configuration Caddy testnet (genere par deploy-testnet.sh) |
| Volume `caddy_data` | Docker | Certificats Let's Encrypt persistes |

## Configuration Production

### Arborescence VPS (`/opt/pms`)

```
/opt/pms/
  +-- docker-compose.yml                 # Compose production
  +-- docker-compose.testnet.yml         # Compose testnet
  +-- Dockerfile                         # Image Engine
  +-- Dockerfile.gateway                 # Image Gateway
  +-- Caddyfile.prod                     # Config Caddy (genere)
  +-- etc/
  |   +-- config/
  |   |   +-- config.prod.toml           # Config Engine production
  |   |   +-- config.prod.template.toml  # Template avec placeholder token
  |   |   +-- config.testnet.toml        # Config Engine testnet
  |   +-- pms/
  |   |   +-- node.key                   # Identite noeud (32 bytes hex)
  |   |   +-- coordinator.key            # Cle privee coordinateur (hex)
  |   |   +-- coordinator.json           # Wallet coordinateur (JSON complet)
  |   |   +-- treasury-wallets.json      # Wallets treasury signes
  |   |   +-- treasury-keys/             # Cles privees treasury
  |   |   +-- api-keys.json              # Store cles API SDK
  |   +-- prometheus/
  |       +-- prometheus.yml             # Config Prometheus
  +-- secrets/
  |   +-- tls/
  |       +-- cert.pem                   # Certificat auto-signe
  |       +-- key.pem                    # Cle privee
  +-- pms-dashboard/                     # Sources dashboard React
```

### Fichiers de configuration TOML

| Fichier | Environnement | Description |
|---|---|---|
| `etc/config/config.prod.toml` | Production | Config complete avec token admin, coordinator keys |
| `etc/config/config.prod.template.toml` | Production | Template avec `REPLACE_WITH_YOUR_SECRET_TOKEN` |
| `etc/config/config.testnet.toml` | Testnet | Config testnet (rate limits plus eleves, HTTPS simule prod) |
| `etc/config/config.docker-test.toml` | Test Docker | Config pour tests locaux |
| `etc/config/config.e2e-prod.toml` | E2E | Config simulation E2E production |
| `etc/config/config.vps-test.toml` | VPS Test | Config architecture VPS multi-processus |
| `etc/config/config.dev.toml` | Developpement | Config developpement local |
| `etc/config/config.local.toml` | Local | Config locale minimale |

### Variables d'environnement

| Variable | Service | Description |
|---|---|---|
| `PMS_ADMIN_TOKEN` | Engine, Gateway | Token d'authentification admin (requis) |
| `PMS_CONFIG` | Engine | Chemin vers le fichier config TOML |
| `RUST_LOG` | Tous | Niveau de log (`info`, `debug`, etc.) |
| `UPSTREAM_URL` | Gateway | URL de l'Engine. `https://pms-engine:8080` (HTTPS auto-signe, prod et testnet). Le Gateway accepte les certs auto-signes automatiquement. |
| `LISTEN_ADDR` | Gateway | Adresse d'ecoute Gateway (`0.0.0.0:8443`) |
| `TLS_CERT` / `TLS_KEY` | Gateway | Chemins certificat/cle TLS |
| `DASHBOARD_PATH` | Gateway | Chemin fichiers statiques dashboard |
| `RATE_LIMIT_RPS` / `BURST_SIZE` | Gateway | Rate limiting (RPS et burst) |
| `PMS_API_KEY` | Simulator | Cle API SDK pour le simulateur |
| `PMS_COORDINATOR_KEY` | Simulator | Cle privee coordinateur (pour minting) |
| `PMS_COORDINATOR_ADDR` | Simulator | Adresse wallet coordinateur |

### Prometheus

Configuration du scraping (`etc/prometheus/prometheus.yml`) :

```yaml
global:
  scrape_interval: 15s
  evaluation_interval: 15s

scrape_configs:
  - job_name: 'pms-engine'
    scheme: https
    tls_config:
      insecure_skip_verify: true
    static_configs:
      - targets: ['pms-engine:8080']
    metrics_path: /metrics

  - job_name: 'pms-gateway'
    scheme: https
    tls_config:
      insecure_skip_verify: true
    static_configs:
      - targets: ['pms-gateway:8443']
    metrics_path: /metrics
```

Les deux services sont scrapes en HTTPS avec `insecure_skip_verify: true` (certificats auto-signes internes).

## Volumes Docker

| Volume | Compose | Description |
|---|---|---|
| `rocksdb_data` | Production | Donnees RocksDB (blockchain, UTXO, ledgers, contrats) |
| `rocksdb_testnet_data` | Testnet | Donnees RocksDB testnet |
| `rocksdb_e2e_data` | E2E | Donnees RocksDB E2E (ephemere) |
| `caddy_data` | Production | Certificats Let's Encrypt |
| `caddy_config` | Production | Configuration Caddy persistee |
| `caddy_testnet_data` | Testnet | Certificats Let's Encrypt testnet |
| `caddy_testnet_config` | Testnet | Configuration Caddy testnet persistee |
| `prometheus_data` | Production | Time series Prometheus |
| `prometheus_testnet_data` | Testnet | Time series Prometheus testnet (retention 3j/1Go) |

## Operations Courantes

### Demarrage

```bash
# Production
PMS_ADMIN_TOKEN=xxx docker compose -f docker-compose.yml up -d

# Testnet
PMS_ADMIN_TOKEN=xxx docker compose -f docker-compose.testnet.yml up -d
```

### Arret

```bash
# Arret gracieux (SIGTERM, flush RocksDB)
docker compose stop pms-engine

# Arret complet
docker compose down

# Arret + suppression donnees (DANGER)
docker compose down -v
```

### Logs

```bash
# Logs en direct
docker compose logs -f --tail 100 pms-engine
docker compose logs -f pms-gateway

# Logs Caddy (VPS)
./scripts/check_caddy_logs.sh <VPS_IP>
```

### Metriques

```bash
# Metriques Engine (necessite admin token)
curl -k -H "Authorization: Bearer $PMS_ADMIN_TOKEN" https://127.0.0.1:8080/metrics

# Metriques cles:
# - pms_blocks_total        : nombre total de blocs dans le DAG
# - pms_blocks_persisted_total : blocs ecrits sur disque
```

### Health Checks

```bash
# Via Caddy (production)
curl -sk https://pms-network.com/livez

# Via Gateway (direct)
curl -k https://127.0.0.1:8443/livez

# Depuis l'interieur du conteneur
docker exec pms-engine bash -c 'timeout 2 bash -c "</dev/tcp/127.0.0.1/8080"'
```

### Backup

```bash
# 1. Arreter le noeud (coherence DB)
docker compose stop pms-engine

# 2. Archiver les donnees
tar -czvf "backup_$(date +%Y%m%d).tar.gz" docker_data/

# 3. Redemarrer
docker compose start pms-engine
```

### Upgrade Testnet (sans perte de donnees)

```bash
./scripts/upgrade-testnet.sh <VPS_IP> pms ~/.ssh/pms_vps
```

## Troubleshooting

| Symptome | Cause probable | Resolution |
|---|---|---|
| `401 Unauthorized` sur `/metrics` | Token admin manquant | Ajouter `-H "Authorization: Bearer $PMS_ADMIN_TOKEN"` |
| Noeud bloque au demarrage | Fichier `LOCK` RocksDB | Verifier qu'aucun processus ne tourne, supprimer le `LOCK` |
| `429 Too Many Requests` | Rate limit depasse | Augmenter `RATE_LIMIT_RPS` dans docker-compose et redemarrer |
| Engine ne demarre pas | Fichiers critiques manquants | Verifier le pre-flight check (11 fichiers) |
| Gateway `502 Bad Gateway` | Engine pas encore ready | Attendre le health check (30s max) |
| Certificat Let's Encrypt echoue | DNS non propage ou ports 80/443 bloques | Verifier DNS + firewall |

## Fichiers Cles

| Fichier | Chemin absolu |
|---|---|
| Compose Production | `docker-compose.yml` |
| Compose Testnet | `docker-compose.testnet.yml` |
| Compose VPS | `docker-compose.vps.yml` |
| Compose E2E | `docker-compose.e2e-prod.yml` |
| Compose Benchmark | `docker-compose.bench.yml` |
| Compose Simulator | `docker-compose.simulator.yml` |
| Dockerfile Engine | `Dockerfile` |
| Dockerfile Gateway | `Dockerfile.gateway` |
| Dockerfile Testnet | `Dockerfile.testnet` |
| Dockerfile Simulator | `Dockerfile.simulator` |
| Caddyfile Production | `Caddyfile.prod` |
| Deploy Production | `scripts/deploy.sh` |
| Deploy Testnet | `scripts/deploy-testnet.sh` |
| Upgrade Testnet | `scripts/upgrade-testnet.sh` |
| Setup Production | `scripts/setup_production.sh` |
| Docker Test | `scripts/docker_test.sh` |
| Verify VPS | `scripts/verify_vps.sh` |
| Benchmark | `scripts/benchmark_local.sh` |
| E2E Prod | `scripts/run_e2e_prod.sh` |
| Crash Recovery | `scripts/verify_crash_recovery.sh` |
| Setup Cluster | `scripts/setup_test_cluster.sh` |
| Gen Treasury | `scripts/generate_treasury_wallets.sh` |
| Debug Health | `scripts/debug_check_health.sh` |
| Debug Logs | `scripts/debug_logs.sh` |
| Check Caddy | `scripts/check_caddy_logs.sh` |
| Runbook Ops | `runbook_ops.md` |
| Prometheus Config | `etc/prometheus/prometheus.yml` |

## Interactions

- [[gateway]] : Le Gateway est le seul point d'entree public. Le deploiement configure `UPSTREAM_URL`, `RATE_LIMIT_RPS`, `ADMIN_TOKEN` et le `DASHBOARD_PATH` pour servir le dashboard React.
- [[server-engine]] : L'Engine (coordinateur) est le coeur du systeme, isole sur le reseau interne Docker. Les scripts d'initialisation utilisent `tools-cli gen-coordinator`, `treasury-generate` et `treasury-sign` pour configurer le coordinateur et les wallets treasury.
- [[metrics-monitoring]] : Prometheus scrape les endpoints `/metrics` de l'Engine et du Gateway toutes les 15 secondes en HTTPS. Les metriques cles incluent `pms_blocks_total`, `pms_blocks_persisted_total`, et les compteurs de transactions.
- [[storage-rocksdb]] : Les donnees persistantes du DAG sont stockees dans un volume Docker (`rocksdb_data`). L'upgrade testnet preserve explicitement ce volume. Les backups necessitent l'arret du noeud pour garantir la coherence.
- [[simulator]] : Le Simulator testnet se connecte au Gateway via le reseau public Docker et utilise une cle API SDK + les credentials coordinateur pour generer du trafic realiste.
- [[config-system]] : Les fichiers de configuration TOML sont montes en lecture seule dans les conteneurs. Les scripts de deploiement preservent les valeurs VPS-specifiques (cles coordinateur, adresses wallet/treasury) lors des mises a jour de config.
- [[api-key-authentication]] : Les scripts de deploiement creent automatiquement une cle API SDK via `POST /admin/api-keys` apres le demarrage de l'Engine, puis la sauvegardent dans le backup securise.
