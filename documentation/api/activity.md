# 📊 Activity API

> Flux d'activite complet d'un wallet avec classification semantique et streaming temps reel (SSE).

Contrairement a `/wallet/history` qui retourne des payloads bruts, l'Activity API classifie chaque evenement
avec un type semantique (`fee_received`, `transfer_in`, `nft_mint`, `freeze`, etc.), une direction
(`in`, `out`, `info`), un montant net et la contrepartie.

---

## GET `/v1/wallet/{address}/activity`

Recupere l'activite complete d'un wallet avec classification, filtrage et pagination.

### Path Parameters

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `address` | string | ✅ | Adresse du wallet (bech32 ou hex) |

### Query Parameters

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `type` | string | ❌ | Filtre par type(s) d'activite, separes par virgule (ex: `fee_received,transfer_in`) |
| `limit` | number | ❌ | Nombre max d'elements (defaut: 50, max: 500) |
| `after_ts` | number | ❌ | Cursor de pagination: timestamp (ms) |
| `after_id` | string | ❌ | Cursor de pagination: block ID |
| `asset_id` | string | ❌ | Filtre par asset (undefined = PMS natif) |
| `x25519_sk_hex` | string | ❌ | Cle privee X25519 (hex) pour dechiffrer les payloads chiffres (EncryptedReward, Encrypted) |

### Response

```json
{
  "address": "pms1abc123...",
  "items": [
    {
      "block_id": "a1b2c3d4...",
      "ts_ms": 1740000000000,
      "activity_type": "fee_received",
      "direction": "in",
      "amount": "1.95000000",
      "asset_id": null,
      "counterparty": null,
      "ledger_id": "side-chain-1",
      "payload": { "fee_outputs": [...], "reward_outputs": [...], "burned": "0", "tx_block_id": "..." }
    },
    {
      "block_id": "e5f6g7h8...",
      "ts_ms": 1739999000000,
      "activity_type": "transfer_in",
      "direction": "in",
      "amount": "100.00000000",
      "asset_id": null,
      "counterparty": "pms1sender...",
      "payload": { "inputs": [...], "outputs": [...], "fee": "3.0", "unlocks": [...] }
    }
  ],
  "count": 2,
  "next_after_ts": 1739999000000,
  "next_after_id": "e5f6g7h8...",
  "has_more": true
}
```

| Champ | Type | Description |
|-------|------|-------------|
| `address` | string | Adresse du wallet |
| `items` | ActivityItem[] | Activites classifiees |
| `count` | number | Nombre d'elements retournes |
| `next_after_ts` | number? | Cursor de pagination (timestamp) |
| `next_after_id` | string? | Cursor de pagination (block ID) |
| `has_more` | boolean | S'il y a plus de resultats |

### ActivityItem

| Champ | Type | Description |
|-------|------|-------------|
| `block_id` | string | ID du bloc contenant l'activite |
| `ts_ms` | number | Timestamp en millisecondes |
| `activity_type` | string | Type d'activite semantique (voir tableau ci-dessous) |
| `direction` | string | `"in"` (recu), `"out"` (envoye), `"info"` (informatif) |
| `amount` | string? | Montant net pour cette adresse (absent pour NFT/compliance) |
| `asset_id` | string? | Asset ID (null = PMS natif) |
| `counterparty` | string? | Adresse de la contrepartie |
| `ledger_id` | string? | Ledger ID d'origine (absent en mode single-ledger "main") |
| `payload` | object | Payload brut pour details complets |

### Types d'activite

| Type | Direction | Description |
|------|-----------|-------------|
| `transfer_in` | in | Reception de PMS ou token |
| `transfer_out` | out | Envoi de PMS ou token |
| `transfer_self` | info | Envoi a soi-meme (consolidation) |
| `fee_received` | in | Fee recue (coordinateur ou tresor) |
| `reward` | in | Reward de bloc |
| `mint` | in | Mint recu |
| `nft_mint` | in | NFT cree |
| `nft_transfer_in` | in | NFT recu |
| `nft_transfer_out` | out | NFT envoye |
| `nft_burn` | out | NFT brule |
| `nft_use` | info | NFT utilise |
| `token_create` | info | Token cree |
| `bridge_lock_in` | in | Fonds verouilles pour bridge (dest) |
| `bridge_lock_out` | out | Fonds verouilles pour bridge (source) |
| `bridge_mint` | in | Fonds bridge recus |
| `freeze` | info | Compte gele |
| `unfreeze` | info | Compte degele |
| `seized` | out | Fonds saisis |
| `seize_received` | in | Fonds saisis recus (tresor) |
| `reverse_received` | in | Transaction inversee (fonds rendus) |

