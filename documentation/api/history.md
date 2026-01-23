# 📜 History API

> Endpoints pour consulter l'historique des blocs et transactions

---

## GET `/v1/history/encrypted`

Récupère les blocs avec payloads **chiffrés** (Encrypted), paginés par timestamp.

### Query Parameters

| Paramètre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `after_ts` | number | ❌ | Timestamp (ms) après lequel commencer |
| `after_id` | string | ❌ | Block ID après lequel commencer |
| `limit` | number | ❌ | Nombre max de résultats (défaut: 200, max: 500) |

### Response

```json
{
  "items": [
    {
      "id": "block_abc123",
      "parents": ["parent1", "parent2"],
      "payload_json": "{\"Encrypted\":{...}}",
      "nonce": 12345,
      "network_id": "mainnet",
      "protocol_version": 1,
      "metadata": {"timestamp": 1706000000000}
    }
  ],
  "next_after_ts": 1705999000000,
  "next_after_id": "block_xyz789",
  "has_more": true
}
```

### Pagination

Utilisez `next_after_ts` et `next_after_id` pour la page suivante :

```bash
# Page 1
curl "http://localhost:3000/v1/history/encrypted?limit=100"

# Page 2
curl "http://localhost:3000/v1/history/encrypted?after_ts=1705999000000&after_id=block_xyz789&limit=100"
```

---

## GET `/v1/history/plain`

Récupère les blocs avec payloads **en clair** (Plain), paginés par timestamp.

> 📝 Les payloads Plain incluent : Mint, Reward, NFT actions

### Query Parameters

Identiques à `/v1/history/encrypted`.

### Response

```json
{
  "items": [
    {
      "id": "mint_block_001",
      "parents": ["genesis"],
      "payload_json": "{\"Plain\":{\"Mint\":{\"outputs\":[...]}}}",
      "metadata": {"timestamp": 1700000000000}
    }
  ],
  "next_after_ts": null,
  "next_after_id": null,
  "has_more": false
}
```

---

## POST `/wallet/history`

Récupère l'historique des transactions d'une adresse spécifique.

> 📝 Voir [Wallet API](./wallet.md#post-wallethistory) pour les détails complets.

### Request Body

```json
{
  "bech32_addr": "pms1abc123...",
  "limit": 100
}
```

### Response

```json
{
  "address": "pms1abc123...",
  "items": [
    {
      "block_id": "tx_001",
      "ts_ms": 1706000000000,
      "payload_type": "TxUtxo",
      "payload": {...}
    }
  ],
  "count": 1
}
```

---

## 📊 Types de Payload

| Type | Catégorie | Description |
|------|-----------|-------------|
| `Mint` | Plain | Création de tokens (Genesis, rewards) |
| `TxUtxo` | Encrypted/Plain | Transaction standard |
| `Reward` | Plain | Distribution de récompenses |
| `EncryptedReward` | Encrypted | Récompense avec outputs chiffrés |
| `Nft` | Plain | Actions NFT (Mint, Burn, Transfer) |
