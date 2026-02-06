# 💰 Wallet API

> Endpoints pour la gestion des portefeuilles et des balances

---

## POST `/wallet/balance`

Récupère le solde d'un wallet avec décryptage des UTXOs chiffrés.

### Request Body

```json
{
  "bech32_addr": "pms1abc123...",
  "x25519_sk_hex": "clé_privée_x25519_en_hex",
  "ecdsa_pk_hex": "clé_publique_ecdsa_en_hex",
  "scan_limit": 2000
}
```

| Champ | Type | Requis | Description |
|-------|------|--------|-------------|
| `bech32_addr` | string | ✅ | Adresse Bech32 du wallet |
| `x25519_sk_hex` | string | ✅ | Clé privée X25519 (hex) pour décrypter |
| `ecdsa_pk_hex` | string | ✅ | Clé publique ECDSA (hex) |
| `scan_limit` | number | ❌ | Nombre max d'UTXOs à scanner (défaut: 2000) |

### Response

```json
{
  "balance": "1234.56789012",
  "utxos": [
    {
      "txid": "abc123...",
      "index": 0,
      "amount": "100.00000000"
    }
  ]
}
```

### Exemple

```bash
curl -k -X POST https://localhost:8443/wallet/balance \
  -H "Content-Type: application/json" \
  -d '{
    "bech32_addr": "pms1qw508d6qejxtdg4y5r3zarvary0c5xw7k...",
    "x25519_sk_hex": "a1b2c3...",
    "ecdsa_pk_hex": "04abcd..."
  }'
```

---

## POST `/v1/balance`

Version simplifiée : récupère le solde d'une adresse sans clés privées.

> ⚠️ **Note**: Cette méthode ne peut pas décrypter les UTXOs chiffrés.

### Request Body

```json
{
  "address": "pms1abc123..."
}
```

### Response

```json
{
  "balance": "1234.56789012"
}
```

### Exemple

```bash
curl -k -X POST https://localhost:8443/v1/balance \
  -H "Content-Type: application/json" \
  -d '{"address": "pms1qw508d6qejxtdg4y5r3zarvary0c5xw7k..."}'
```

---

## GET `/v1/wallet/{address}/utxos`

Récupère les UTXOs (Unspent Transaction Outputs) d'une adresse.

### Path Parameters

| Paramètre | Type | Description |
|-----------|------|-------------|
| `address` | string | Adresse Bech32 du wallet |

### Response

```json
{
  "utxos": [
    {
      "address": "pms1abc123...",
      "amount": "100.00000000",
      "outpoint": {
        "txid": "abc123def456...",
        "index": 0
      }
    }
  ]
}
```

### Exemple

```bash
curl -k https://localhost:8443/v1/wallet/pms1abc123.../utxos
```

---

## POST `/wallet/tx/send`

Envoie des tokens depuis un wallet vers une adresse destination.

### Request Body

```json
{
  "wb": {
    "id": "block_id",
    "parents": ["parent1", "parent2"],
    "payload_json": "{...}",
    "nonce": 12345,
    "network_id": "mainnet",
    "protocol_version": 1,
    "signer_pk_hex": "04abc...",
    "signature_hex": "304...",
    "metadata": {}
  }
}
```

> 📝 **Note**: Le `WireBlock` doit être pré-signé côté client avec la clé privée du wallet source. Utilisez le SDK TypeScript pour simplifier cette opération.

### Response (Succès)

```json
{
  "status": "ok",
  "block_id": "abc123...",
  "fee": "0.00100000"
}
```

### Response (Erreur - Fonds insuffisants)

```json
{
  "status": "error",
  "code": "INSUFFICIENT_FUNDS",
  "message": "Not enough balance"
}
```

### Codes d'erreur

| Code | HTTP | Description |
|------|------|-------------|
| `INVALID_SIGNATURE` | 401 | Signature du bloc invalide |
| `INSUFFICIENT_FUNDS` | 422 | Solde insuffisant |
| `DUPLICATE_INPUTS` | 400 | UTXOs déjà dépensés |
| `INVALID_PAYLOAD` | 400 | Format du payload invalide |

### Exemple avec SDK

```typescript
import { PmsClient, Wallet } from '@pms/sdk';

const client = new PmsClient('https://localhost:8443');
const wallet = await Wallet.create();

const result = await client.send({
  to: 'pms1destination...',
  amount: '100.0',
  wallet
});
console.log(result.blockId);
```

---

## POST `/wallet/history`

Récupère l'historique des transactions d'un wallet.

### Request Body

```json
{
  "bech32_addr": "pms1abc123...",
  "limit": 100
}
```

| Champ | Type | Requis | Description |
|-------|------|--------|-------------|
| `bech32_addr` | string | ✅ | Adresse Bech32 du wallet |
| `limit` | number | ❌ | Nombre max d'items (défaut: 100, max: 500) |

### Response

```json
{
  "address": "pms1abc123...",
  "items": [
    {
      "block_id": "abc123...",
      "ts_ms": 1706000000000,
      "payload_type": "TxUtxo",
      "payload": {
        "inputs": [...],
        "outputs": [...]
      }
    },
    {
      "block_id": "def456...",
      "ts_ms": 1705999000000,
      "payload_type": "Mint",
      "payload": {
        "outputs": [{"address": "...", "amount": "1000"}]
      }
    },
    {
      "block_id": "ghi789...",
      "ts_ms": 1705998000000,
      "payload_type": "Nft",
      "payload": {
        "Mint": {
          "token_id": "cube_123",
          "owner": "pms1...",
          "metadata": {...}
        }
      }
    }
  ],
  "count": 3
}
```

### Types de payload

| Type | Description |
|------|-------------|
| `Mint` | Émission de tokens (Genesis ou récompense) |
| `TxUtxo` | Transaction standard |
| `Reward` | Récompense de bloc |
| `EncryptedReward` | Récompense chiffrée |
| `Nft` | Action NFT (Mint, Burn, Transfer) |

### Exemple

```bash
curl -k -X POST https://localhost:8443/wallet/history \
  -H "Content-Type: application/json" \
  -d '{"bech32_addr": "pms1abc...", "limit": 50}'
```

---

## 📊 Précision des montants

Tous les montants sont en **chaînes de caractères** avec jusqu'à **8 décimales**.

```json
{
  "balance": "1234.56789012",
  "amount": "0.00000001"
}
```

> ⚠️ **Important**: Utilisez des bibliothèques de précision décimale (comme `Decimal` en Rust ou `bignumber.js` en JS) pour manipuler les montants. Évitez les calculs en virgule flottante.
