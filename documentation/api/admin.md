# 🔧 Admin API

> Endpoints protégés pour l'administration du serveur

---

## 🔐 Authentification

Tous les endpoints `/admin/*` et `/metrics` nécessitent une authentification :

### Méthode 1 : Localhost
Les requêtes depuis `127.0.0.1` ou `::1` sont automatiquement autorisées.

### Méthode 2 : IP Allowlist
Si configuré dans `settings.toml`, seules les IPs dans la liste sont autorisées :

```toml
[auth]
allowed_ips = ["192.168.1.0/24", "10.0.0.5"]
```

### Méthode 3 : Bearer Token

```http
Authorization: Bearer votre_token_admin
```

Configuration dans `settings.toml` :
```toml
[auth]
admin_api_token = "env:ADMIN_TOKEN"  # Lit depuis $ADMIN_TOKEN
# ou
admin_api_token = "token_direct"
```

---

## GET `/admin/ping`

Test de connectivité admin.

### Response

```json
{
  "status": "pong",
  "timestamp": 1706000000000
}
```

### Exemple

```bash
curl -k -H "Authorization: Bearer $TOKEN" https://localhost:8443/admin/ping
```

---

## POST `/admin/compact`

Déclenche une compaction de la base de données RocksDB.

> ⚠️ **Attention**: Cette opération peut être lente et impacter les performances temporairement.

### Response

```json
{
  "status": "ok",
  "duration_ms": 1234,
  "space_reclaimed_mb": 56
}
```

### Exemple

```bash
curl -k -X POST -H "Authorization: Bearer $TOKEN" https://localhost:8443/admin/compact
```

---

## POST `/admin/distribute_fees`

Déclenche manuellement une distribution des frais accumulés.

> 💡 Normalement, la distribution est automatique lors des Milestones.

### Request Body (optionnel)

```json
{
  "force": true
}
```

| Champ | Type | Description |
|-------|------|-------------|
| `force` | boolean | Forcer la distribution même si le seuil n'est pas atteint |

### Response

```json
{
  "status": "ok",
  "distributed_amount": "123.45678900",
  "recipients": [
    {
      "address": "pms1treasury...",
      "amount": "61.72839450",
      "type": "treasury"
    },
    {
      "address": "pms1validator...",
      "amount": "61.72839450",
      "type": "validator"
    }
  ],
  "block_id": "distribution_block_123"
}
```

### Exemple

```bash
curl -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"force": true}' \
  https://localhost:8443/admin/distribute_fees
```

---

## GET `/admin/config`

Récupère la configuration runtime actuelle du serveur.

### Response

```json
{
  "fee_rate_bps": 300,
  "base_fee": "0.001",
  "coordinator_fee_bps": 6700,
  "treasury_fee_bps": 3300,
  "min_pow_bits": 8,
  "max_mint_per_block": 1000000,
  "mint_enabled": true,
  "updated_at_block": "admin-1707350000000",
  "updated_at_timestamp": 1707350000000
}
```

| Champ | Type | Description |
|-------|------|-------------|
| `fee_rate_bps` | u32 | Taux de commission (basis points, 100 = 1%) |
| `base_fee` | string | Frais fixes par transaction |
| `coordinator_fee_bps` | u32 | Part des fees pour le Coordinator (basis points, 6700 = 67%) |
| `treasury_fee_bps` | u32 | Part des fees pour le Treasury (basis points, 3300 = 33%) |
| `min_pow_bits` | u8 | Difficulté PoW minimum |
| `max_mint_per_block` | u64 | Maximum de tokens mintables par bloc |
| `mint_enabled` | bool | Minting activé ou non |
| `updated_at_block` | string | ID du bloc/action de dernière mise à jour |
| `updated_at_timestamp` | i64 | Timestamp de dernière mise à jour (ms) |

### Exemple

```bash
curl -k -H "Authorization: Bearer $TOKEN" https://localhost:8443/admin/config
```

---

## POST `/admin/config`

Modifie un ou plusieurs paramètres de la configuration runtime.

> 💡 Les changements sont persistés dans RocksDB et appliqués immédiatement (Hot-Swap).

### Request Body

Un `ConfigUpdate` JSON. Plusieurs types disponibles :

