# Tokens API

> Endpoints pour la gestion des tokens custom (multi-token)

---

## GET `/v1/tokens`

Liste tous les tokens enregistres dans le registre.

### Response

```json
{
  "tokens": [
    {
      "asset_id": "edenite",
      "symbol": "EDN",
      "name": "Edenite",
      "decimals": 8,
      "max_supply": "1000000.00000000",
      "creator": "04abc123...",
      "mint_authority": "04abc123..."
    }
  ]
}
```

### Exemple

```bash
curl -k https://localhost:8443/v1/tokens
```

---

## GET `/v1/tokens/{asset_id}`

Recupere les informations d'un token specifique.

### Path Parameters

| Parametre | Type | Description |
|-----------|------|-------------|
| `asset_id` | string | Identifiant unique du token |

### Response (Succes)

```json
{
  "asset_id": "edenite",
  "symbol": "EDN",
  "name": "Edenite",
  "decimals": 8,
  "max_supply": "1000000.00000000",
  "creator": "04abc123...",
  "mint_authority": "04abc123..."
}
```

### Response (Non trouve)

```json
{
  "error": "token not found: edenite"
}
```

| HTTP | Description |
|------|-------------|
| 200 | Token trouve |
| 404 | Token inexistant |

### Exemple

```bash
curl -k https://localhost:8443/v1/tokens/edenite
```

---

## POST `/admin/tokens/create`

Cree un nouveau token custom dans le registre. Necessite les droits admin.

### Request Body

```json
{
  "asset_id": "edenite",
  "symbol": "EDN",
  "name": "Edenite",
  "decimals": 8,
  "max_supply": "1000000.00000000"
}
```

| Champ | Type | Requis | Description |
|-------|------|--------|-------------|
| `asset_id` | string | oui | ID unique (1-32 chars, lowercase alphanum ou `_`) |
| `symbol` | string | oui | Symbole du token (ex: "EDN") |
| `name` | string | oui | Nom complet du token |
| `decimals` | u8 | oui | Nombre de decimales (ex: 8) |
| `max_supply` | string | non | Supply max (si absent, supply illimitee) |

### Response (Succes - 201)

```json
{
  "status": "ok",
  "token": {
    "asset_id": "edenite",
    "symbol": "EDN",
    "name": "Edenite",
    "decimals": 8,
    "max_supply": "1000000.00000000",
    "creator": "04abc123...",
    "mint_authority": "04abc123..."
  }
}
```

### Erreurs

| HTTP | Description |
|------|-------------|
| 400 | `asset_id` invalide (format) |
| 401 | Non autorise |
| 409 | Token deja existant |

### Exemple

```bash
curl -k -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "asset_id": "edenite",
    "symbol": "EDN",
    "name": "Edenite",
    "decimals": 8,
    "max_supply": "1000000.00000000"
  }' \
  https://localhost:8443/admin/tokens/create
```

---

## POST `/admin/tokens/mint`

Mint des tokens custom vers une adresse. Necessite les droits admin. Verifie la `max_supply` si definie.

### Request Body

```json
{
  "asset_id": "edenite",
  "to": "pms1recipient...",
  "amount": "500.00000000"
}
```

| Champ | Type | Requis | Description |
|-------|------|--------|-------------|
| `asset_id` | string | oui | ID du token a minter |
| `to` | string | oui | Adresse Bech32 du destinataire |
| `amount` | string | oui | Montant (decimale positive) |

### Response (Succes - 200)

```json
{
  "status": "ok",
  "block_id": "abc123...",
  "asset_id": "edenite",
  "amount": "500.00000000",
  "to": "pms1recipient..."
}
```

### Erreurs

| HTTP | Description |
|------|-------------|
| 400 | Montant invalide |
| 401 | Non autorise |
| 404 | Token inexistant |
| 422 | Depasserait la `max_supply` |

### Exemple

```bash
curl -k -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "asset_id": "edenite",
    "to": "pms1qw508d6qejxtdg4y5r3zarvary0c5xw7k...",
    "amount": "500.0"
  }' \
  https://localhost:8443/admin/tokens/mint
```
