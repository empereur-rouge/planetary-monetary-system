# 💸 Transactions API

> Endpoints pour soumettre des blocs et des transactions au DAG

---

## POST `/submit/block`

Soumet un bloc signé au DAG. C'est le point d'entrée principal pour toutes les opérations qui modifient l'état du réseau.

### Request Body

```json
{
  "id": "sha256_hash_du_bloc",
  "parents": ["parent_block_id_1", "parent_block_id_2"],
  "payload_json": "{...}",
  "nonce": 12345,
  "network_id": "mainnet",
  "protocol_version": 1,
  "signer_pk_hex": "04abc123...",
  "signature_hex": "304402...",
  "metadata": {
    "timestamp": 1706000000000
  }
}
```

### Structure du WireBlock

| Champ | Type | Description |
|-------|------|-------------|
| `id` | string | Hash SHA256 du bloc (calculé à partir du contenu) |
| `parents` | string[] | IDs des blocs parents (tips du DAG) |
| `payload_json` | string | Payload JSON sérialisé (voir types ci-dessous) |
| `nonce` | number | Nonce (réservé, peut être 0) |
| `network_id` | string | ID du réseau ("mainnet", "testnet") |
| `protocol_version` | number | Version du protocole (actuellement 1) |
| `signer_pk_hex` | string | Clé publique ECDSA du signataire (hex) |
| `signature_hex` | string | Signature ECDSA du bloc (hex) |
| `metadata` | object | Métadonnées (timestamp, etc.) |

### Types de Payload

#### Transaction UTXO

```json
{
  "Encrypted": {
    "TxUtxo": {
      "inputs": [
        {
          "txid": "previous_block_id",
          "index": 0,
          "signature": "input_signature_hex"
        }
      ],
      "outputs": [
        {
          "recipient_pk_hex": "destination_pk",
          "amount_encrypted": "encrypted_amount_base64"
        }
      ],
      "fee": "0.001"
    }
  }
}
```

#### Mint (Genesis/Rewards)

```json
{
  "Plain": {
    "Mint": {
      "outputs": [
        {
          "address": "pms1recipient...",
          "amount": "1000.00000000"
        }
      ]
    }
  }
}
```

#### NFT Action

```json
{
  "Plain": {
    "Nft": {
      "Mint": {
        "token_id": "cube_123",
        "owner": "pms1owner...",
        "metadata": {...}
      }
    }
  }
}
```

### Validation effectuée

> [!NOTE]
> PMS est un DAG privé. Le **serveur est l'autorité finale** de validation.

1. ✅ **Signature** : Vérification ECDSA de `signature_hex`
2. ✅ **Autorité** : Vérification que le signataire est autorisé (Coordinator ou wallet connu)
3. ✅ **Parents** : Les parents doivent exister dans le DAG
4. ✅ **Network ID** : Doit correspondre au réseau configuré
5. ✅ **Unicité** : Le block ID ne doit pas déjà exister

### Response (Succès)

```json
{
  "status": "ok",
  "block_id": "submitted_block_id",
  "accepted": true
}
```

### Response (Erreurs)

| HTTP | Code | Description |
|------|------|-------------|
| 401 | `INVALID_SIGNATURE` | Signature du bloc invalide |
| 403 | `UNAUTHORIZED` | Signataire non autorisé |
| 400 | `INVALID_PARENTS` | Parents inconnus ou invalides |
| 409 | `DUPLICATE_BLOCK` | Bloc déjà existant |
| 422 | `INVALID_PAYLOAD` | Payload malformé |

### Exemple

```bash
curl -k -X POST https://localhost:8443/submit/block \
  -H "Content-Type: application/json" \
  -d '{
    "id": "abc123...",
    "parents": ["tip1", "tip2"],
    "payload_json": "{\"Plain\":{\"Mint\":{\"outputs\":[]}}}",
    "nonce": 12345,
    "network_id": "testnet",
    "protocol_version": 1,
    "signer_pk_hex": "04...",
    "signature_hex": "304402..."
  }'
```

---

## POST `/v1/tx/prepare`

Prépare une transaction non-signée pour transfert wallet-à-wallet. Le serveur sélectionne les UTXOs, calcule les frais, et retourne une transaction prête à être signée par le client.

> 💡 **Flow recommandé** : Utiliser cet endpoint pour obtenir une TX non-signée, puis signer localement et soumettre via `/wallet/tx/send`.

### Request Body

```json
{
  "from": "pms1sender_address...",
  "to": "pms1recipient_address...",
  "amount": "100.50",
  "asset_id": null
}
```

| Champ | Type | Requis | Description |
|-------|------|--------|-------------|
| `from` | string | oui | Adresse Bech32 de l'expediteur |
| `to` | string | oui | Adresse Bech32 du destinataire |
| `amount` | string | oui | Montant a envoyer (decimal, ex: "100.5") |
| `asset_id` | string | non | ID du token custom (null = PMS natif) |

> **Multi-token** : Pour les tokens custom, les frais sont toujours payes en PMS natif. Le serveur selectionne des UTXOs PMS supplementaires pour couvrir les frais.

### Response (Succès)