### Exemples

```bash
# Toute l'activite
curl -k https://localhost:8443/v1/wallet/pms1abc.../activity

# Seulement les fees recues
curl -k "https://localhost:8443/v1/wallet/pms1abc.../activity?type=fee_received"

# Fees + transferts entrants, limite 20
curl -k "https://localhost:8443/v1/wallet/pms1abc.../activity?type=fee_received,transfer_in&limit=20"

# Pagination (page 2)
curl -k "https://localhost:8443/v1/wallet/pms1abc.../activity?after_ts=1739999000000&after_id=e5f6g7h8..."

# Filtrer par asset
curl -k "https://localhost:8443/v1/wallet/pms1abc.../activity?asset_id=edenite"

# Avec dechiffrement des payloads chiffres (TLS requis)
curl -k "https://localhost:8443/v1/wallet/pms1abc.../activity?x25519_sk_hex=abcdef0123..."
```

#### SDK TypeScript

```typescript
import { PmsClient } from "@empereur-rouge/pms-sdk";

const client = new PmsClient({ nodeUrl: "https://node.pms.network", apiKey: "pk_..." });

// Toute l'activite
const activity = await client.getActivity("pms1abc...");

// Filtrer les fees recues
const fees = await client.getActivity("pms1abc...", {
  type: ["fee_received"],
  limit: 20,
});

// Pagination
const page2 = await client.getActivity("pms1abc...", {
  after_ts: activity.next_after_ts,
  after_id: activity.next_after_id,
});
```

---

## GET `/v1/wallet/{address}/activity/stream`

Ecoute l'activite d'un wallet en temps reel via **Server-Sent Events (SSE)**.

> 💡 Les evenements sont filtres en memoire (zero acces DB) grace aux adresses pre-calculees
> dans l'EventBus. Performant meme sous haute charge.

### Path Parameters

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `address` | string | ✅ | Adresse du wallet (bech32 ou hex) |

### Query Parameters

| Parametre | Type | Requis | Description |
|-----------|------|--------|-------------|
| `type` | string | ❌ | Filtre par type(s) d'activite, separes par virgule |
| `x25519_sk_hex` | string | ❌ | Cle privee X25519 (hex) pour dechiffrer les payloads en temps reel |

### SSE Events

Chaque evenement SSE a le format :

```
event: activity
data: {"block_id":"a1b2...","ts_ms":1740000000000,"activity_type":"fee_received","direction":"in","amount":"1.95","ledger_id":"side-chain-1","payload":{...}}

```

En cas de retard du client :

```
event: warning
data: {"warning":"lagged by 5 events"}

```

### Exemples

```bash
# Stream de toute l'activite (connexion persistante)
curl -k -N "https://localhost:8443/v1/wallet/pms1abc.../activity/stream"

# Stream filtre sur les fees et transfers
curl -k -N "https://localhost:8443/v1/wallet/pms1abc.../activity/stream?type=fee_received,transfer_in"
```

#### SDK TypeScript

```typescript
import { PmsClient } from "@empereur-rouge/pms-sdk";

const client = new PmsClient({ nodeUrl: "https://node.pms.network", apiKey: "pk_..." });

// Ecouter l'activite en temps reel
const stop = client.streamActivity("pms1abc...", {
  type: ["transfer_in", "fee_received"],
  onActivity: (item) => {
    console.log(`${item.activity_type}: ${item.amount} (${item.direction})`);
  },
  onError: (err) => console.error("SSE error:", err),
  onClose: () => console.log("Stream closed"),
});

// Plus tard: fermer la connexion
stop();
```

---

## Notes

> 🔐 **Encrypted payloads**: Passez `x25519_sk_hex` en query parameter pour dechiffrer les
> `EncryptedReward` et `Encrypted` envelopes. Sans cette cle, les payloads chiffres sont ignores.
> **Securite**: ne passez la cle privee que sur une connexion TLS.

> 📝 **Detection du sender**: Le endpoint GET (non-streaming) resout le sender d'une transaction
> via le cache UTXO pour classifier correctement `transfer_in` vs `transfer_out`.
> Le streaming SSE classifie uniquement par les outputs (pas de lookup UTXO async).

> 💡 **Multi-Ledger**: Les routes sont disponibles sous `/l/{ledger_id}/v1/wallet/{address}/activity`
> pour cibler un ledger specifique.
