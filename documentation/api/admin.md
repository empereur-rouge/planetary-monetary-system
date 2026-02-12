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

## 🔑 Rotation des clés Authority

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