```json
{
  "unsigned_tx": {
    "inputs": [
      { "out": { "txid": "abc123...", "index": 0 } }
    ],
    "outputs": [
      { "address": "pms1recipient...", "amount": "100.50" },
      { "address": "pms1sender...", "amount": "49.47" },
      { "address": "pms1admin...", "amount": "0.03" }
    ],
    "fee": "0.03",
    "unlocks": []
  },
  "tx_hash": "sha256_hex_to_sign",
  "fee": "0.03",
  "inputs_detail": [
    { "txid": "abc123...", "index": 0, "amount": "150.00" }
  ]
}
```

| Champ | Type | Description |
|-------|------|-------------|
| `unsigned_tx` | Transaction | Transaction prête à signer (unlocks vide) |
| `tx_hash` | string | Hash SHA256 du message à signer (hex) |
| `fee` | string | Frais calculés |
| `inputs_detail` | UtxoDetail[] | UTXOs sélectionnés comme inputs |

### Flow de signature client

```typescript
// 1. Préparer la TX
const { unsigned_tx, tx_hash } = await fetch('/v1/tx/prepare', {
  method: 'POST',
  body: JSON.stringify({ from, to, amount })
}).then(r => r.json());

// 2. Signer le hash avec la clé privée ECDSA
const signature = secp256k1.sign(
  Buffer.from(tx_hash, 'hex'),
  privateKey
);

// 3. Remplir les unlocks
const signed_tx = {
  ...unsigned_tx,
  unlocks: [{
    pubkey_hex: publicKeyHex,
    signature_b64: Buffer.from(signature.toDER()).toString('base64')
  }]
};

// 4. Soumettre
await fetch('/wallet/tx/send', {
  method: 'POST',
  body: JSON.stringify({
    tx: signed_tx,
    recipients_xpk: [sender_xpk, recipient_xpk]
  })
});
```

### Response (Erreurs)

| HTTP | Code | Description |
|------|------|-------------|
| 400 | `INVALID_AMOUNT` | Montant invalide ou <= 0 |
| 403 | `FROZEN` | Adresse expediteur ou destinataire gelee (compliance) |
| 422 | `NO_UTXOS` | Aucun UTXO disponible pour l'expediteur |
| 422 | `INSUFFICIENT_BALANCE` | Solde insuffisant (amount + fee) |
| 422 | `INSUFFICIENT_PMS_FEE` | Solde PMS insuffisant pour les frais (tokens custom) |

### Exemple

```bash
curl -k -X POST https://localhost:8443/v1/tx/prepare \
  -H "Content-Type: application/json" \
  -d '{
    "from": "pms1qw508d6qejxtdg4y5r3zarvary0c5xw7k...",
    "to": "pms1qrp33g0q5c5txsp9arysrx4k6zdkfs4n...",
    "amount": "100.0"
  }'
```

---

## GET `/blocks/stream`

Stream SSE (Server-Sent Events) des nouveaux blocs ajoutés au DAG.

> 💡 **Usage**: Utile pour les clients qui veulent être notifiés en temps réel des nouvelles transactions.

### Response

Le serveur envoie des événements SSE au format :

```
event: block
data: {"id":"abc123...","payload_json":"..."}

event: block
data: {"id":"def456...","payload_json":"..."}
```

### Exemple avec curl

```bash
curl -k -N https://localhost:8443/blocks/stream
```

### Exemple JavaScript

```javascript
const events = new EventSource('https://localhost:8443/blocks/stream');

events.addEventListener('block', (e) => {
  const block = JSON.parse(e.data);
  console.log('New block:', block.id);
});

events.onerror = (e) => {
  console.error('SSE error:', e);
};
```

---

## POST `/wallet/tx/send`

Endpoint helper pour envoyer une transaction. Le client fournit un `WireBlock` pré-signé.

> 📝 **Note**: Voir [Wallet API](./wallet.md) pour plus de détails.

---

## 🔐 Signature des blocs

### Format du message à signer

Le message canonique à signer est construit ainsi :

```rust
fn canonical_wireblock_message(wb: &WireBlock) -> Vec<u8> {
    // 1. Sérialiser le bloc en JSON canonique (clés triées)
    // 2. Exclure les champs signature_hex et signer_pk_hex
    // 3. Hasher avec SHA256
    serde_json::to_string_canonical(&wb_without_sig)
}
```

### Process de signature

```typescript
import { sha256 } from '@noble/hashes/sha256';
import { secp256k1 } from '@noble/curves/secp256k1';

// 1. Préparer le bloc sans signature
const blockWithoutSig = { ...block };
delete blockWithoutSig.signer_pk_hex;
delete blockWithoutSig.signature_hex;

// 2. Sérialiser en JSON canonique
const message = JSON.stringify(blockWithoutSig, Object.keys(blockWithoutSig).sort());

// 3. Hasher
const hash = sha256(new TextEncoder().encode(message));

// 4. Signer
const signature = secp256k1.sign(hash, privateKey);
block.signature_hex = signature.toDERHex();
block.signer_pk_hex = secp256k1.getPublicKey(privateKey, false).toHex();
```

---

## 📊 Frais de transaction

Les frais sont accumulés dans le **Fee Pool** et distribués lors des Milestones.

| Type de transaction | Frais |
|--------------------|-------|
| Transfer UTXO | 0.1% du montant |
| NFT Mint | Gratuit (signé par Coordinator) |
| NFT Burn | Gratuit |

> 📝 **Note**: Les frais sont configurés dans `settings.toml` et peuvent varier selon le réseau.