| Type | Format | Description |
|------|--------|-------------|
| `SetFeeRate` | `{"SetFeeRate": {"bps": 300}}` | Modifier le taux de commission |
| `SetBaseFee` | `{"SetBaseFee": {"fee": "0.002"}}` | Modifier les frais fixes |
| `SetCoordinatorFee` | `{"SetCoordinatorFee": {"bps": 7000}}` | Modifier la part Coordinator (67%→70%) |
| `SetTreasuryFee` | `{"SetTreasuryFee": {"bps": 3000}}` | Modifier la part Treasury (33%→30%) |
| `SetMinPow` | `{"SetMinPow": {"bits": 10}}` | Modifier la difficulté PoW |
| `SetMaxMint` | `{"SetMaxMint": {"amount": 500000}}` | Modifier le max mint par bloc |
| `SetMintEnabled` | `{"SetMintEnabled": {"enabled": false}}` | Activer/désactiver le minting |
| `BatchUpdate` | `{"BatchUpdate": [...]}` | Appliquer plusieurs updates |

> ⚠️ **Important**: La somme de `coordinator_fee_bps` + `treasury_fee_bps` doit toujours égaler 10000 (100%).

### Response

```json
{
  "status": "ok",
  "update_applied": "SetCoordinatorFee(7000bps)",
  "config": {
    "fee_rate_bps": 300,
    "coordinator_fee_bps": 7000,
    "treasury_fee_bps": 3000,
    "..."
  }
}
```

### Exemples

**Modifier la part du Coordinator :**
```bash
curl -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"SetCoordinatorFee": {"bps": 7000}}' \
  https://localhost:8443/admin/config
```

**Modifier plusieurs paramètres (BatchUpdate) :**
```bash
curl -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"BatchUpdate": [{"SetCoordinatorFee": {"bps": 7000}}, {"SetTreasuryFee": {"bps": 3000}}]}' \
  https://localhost:8443/admin/config
```

---

## GET `/metrics`


Expose les métriques au format Prometheus.

### Response

```prometheus
# HELP pms_blocks_total Total number of blocks in the DAG
# TYPE pms_blocks_total counter
pms_blocks_total 123456

# HELP pms_transactions_total Total transactions processed
# TYPE pms_transactions_total counter
pms_transactions_total 98765

# HELP pms_fee_pool_amount Current fee pool balance
# TYPE pms_fee_pool_amount gauge
pms_fee_pool_amount 123.45678

# HELP pms_connected_nodes Number of connected nodes
# TYPE pms_connected_nodes gauge
pms_connected_nodes 5
```

### Intégration Prometheus

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'pms-server'
    bearer_token: 'votre_token_admin'
    static_configs:
      - targets: ['localhost:8443']
    metrics_path: '/metrics'
```

### Exemple

```bash
curl -k -H "Authorization: Bearer $TOKEN" https://localhost:8443/metrics
```

---

## POST `/admin/faucet`

Mint du PMS natif vers une adresse. Disponible uniquement en mode dev/testnet. Bloque en production.

### Request Body

```json
{
  "to": "pms1recipient...",
  "amount": "1000.0"
}
```

| Champ | Type | Requis | Description |
|-------|------|--------|-------------|
| `to` | string | oui | Adresse Bech32 du destinataire |
| `amount` | string | oui | Montant a minter (decimal positif) |

### Response (Succes - 201)

```json
{
  "block_id": "faucet_abc123...",
  "amount": "1000.0"
}
```

### Erreurs

| HTTP | Description |
|------|-------------|
| 400 | Montant invalide |
| 403 | Faucet desactive sur mainnet |

### Exemple

```bash
curl -k -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"to": "pms1recipient...", "amount": "1000.0"}' \
  https://localhost:8443/admin/faucet
