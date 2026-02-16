# PMS Simulator

Simulateur multi-agents pour le reseau DAG-PMS. Genere du trafic realiste (transactions PMS, burns de cubes NFT, echanges d'Edenite) via des agents autonomes configurables.

## Quick Start

### Local

```bash
cd tools/simulator
cargo run -- --config simulator.dev.toml
```

### Docker

```bash
# Prerequis : engine + gateway deja actifs
docker compose -f docker-compose.simulator.yml up --build -d

# Logs
docker logs -f pms-simulator

# Web dashboard
open http://localhost:9090
```

## CLI

```
pms-simulator [OPTIONS]

Options:
  -c, --config <PATH>    Chemin vers le fichier TOML [default: simulator.dev.toml]
  -h, --help             Aide
```

## Configuration

Le simulateur se configure via un fichier TOML. Les agents peuvent etre definis inline (`[[agents]]`) ou dans des fichiers externes via `agent_files`.

> **Important** : `agent_files` doit etre place **avant** la premiere section `[...]` du TOML (c'est une cle root).

### Exemple minimal

```toml
agent_files = ["agents.toml"]

[server]
url = "https://127.0.0.1:8443"
admin_token = "dev-token"
accept_invalid_certs = true

[simulation]
duration_secs = 300
faucet_amount = "50.00"

[simulation.game]
ledger_id = "eden"
network_id = "eden-net"
symbol = "EDN"
divisor = 19300000000

[tui]
enabled = true

[web]
enabled = true
port = 9090
```

### Reference des champs

#### Root

| Champ | Type | Default | Description |
|-------|------|---------|-------------|
| `agent_files` | `string[]` | `[]` | Fichiers TOML d'agents (chemin relatif au config) |

#### `[server]`

| Champ | Type | Default | Description |
|-------|------|---------|-------------|
| `url` | string | **requis** | URL du gateway (ex: `https://127.0.0.1:8443`) |
| `admin_token` | string? | `null` | Bearer token pour les endpoints admin |
| `ledger_id` | string? | `null` | Prefixe ledger pour multi-ledger (ex: `"main"`) |
| `accept_invalid_certs` | bool | `true` | Accepter les certificats TLS auto-signes |

#### `[gemini]` (optionnel, requis pour les agents `smart`)

| Champ | Type | Default | Description |
|-------|------|---------|-------------|
| `api_key` | string? | `null` | Cle API Gemini (`"env:GEMINI_API_KEY"` pour lire depuis env) |
| `client_id` | string? | `null` | OAuth client ID (mode navigateur) |
| `client_secret` | string? | `null` | OAuth client secret |
| `model` | string | `"gemini-2.0-flash"` | Modele Gemini |

#### `[simulation]`

| Champ | Type | Default | Description |
|-------|------|---------|-------------|
| `duration_secs` | u64 | `0` | Duree en secondes (`0` = infini) |
| `base_tick_ms` | u64 | `1000` | Tick de base |
| `faucet_amount` | string | `"50.00"` | PMS distribues par agent au demarrage |

#### `[simulation.game]` (optionnel, active le game engine Edenite)

| Champ | Type | Default | Description |
|-------|------|---------|-------------|
| `ledger_id` | string | **requis** | ID du ledger de jeu (ex: `"eden"`) |
| `network_id` | string | **requis** | Network ID du ledger de jeu |
| `symbol` | string? | `null` | Symbole du token natif (ex: `"EDN"`) |
| `divisor` | f64? | `19300000000` | Diviseur de la formule de reward |

#### `[tui]`

| Champ | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `true` | Activer le dashboard terminal (ratatui) |
| `refresh_ms` | u64 | `250` | Frequence de rafraichissement (ms) |

#### `[web]`

| Champ | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `false` | Activer le dashboard web (WebSocket) |
| `port` | u16 | `9090` | Port du serveur web |

## Agents

Trois types d'agents sont disponibles :

### Random

Envoie des montants aleatoires a des peers aleatoires. Auto-refuel via faucet quand le solde PMS < 10. Si le game engine est active, execute le game loop a chaque tick.

```toml
[[agents]]
count = 5
interval_ms = 1000
name_prefix = "rnd"

[agents.behavior]
type = "random"
min_amount = 0.1          # Min PMS par envoi
max_amount = 5.0          # Max PMS par envoi
send_probability = 0.5    # Probabilite d'envoi par tick (0.0-1.0)
```

### Smart (IA Gemini)

Agent pilote par Gemini. Appelle l'API Gemini toutes les `ai_interval` ticks pour decider d'une action (envoyer PMS, attendre, observer, envoyer un message).

```toml
[[agents]]
count = 1
interval_ms = 3000
name_prefix = "ai"
ai_interval = 5

[agents.behavior]
type = "smart"
system_prompt = "Tu es un utilisateur de paiement DAG-PMS..."
```

Necessite une section `[gemini]` avec cle API ou OAuth.

### Observer

Agent passif qui monitore le reseau. Alterne entre : tips, supply, balances. Ne fait aucune transaction.

```toml
[[agents]]
count = 1
interval_ms = 5000
name_prefix = "obs"

[agents.behavior]
type = "observer"
```

## Fichiers agents externes

Au lieu de definir les agents inline dans le TOML principal, on peut les externaliser :

```toml
# simulator.toml
agent_files = ["agents_random.toml", "agents_smart.toml"]
```

Chaque fichier contient des sections `[[agents]]` :

```toml
# agents_random.toml

[[agents]]
count = 10
interval_ms = 500
name_prefix = "fast"

[agents.behavior]
type = "random"
min_amount = 0.01
max_amount = 1.0
send_probability = 0.8

[agents.game]
enabled = true
cubes_per_agent = 10
cubes_per_remint = 5
edn_send_min_pct = 5.0
edn_send_max_pct = 30.0

[[agents]]
count = 3
interval_ms = 5000
name_prefix = "slow"

[agents.behavior]
type = "random"
min_amount = 1.0
max_amount = 20.0
send_probability = 0.3

[agents.game]
enabled = true
cubes_per_agent = 3
cubes_per_remint = 2
edn_send_min_pct = 20.0
edn_send_max_pct = 60.0
```

### `[agents.game]` — Configuration game par groupe

| Champ | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `true` | Activer le game loop pour ce groupe |
| `cubes_per_agent` | usize | `5` | Cubes a mint par agent au demarrage |
| `cubes_per_remint` | usize | `3` | Cubes a re-mint quand epuises |
| `edn_send_min_pct` | f64 | `10.0` | % min du solde EDN a envoyer |
| `edn_send_max_pct` | f64 | `50.0` | % max du solde EDN a envoyer |

Les agents inline et ceux des fichiers externes sont fusionnes (inline en premier).

## Game Engine (Edenite)

Le game engine gere un ledger secondaire ("eden") avec un token custom ("edenite" / EDN).

### Mecanisme

1. **Mint** : Au demarrage, le funder mint `cubes_per_agent` cubes NFT par agent sur le ledger principal
2. **Burn** : L'agent batch-burn tous ses cubes en un seul appel API
3. **Reward** : Pour chaque cube brule, l'edenite est calculee et mintee sur le ledger eden
4. **Send** : L'agent envoie un % aleatoire de son solde EDN a un peer
5. **Re-mint** : Quand l'agent n'a plus de cubes ni d'EDN, il re-mint des cubes

### Attributs d'un cube

Chaque cube a des attributs aleatoires :

| Attribut | Range |
|----------|-------|
| `weight` | 100.0 - 1000.0 |
| `size` | 100.0 - 500.0 |
| `density` | 1.0 - 20.0 |

### Formule de reward

```
Edenite = (weight * size * density) / divisor
divisor = 19,300,000,000 (configurable)
```

### Game loop (chaque tick)

```
si cubes disponibles :
    batch burn tous les cubes -> gagner EDN
sinon si solde EDN >= 0.00000001 :
    envoyer min_pct..max_pct % de l'EDN a un peer aleatoire
sinon :
    re-mint cubes_per_remint cubes
```

## Docker

### Fichiers

- `Dockerfile.simulator` : Build multi-stage (rust:1-slim -> debian:trixie-slim)
- `docker-compose.simulator.yml` : Service pms-simulator

### Volumes montes

| Local | Container | Description |
|-------|-----------|-------------|
| `tools/simulator/simulator.docker.toml` | `/app/config/simulator.toml` | Config principale |
| `tools/simulator/agents_docker.toml` | `/app/config/agents_docker.toml` | Definitions d'agents |
| Token cache Gemini | `/root/.cache/pms-simulator` | Cache OAuth (si smart agents) |

### Reseau

Le container rejoint le reseau Docker externe `dag-pms_pms-public` (ou `$PMS_NETWORK`). L'engine et le gateway doivent deja tourner.

```bash
# Lancer avec un reseau custom
PMS_NETWORK=my-network docker compose -f docker-compose.simulator.yml up --build -d

# Port custom pour le dashboard web
SIMULATOR_PORT=3000 docker compose -f docker-compose.simulator.yml up --build -d
```

## Architecture

### Sequence de demarrage

```
1. Charger config TOML + fichiers agents externes
2. Resoudre les secrets (env:...)
3. Creer le client HTTP + health check gateway
4. Creer le client Gemini (si agents smart)
5. Demarrer le pipeline de metriques
6. Demarrer le CommsRouter + dashboard web
7. Creer les wallets agents via /v1/wallet/create
8. Setup game engine (creer ledger eden + token edenite)
9. Funder : faucet PMS + mint cubes par agent
10. Spawner les agents (1 tokio task par agent, cadence = interval_ms)
11. Lancer le TUI ou mode headless
12. Arret : Ctrl+C -> CancellationToken -> await all handles
```

### Metriques

Le pipeline de metriques collecte :
- **TPS** : Transactions par seconde (fenetre glissante 10s)
- **Latence** : p50, p95, p99
- **Par agent** : tx_count, errors, balance
- **DAG** : tips, supply, UTXO count

Affiche dans le TUI (ratatui) et/ou le dashboard web (WebSocket sur port 9090).

### Retry HTTP

Sur les reponses 429 (Too Many Requests), le client fait un backoff exponentiel :

| Tentative | Delai |
|-----------|-------|
| 1 | 500ms |
| 2 | 1s |
| 3 | 2s |
| 4 | 4s |
| 5 | 8s |

Apres 5 tentatives, l'erreur est propagee.

### Endpoints appeles

| Methode | Endpoint | Usage |
|---------|----------|-------|
| GET | `/livez` | Health check |
| POST | `/v1/wallet/create` | Creer wallet |
| POST | `/v1/wallet/send-simple` | Envoyer PMS |
| POST | `/v1/balance` | Solde |
| POST | `/v1/dag/tips` | Tips DAG |
| GET | `/v1/supply` | Supply circulante |
| POST | `/admin/faucet` | Faucet PMS |
| POST | `/admin/ledgers/create` | Creer ledger |
| POST | `/admin/tokens/create` | Creer token (prefixe ledger) |
| POST | `/admin/tokens/mint` | Mint token (prefixe ledger) |
| POST | `/v1/nft/mint` | Mint NFT |
| POST | `/v1/nft/burn-simple` | Burn 1 NFT |
| POST | `/v1/nft/burn-batch-simple` | Burn N NFTs en un bloc |
