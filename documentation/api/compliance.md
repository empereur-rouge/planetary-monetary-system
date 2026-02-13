# Compliance API

> Endpoints d'administration pour la conformite reglementaire (gel, saisie, inversion)

Tous les endpoints `/admin/compliance/*` necessitent les droits admin. Chaque action de compliance est enregistree dans le DAG sous forme de bloc signe par le Coordinateur, garantissant un audit trail immutable.

---

## POST `/admin/compliance/freeze`

Gele une adresse. Une adresse gelee ne peut plus envoyer ni recevoir de transactions (bloquee au niveau de `/v1/tx/prepare`).

### Request Body

```json
{
  "address": "pms1suspect...",
  "reason": "AML investigation case #1234"
}
```

| Champ | Type | Requis | Description |
|-------|------|--------|-------------|
| `address` | string | oui | Adresse Bech32 a geler |
| `reason` | string | oui | Motif du gel (audit trail) |

### Response (Succes - 200)

```json
{
  "status": "frozen",
  "block_id": "abc123...",
  "address": "pms1suspect..."
}
```

### Erreurs

| HTTP | Description |
|------|-------------|
| 400 | Adresse vide |
| 401 | Non autorise |
| 409 | Adresse deja gelee |

### Exemple

```bash
curl -k -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"address": "pms1suspect...", "reason": "AML investigation"}' \
  https://localhost:8443/admin/compliance/freeze
```

---

## POST `/admin/compliance/unfreeze`

Degele une adresse precedemment gelee. Necessite le `freeze_block_id` original pour traçabilite.

### Request Body

```json
{
  "address": "pms1suspect...",
  "reason": "Investigation closed, account cleared",
  "freeze_block_id": "abc123..."
}
```

| Champ | Type | Requis | Description |
|-------|------|--------|-------------|
| `address` | string | oui | Adresse a degeler |
| `reason` | string | oui | Motif du degel |
| `freeze_block_id` | string | oui | ID du bloc Freeze original |

### Response (Succes - 200)

```json
{
  "status": "unfrozen",
  "block_id": "def456...",
  "address": "pms1suspect..."
}
```

### Erreurs

| HTTP | Description |
|------|-------------|
| 401 | Non autorise |
| 404 | Adresse non gelee |

### Exemple

```bash
curl -k -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "address": "pms1suspect...",
    "reason": "Case closed",
    "freeze_block_id": "abc123..."
  }' \
  https://localhost:8443/admin/compliance/unfreeze
```

---

## POST `/admin/compliance/seize`

Saisit les UTXOs d'une adresse et les transfere vers le treasury. Permet de cibler des UTXOs specifiques ou de tout saisir.

### Request Body

```json
{
  "address": "pms1suspect...",
  "reason": "Court order #5678",
  "utxo_ids": [
    {"txid": "block_abc...", "index": 0},
    {"txid": "block_def...", "index": 1}
  ]
}
```

| Champ | Type | Requis | Description |
|-------|------|--------|-------------|
| `address` | string | oui | Adresse dont on saisit les fonds |
| `reason` | string | oui | Motif de la saisie |
| `utxo_ids` | array | non | UTXOs specifiques a saisir. Si vide, saisit tout. |
| `utxo_ids[].txid` | string | - | Block ID du UTXO |
| `utxo_ids[].index` | u32 | - | Index de l'output dans le bloc |

### Response (Succes - 200)

```json
{
  "status": "seized",
  "block_id": "seize_abc123...",
  "from_address": "pms1suspect...",
  "treasury_address": "pms1treasury...",
  "seized_utxos_count": 3
}
```

### Erreurs

| HTTP | Description |
|------|-------------|
| 400 | Pas d'adresse treasury configuree |
| 401 | Non autorise |
| 404 | Aucun UTXO trouve pour l'adresse / UTXOs specifies introuvables |

### Exemple

Saisir tous les fonds d'une adresse :

```bash
curl -k -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"address": "pms1suspect...", "reason": "Court order"}' \
  https://localhost:8443/admin/compliance/seize
```

Saisir des UTXOs specifiques :

```bash
curl -k -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "address": "pms1suspect...",
    "reason": "Court order",
    "utxo_ids": [{"txid": "block_abc...", "index": 0}]
  }' \
  https://localhost:8443/admin/compliance/seize
```

---

## POST `/admin/compliance/reverse`

Inverse une transaction TxUtxo. Consomme les outputs de la TX originale et recree des UTXOs pour les adresses sources originales. Impossible si les outputs ont deja ete depenses.

### Request Body

```json
{
  "block_id": "tx_block_abc123...",
  "reason": "Fraudulent transaction reversal"
}
```

| Champ | Type | Requis | Description |
|-------|------|--------|-------------|
| `block_id` | string | oui | ID du bloc contenant la TX a inverser |
| `reason` | string | oui | Motif de l'inversion |

### Response (Succes - 200)

```json
{
  "status": "reversed",
  "block_id": "reverse_def456...",
  "reversed_block_id": "tx_block_abc123...",
  "refunded_addresses": ["pms1original_sender..."]
}
```

### Erreurs

| HTTP | Description |
|------|-------------|
| 400 | Le bloc n'est pas une TxUtxo (seules les TX UTXO peuvent etre inversees) |
| 401 | Non autorise |
| 404 | Bloc original introuvable |
| 422 | Un ou plusieurs outputs deja depenses |

### Exemple

```bash
curl -k -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"block_id": "tx_block_abc123...", "reason": "Fraud reversal"}' \
  https://localhost:8443/admin/compliance/reverse
```

---

## GET `/admin/compliance/frozen`

Liste toutes les adresses actuellement gelees.

### Response

```json
{
  "frozen_accounts": 2,
  "accounts": [
    {
      "address": "pms1suspect_a...",
      "reason": "AML investigation",
      "block_id": "freeze_abc...",
      "frozen_at_ms": 1706000000000
    },
    {
      "address": "pms1suspect_b...",
      "reason": "Court order",
      "block_id": "freeze_def...",
      "frozen_at_ms": 1706001000000
    }
  ]
}
```

### Exemple

```bash
curl -k -H "Authorization: Bearer $TOKEN" \
  https://localhost:8443/admin/compliance/frozen
```

---

## GET `/admin/compliance/log`

Retourne le journal complet de toutes les actions de compliance (freeze, unfreeze, seize, reverse).

### Response

```json
{
  "total_entries": 5,
  "log": [
    {
      "action": "freeze",
      "address": "pms1suspect...",
      "block_id": "abc123...",
      "reason": "AML investigation",
      "timestamp_ms": 1706000000000
    }
  ]
}
```

### Exemple

```bash
curl -k -H "Authorization: Bearer $TOKEN" \
  https://localhost:8443/admin/compliance/log
```

---

## GET `/admin/compliance/shadow_balance`

Retourne les balances detaillees de tous les comptes geles, incluant la repartition par asset.

### Response

```json
{
  "frozen_accounts": 1,
  "total_pms_frozen": "15000.00000000",
  "accounts": [
    {
      "address": "pms1suspect...",
      "pms_balance": "15000.00000000",
      "frozen_since_ms": 1706000000000,
      "reason": "AML investigation",
      "freeze_block_id": "freeze_abc...",
      "assets": [
        {
          "asset_id": null,
          "balance": "15000.00000000",
          "utxo_count": 5
        },
        {
          "asset_id": "edenite",
          "balance": "200.00",
          "utxo_count": 1
        }
      ]
    }
  ]
}
```

### Exemple

```bash
curl -k -H "Authorization: Bearer $TOKEN" \
  https://localhost:8443/admin/compliance/shadow_balance
```
