# Bridge API

> Endpoints pour le pont cross-ledger (transferts entre ledgers)

Le bridge permet de transferer des fonds entre deux ledgers distincts via un mecanisme lock-and-mint. Il necessite que le multi-ledger soit actif (`LedgerManager`).

---

## POST `/admin/bridge/enable`

Active un pont entre deux ledgers. Necessite les droits admin.

### Request Body

```json
{
  "ledger_a": "main",
  "ledger_b": "gaming",
  "direction": "Bidirectional"
}
```

| Champ | Type | Requis | Description |
|-------|------|--------|-------------|
| `ledger_a` | string | oui | Premier ledger |
| `ledger_b` | string | oui | Second ledger |
| `direction` | string | non | `"Bidirectional"` (defaut), `"AtoB"`, ou `"BtoA"` |

### Response (Succes - 201)

```json
{
  "ledger_a": "gaming",
  "ledger_b": "main",
  "direction": "Bidirectional",
  "enabled": true,
  "created_at": 1706000000000,
  "disabled_at": null,
  "authorized_by": ["04abc123..."]
}
```

### Erreurs

| HTTP | Description |
|------|-------------|
| 400 | Ledger invalide ou pont deja actif |
| 401 | Non autorise |
| 503 | Multi-ledger non active |

### Exemple

```bash
curl -k -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"ledger_a": "main", "ledger_b": "gaming"}' \
  https://localhost:8443/admin/bridge/enable
```

---

## POST `/admin/bridge/disable`

Desactive un pont entre deux ledgers. Necessite les droits admin.

### Request Body

```json
{
  "ledger_a": "main",
  "ledger_b": "gaming"
}
```

| Champ | Type | Requis | Description |
|-------|------|--------|-------------|
| `ledger_a` | string | oui | Premier ledger |
| `ledger_b` | string | oui | Second ledger |

### Response (Succes - 200)

```json
{
  "ledger_a": "gaming",
  "ledger_b": "main",
  "direction": "Bidirectional",
  "enabled": false,
  "created_at": 1706000000000,
  "disabled_at": 1706001000000,
  "authorized_by": ["04abc123..."]
}
```

### Exemple

```bash
curl -k -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"ledger_a": "main", "ledger_b": "gaming"}' \
  https://localhost:8443/admin/bridge/disable
```

---

## POST `/admin/bridge/transfer`

Execute un transfert cross-ledger. Le mecanisme est lock-and-mint : les fonds sont verrouilles sur le ledger source et mintes sur le ledger destination.

### Request Body

```json
{
  "from_ledger": "main",
  "to_ledger": "gaming",
  "from_address": "pms1sender...",
  "to_address": "pms1recipient...",
  "amount": "100.0",
  "asset_id": null
}
```

| Champ | Type | Requis | Description |
|-------|------|--------|-------------|
| `from_ledger` | string | oui | Ledger source |
| `to_ledger` | string | oui | Ledger destination |
| `from_address` | string | oui | Adresse source (fonds a verrouiller) |
| `to_address` | string | oui | Adresse destination (fonds mintes) |
| `amount` | string | oui | Montant a transferer |
| `asset_id` | string | non | ID du token (null = PMS natif) |

### Response (Succes - 201)

```json
{
  "lock_block_id": "lock_abc123...",
  "mint_block_id": "mint_def456...",
  "from_ledger": "main",
  "to_ledger": "gaming",
  "amount": "100.0",
  "asset_id": null
}
```

### Erreurs

| HTTP | Description |
|------|-------------|
| 400 | Pont inexistant, desactive, ou direction non autorisee |
| 401 | Non autorise |
| 503 | Multi-ledger non active |

### Exemple

```bash
curl -k -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "from_ledger": "main",
    "to_ledger": "gaming",
    "from_address": "pms1sender...",
    "to_address": "pms1recipient...",
    "amount": "100.0"
  }' \
  https://localhost:8443/admin/bridge/transfer
```

---

## GET `/v1/bridge/links`

Liste tous les ponts configures (actifs et inactifs). Endpoint public.

### Response

```json
{
  "links": [
    {
      "ledger_a": "gaming",
      "ledger_b": "main",
      "direction": "Bidirectional",
      "enabled": true,
      "created_at": 1706000000000,
      "disabled_at": null,
      "authorized_by": ["04abc123..."]
    }
  ]
}
```

### Exemple

```bash
curl -k https://localhost:8443/v1/bridge/links
```

---

## GET `/v1/bridge/status/{lock_block_id}`

Verifie le statut d'un transfert cross-ledger a partir du block ID de verrouillage. Endpoint public.

### Path Parameters

| Parametre | Type | Description |
|-----------|------|-------------|
| `lock_block_id` | string | ID du bloc BridgeLock sur le ledger source |

### Response (Complete)

```json
{
  "lock_block_id": "lock_abc123...",
  "mint_block_id": "mint_def456...",
  "status": "completed"
}
```

### Response (Non trouve)

```json
{
  "lock_block_id": "lock_abc123...",
  "status": "not_found"
}
```

### Exemple

```bash
curl -k https://localhost:8443/v1/bridge/status/lock_abc123...
```
