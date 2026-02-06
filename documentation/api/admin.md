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
