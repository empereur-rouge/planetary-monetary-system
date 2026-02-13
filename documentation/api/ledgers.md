# Ledgers API

> Endpoints pour la gestion multi-ledger

Le systeme multi-ledger permet de gerer N instances de DAG independantes au sein d'un meme serveur, avec une RocksDB partagee et des column families isolees par prefixe.

Chaque ledger dispose de ses propres routes via le prefixe `/l/{ledger_id}/...`. Les routes sans prefixe pointent vers le ledger par defaut ("main").

---

## GET `/v1/ledgers`

Liste tous les ledgers actifs. Endpoint public.

### Response

```json
{
  "ledgers": [
    {
      "id": "main",
      "network_id": "mainnet",
      "prefix": "",
      "protocol_version": 1,
      "block_count": 12345
    },
    {
      "id": "gaming",
      "network_id": "gaming-net",
      "prefix": "gam_",
      "protocol_version": 1,
      "block_count": 678
    }
  ]
}
```

### Exemple

```bash
curl -k https://localhost:8443/v1/ledgers
```

---

## GET `/admin/ledgers`

Liste detaillee des ledgers avec informations techniques. Necessite les droits admin.

### Response

```json
{
  "ledgers": [
    {
      "id": "main",
      "network_id": "mainnet",
      "prefix": "",
      "protocol_version": 1,
      "tip_limit": null,
      "block_count": 12345,
      "utxo_shards": 256
    }
  ],
  "count": 1
}
```

### Exemple

```bash
curl -k -H "Authorization: Bearer $TOKEN" \
  https://localhost:8443/admin/ledgers
```

---

## GET `/admin/ledgers/{ledger_id}`

Detail d'un ledger specifique. Necessite les droits admin.

### Path Parameters

| Parametre | Type | Description |
|-----------|------|-------------|
| `ledger_id` | string | ID du ledger |

### Response (Succes)

```json
{
  "id": "gaming",
  "network_id": "gaming-net",
  "prefix": "gam_",
  "protocol_version": 1,
  "tip_limit": 64,
  "block_count": 678,
  "utxo_shards": 256
}
```

### Erreurs

| HTTP | Description |
|------|-------------|
| 401 | Non autorise |
| 404 | Ledger non trouve ou multi-ledger non active |

### Exemple

```bash
curl -k -H "Authorization: Bearer $TOKEN" \
  https://localhost:8443/admin/ledgers/gaming
```

---

## POST `/admin/ledgers/create`

Cree un nouveau ledger. Le ledger est accessible immediatement via `/l/{id}/...` sans redemarrage.

> **Note** : Les column families RocksDB correspondant au prefixe doivent deja exister (configurees dans le TOML). Si ce n'est pas le cas, il faut ajouter le ledger dans la config et redemarrer.

### Request Body

```json
{
  "id": "gaming",
  "network_id": "gaming-net",
  "prefix": "gam_",
  "protocol_version": 1,
  "tip_limit": 64
}
```

| Champ | Type | Requis | Description |
|-------|------|--------|-------------|
| `id` | string | oui | Identifiant unique du ledger |
| `network_id` | string | oui | ID reseau pour le P2P |
| `prefix` | string | oui | Prefixe des column families RocksDB |
| `protocol_version` | u32 | non | Version du protocole (defaut: 1) |
| `tip_limit` | usize | non | Limite de tips par ledger |

### Response (Succes - 201)

```json
{
  "status": "ok",
  "ledger": {
    "id": "gaming",
    "network_id": "gaming-net",
    "prefix": "gam_",
    "protocol_version": 1,
    "block_count": 0
  },
  "message": "Ledger created. API routes available at /l/{id}/..., P2P routing active immediately."
}
```

### Erreurs

| HTTP | Description |
|------|-------------|
| 400 | id, prefix, ou network_id vide |
| 401 | Non autorise |
| 409 | Ledger avec cet ID existe deja |
| 422 | Column families manquantes (ajout dans config + restart requis) |
| 503 | Multi-ledger non active |

### Exemple

```bash
curl -k -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "id": "gaming",
    "network_id": "gaming-net",
    "prefix": "gam_",
    "protocol_version": 1
  }' \
  https://localhost:8443/admin/ledgers/create
```

---

## Routage dynamique

Une fois un ledger cree, toutes les routes ledger-scoped sont disponibles sous `/l/{ledger_id}/` :

```bash
# Balance sur le ledger "gaming"
curl -k -X POST https://localhost:8443/l/gaming/v1/balance \
  -H "Content-Type: application/json" \
  -d '{"address": "pms1..."}'

# Tips du DAG du ledger "gaming"
curl -k -X POST https://localhost:8443/l/gaming/v1/dag/tips \
  -H "Content-Type: application/json" \
  -d '{}'

# Supply du ledger "gaming"
curl -k https://localhost:8443/l/gaming/v1/supply
```

Les routes sans prefixe `/l/` pointent vers le ledger par defaut ("main").