```

---

## POST `/admin/api-keys`

Crée une nouvelle clé API pour un client SDK. La clé secrète est retournée **une seule fois** — elle ne peut pas être récupérée ensuite.

### Request Body

```json
{
  "label": "Clicker Game Prod",
  "scopes": ["wallet", "nft"]
}
```

| Champ | Type | Requis | Description |
|-------|------|--------|-------------|
| `label` | string | oui | Nom descriptif de la clé |
| `scopes` | string[] | non | Permissions (défaut: `["*"]`). Voir scopes ci-dessous |

**Scopes disponibles :**

| Scope | Endpoints couverts |
|-------|--------------------|
| `*` | Accès total à tous les endpoints publics |
| `wallet` | `/wallet/*`, `/v1/balance`, `/v1/tx/*`, `/v1/wallet/*` |
| `nft` | `/v1/nft/*`, `/v1/wallet/{addr}/nfts`, `/v1/wallet/{addr}/utxos` |
| `dag` | `/v1/dag/*`, `/v1/blocks/*`, `/v1/config`, `/submit/*`, `/blocks/*` |
| `supply` | `/v1/supply`, `/v1/fee_pool` |
| `tokens` | `/v1/tokens`, `/v1/tokens/{id}` |
| `history` | `/v1/history/*`, `/wallet/history` |
| `coordinator` | `/v1/coordinator/*` |

> 💡 On peut aussi passer un **path exact** comme scope (ex: `"/v1/nft/mint"`).

### Response (201)

```json
{
  "id": "key_01",
  "key": "pk_a1b2c3d4e5f6...",
  "label": "Clicker Game Prod",
  "scopes": ["wallet", "nft"],
  "created_at": "2026-02-20T19:00:00Z"
}
```

> ⚠️ **IMPORTANT** : Le champ `key` n'est retourné qu'à la création. Notez-le immédiatement.

### Exemple

```bash
curl -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"label": "Clicker Game", "scopes": ["wallet", "nft"]}' \
  http://localhost:7400/admin/api-keys
```

---

## GET `/admin/api-keys`

Liste toutes les clés API enregistrées (sans les hashes ni les secrets).

### Response

```json
{
  "keys": [
    {
      "id": "key_01",
      "label": "Clicker Game Prod",
      "scopes": ["wallet", "nft"],
      "active": true,
      "created_at": "2026-02-20T19:00:00Z"
    },
    {
      "id": "key_02",
      "label": "Dashboard",
      "scopes": ["*"],
      "active": false,
      "created_at": "2026-02-19T10:00:00Z"
    }
  ]
}
```

### Exemple

```bash
curl -H "Authorization: Bearer $TOKEN" http://localhost:7400/admin/api-keys
```

---

## DELETE `/admin/api-keys/{key_id}`

Révoque une clé API (soft-delete). La clé reste dans le fichier mais retournera 403 au middleware.

### Paramètres

| Paramètre | Type | Description |
|-----------|------|-------------|
| `key_id` | string (path) | ID de la clé (ex: `key_01`) |

### Response (200)

```json
{
  "status": "revoked",
  "id": "key_01"
}
```

### Erreurs

| HTTP | Description |
|------|-------------|
| 404 | Clé non trouvée |
| 404 | Clé déjà révoquée |

### Exemple

```bash
curl -X DELETE -H "Authorization: Bearer $TOKEN" \
  http://localhost:7400/admin/api-keys/key_01
```

---

## 🔑 Configuration des clés API

Les clés API protègent les routes publiques (`/v1/*`, `/wallet/*`, `/submit/*`, etc.).

```toml
[auth]
# Chemin vers le fichier JSON des clés API SDK.
# Si absent → pas de vérification (mode dev, backward-compatible).
api_keys_file = "etc/pms/api-keys.json"
```

Côté client SDK, la clé se passe via le header `X-API-Key` :

```http
X-API-Key: pk_a1b2c3d4e5f6...
```

> 💡 Si aucune clé n'est configurée (`api_keys_file` absent ou fichier vide), le middleware laisse tout passer (mode dev).

---

## Autres modules Admin

Les endpoints admin sont organises en modules dedies avec leur propre documentation :

| Module | Endpoints | Documentation |
|--------|-----------|---------------|
| **API Keys** | `/admin/api-keys` (POST, GET, DELETE) | ↑ voir ci-dessus |
| **Tokens** | `/admin/tokens/create`, `/admin/tokens/mint` | [tokens.md](./tokens.md) |
| **Ledgers** | `/admin/ledgers`, `/admin/ledgers/create`, `/admin/ledgers/{id}` | [ledgers.md](./ledgers.md) |
| **Bridge** | `/admin/bridge/enable`, `/admin/bridge/disable`, `/admin/bridge/transfer` | [bridge.md](./bridge.md) |
| **Compliance** | `/admin/compliance/freeze`, `unfreeze`, `seize`, `reverse`, `frozen`, `log`, `shadow_balance` | [compliance.md](./compliance.md) |

---

## Rotation des cles Authority

Le serveur vérifie automatiquement l'âge des clés Authority au démarrage :

```
🔑 Authority keys rotation OK: 45 days since last rotation (2025-12-01)
```

ou un avertissement si > 90 jours :

```
🔑 SECURITY: Authority keys haven't been rotated in 95 days!
```

Configuration dans `settings.toml` :
```toml
[fees]
authority_public_keys = ["04abc...", "04def..."]
authority_keys_last_rotation = "2025-12-01"
```
