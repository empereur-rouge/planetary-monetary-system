---
tags: [guide, payment, integration]
created: 2026-05-04
updated: 2026-05-04
version: v0.8.0
---

# Guide d'intégration paiement PMS

Comment accepter des paiements (PMS natif **ou** custom token) dans ton app, depuis l'inscription d'un user jusqu'au payout d'un créateur.

## Audience

Backend engineer qui intègre PMS comme rail de paiement dans : SaaS streaming, e-commerce, marketplace, plateforme de tips, abonnement récurrent, marketplace NFT, etc.

## Pré-requis

- Un node PMS déployé et accessible (testnet : `https://testnet.pms-network.com`, mainnet : ton déploiement à venir)
- Un **`PMS_ADMIN_TOKEN`** — récupéré au moment du provisioning du node (cf. [[deployment-operations]])
- (Optionnel) Une **API key SDK** émise via `POST /admin/api-keys` — préférable à l'admin token pour les appels du SDK côté client (scopes granulaires, révocable, cf. [[api-key-authentication]])
- **Un master mnemonic BIP39** stocké en cold storage (HSM, hardware wallet, sealed `node.key.enc`). C'est la racine de toutes les adresses de dépôt utilisateurs

---

## 1. Modèle de flux

```
[User wallet]
     │ on-chain TX (signed)
     ▼
[Adresse de dépôt unique par user]   ← dérivée BIP44 depuis ton master mnemonic
     │ détectée par ton watcher (webhook OU SSE OU polling)
     ▼
[Ton backend SaaS — credit user balance off-chain]
     │ payout demandé
     ▼
[Créateur wallet PMS]                ← TX on-chain sortante via /v1/wallet/send-simple
```

**Règle d'or** : les **micro-transactions internes** (tips entre users sur la même plateforme, points de fidélité, etc.) restent **off-chain dans ta DB**. Seuls les **dépôts** (acheter des crédits) et les **payouts** (créateur retire ses gains) sont on-chain. Ça élimine les frais et la latence.

---

## 2. Setup — dériver N adresses de dépôt depuis 1 seed

Pour ne pas stocker N clés privées, on utilise **BIP32/BIP44** ([[hd-wallet-bip32]]). Une seule seed (12 ou 24 mots BIP39) en cold storage produit déterministiquement N wallets enfants.

### 2.1 Côté serveur (Rust)

```rust
use pms_wallet::hd::{master_xprv_from_mnemonic, derive_child_wallet_at_index};

// Au boot du serveur — décrypter le mnemonic depuis ton secrets manager
let mnemonic = decrypt_master_mnemonic_from_hsm()?;
let master = master_xprv_from_mnemonic(&mnemonic, "")?;

// Pour chaque nouveau user → dériver une adresse de dépôt
let user_index: u32 = next_user_index_from_db();    // counter monotone par user
let user_wallet = derive_child_wallet_at_index(&master, /* account */ 0, user_index)?;

let deposit_addr = user_wallet.get_address("8e");   // hrp = "8e" pour mainnet/testnet PMS
let user_x25519_sk = user_wallet.x25519_sk_hex();   // utile pour décrypter les payloads encrypted

// Stocker en DB : (user_id, user_index, deposit_addr) — JAMAIS la clé privée.
// Re-déduit à la demande pour signer un payout, ou pour décrypter un payload.
db.save_deposit_address(user_id, user_index, &deposit_addr).await?;
```

### 2.2 Côté SDK TypeScript

Le SDK PMS (`@empereur-rouge/pms-sdk`) expose la dérivation BIP32 — utile si tu fais la dérivation côté client (mobile app) plutôt que côté backend.

```ts
import { Wallet } from "@empereur-rouge/pms-sdk";

const master = Wallet.fromMnemonic(mnemonic, "");
const userWallet = master.deriveBip44(/* account */ 0, /* index */ user_index);
const depositAddr = userWallet.getAddress("8e");
```

### 2.3 Multi-tenant : un `account` BIP44 par tenant

Si ton SaaS est white-label (chaque client SaaS a son propre namespace), utilise le champ `account` du chemin BIP44 :

| Tenant | Path | Usage |
|--------|------|-------|
| Plateforme principale | `m/44'/PMS'/0'/0/N` | comptes utilisateurs réguliers |
| Tenant A | `m/44'/PMS'/1'/0/N` | comptes du tenant A |
| Tenant B | `m/44'/PMS'/2'/0/N` | comptes du tenant B |
| Treasury | `m/44'/PMS'/99'/0/0` | hot wallet pour les payouts |

⚠️ **Limitation actuelle** : pas de mode "vrai watch-only" (xpub-only sans seed). Le pattern recommandé est : seed chiffrée at-rest (HSM ou fichier `.enc`), déchiffrée brièvement à la demande pour dériver une adresse, puis effacée de la RAM. Détails et roadmap Phase 2.5 dans [[hd-wallet-bip32]].

---

## 3. Recevoir un paiement (déposit)

### 3.1 Choisir un mode de détection

Trois mécanismes complémentaires — choisis selon ta stack :

| Mode | Endpoint | Pour qui |
|------|----------|----------|
| **Webhook HMAC** | `POST /admin/webhooks` | Backends serverless (Lambda, Cloud Functions) qui ne peuvent pas garder une connexion ouverte |
| **Multi-address SSE** | `GET /v1/activity/stream?addresses=...` | Backends persistents (Node.js, Go, Rust) qui hold une connexion. 1 stream / max 1000 addresses |
| **Polling `/v1/blocks/range`** | `GET /v1/blocks/range?after_ts=...` | Cron / batch jobs ; reprise après crash (idempotent) |

Tu peux **combiner** : webhook en priorité + polling `/v1/blocks/range` toutes les 5 min comme filet de sécurité (au cas où une livraison webhook a été perdue).

### 3.2 Mode A — Webhook HMAC

#### Inscription

```bash
curl -X POST https://testnet.pms-network.com/admin/webhooks \
  -H "Authorization: Bearer $PMS_ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "addresses": ["8e1addr_user_001", "8e1addr_user_002"],
    "callback_url": "https://saas.example.com/pms-webhook"
  }'

# Response (201 Created):
# {
#   "subscription_id": "abc123def456...",
#   "secret": "<32 hex chars — RETOURNÉ UNE SEULE FOIS, PERSISTE-LE>",
#   "addresses_count": 2
# }
```

⚠️ **Le `secret` n'est exposé qu'à la création.** `GET /admin/webhooks` ne le re-renvoie jamais. Stocke-le immédiatement dans ton secrets manager.

⚠️ **In-memory only (Phase 4)** : les subscriptions sont perdues au restart du node PMS. Ton serveur doit re-register au boot via un heartbeat sur `/v1/version` (cf. [[webhook-delivery]]). Persistance en Phase 4.5.

#### Réception (Node.js / Express exemple)

```ts
import { createHmac, timingSafeEqual } from "node:crypto";

app.post("/pms-webhook", express.raw({ type: "application/json" }), (req, res) => {
  const signatureHeader = req.headers["x-pms-signature"] as string;
  if (!signatureHeader?.startsWith("sha256=")) {
    return res.status(400).send("invalid signature header");
  }
  const sigHex = signatureHeader.slice("sha256=".length);
  const expected = createHmac("sha256", PMS_WEBHOOK_SECRET)
    .update(req.body)
    .digest("hex");

  if (!timingSafeEqual(Buffer.from(sigHex, "hex"), Buffer.from(expected, "hex"))) {
    return res.status(401).send("hmac mismatch");
  }

  const event = JSON.parse(req.body.toString());
  // event = { subscription_id, block_id, address, ts_ms, encrypted, ledger_id }

  // Idempotence : utilise (block_id, address) comme clé unique en DB
  if (await alreadyProcessed(event.block_id, event.address)) {
    return res.status(200).send("already processed");
  }

  // Verify finality avant de créditer (cf. section 3.5)
  await scheduleDepositCredit(event);

  res.status(200).send("ok");
});
```

### 3.3 Mode B — Multi-address SSE

```ts
import { EventSource } from "eventsource";

// 1 stream pour jusqu'à 1000 adresses simultanées
const addrs = await db.getAllUserDepositAddresses();
const url = `https://testnet.pms-network.com/v1/activity/stream?addresses=${addrs.join(",")}`;
const sse = new EventSource(url);

sse.addEventListener("activity", async (ev) => {
  const item = JSON.parse(ev.data);
  // item = { block_id, ts_ms, activity_type, direction, amount, asset_id, payload: { address, ... } }

  if (item.direction !== "in") return; // outgoing depuis nos adresses → ignore
  if (item.activity_type === "encrypted") {
    // Le serveur ne peut pas décrypter. Re-fetch via le single-address stream
    // avec la clé X25519 du user pour avoir le détail.
    return;
  }

  await scheduleDepositCredit({
    block_id: item.block_id,
    address: item.payload.address,
    amount: item.amount,
    asset_id: item.asset_id,
    ts_ms: item.ts_ms,
  });
});
```

### 3.4 Mode C — Polling `/v1/blocks/range` (recovery / batch)

Pour rattraper après un crash watcher OU comme job cron périodique :

```bash
# Page 1 — most recent first
curl "https://testnet.pms-network.com/v1/blocks/range?limit=100"

# Response:
# {
#   "blocks": [
#     {"id": "abc...", "ts_ms": 1777920321494},
#     ...
#   ],
#   "next_cursor": {
#     "ts_ms": 1777920320000,
#     "id": "def...",
#     "has_more": true
#   }
# }

# Page 2 — pass the cursor
curl "https://testnet.pms-network.com/v1/blocks/range?limit=100&after_ts=1777920320000&after_id=def..."
```

Pour chaque `block_id` retourné, fais un lookup détaillé :

```bash
curl "https://testnet.pms-network.com/v1/transaction/abc..."

# Response:
# {
#   "tx_hash": "abc...",          // par convention PMS = block_id (1 TX = 1 block)
#   "block_id": "abc...",
#   "from": "8e1...",             // null si Mint/Reward
#   "to": "8e1addr_user_001",     // ton adresse de dépôt
#   "amount": "10",
#   "asset_id": null,             // null = PMS natif, sinon "edenite", "monjeton", etc.
#   "fee": "0.03",
#   "timestamp_ms": 1777920321494,
#   "is_finalized": true,
#   "depth": 12,
#   "status": "confirmed",        // pending | confirmed | finalized
#   "inputs": [...],
#   "outputs": [...]
# }
```

L'endpoint est **idempotent** : la même requête retourne les mêmes blocks tant que tu passes le cursor. Sécurise ton watcher contre les double-traitements via `(block_id, address)` comme clé unique en DB.

### 3.5 Vérifier la finalité avant de créditer

Dans le DAG PMS, "N confirmations" = `depth` (nombre de descendants distincts). Recommandation tirée du PROTOCOL.md du SaaS (cf. [[payment-rail-integration]]) :

| Montant USD-equivalent | Seuil minimal | Action |
|------------------------|---------------|--------|
| < $10 | `depth >= 1` OU `is_finalized` | Crédite immédiat |
| $10 – $100 | `depth >= 3` | Crédite après 3 confirmations |
| $100 – $1 000 | `depth >= 6` OU `is_finalized` | Crédite après 6 confirmations |
| > $1 000 | `is_finalized: true` | Attendre la finalité k-depth (≈ minutes) |

Pattern : poll `/v1/transaction/{block_id}` toutes les 5 sec jusqu'à atteindre le seuil, puis crédite.

### 3.6 Idempotence du crédit

```sql
-- Schéma recommandé pour ton store
CREATE TABLE deposits (
  block_id     TEXT NOT NULL,
  address      TEXT NOT NULL,
  amount       NUMERIC NOT NULL,
  asset_id     TEXT,
  status       TEXT NOT NULL,          -- 'pending' | 'credited' | 'rejected'
  ts_ms        BIGINT NOT NULL,
  PRIMARY KEY (block_id, address)      -- ← anti double-crédit
);

-- À la réception (webhook OU SSE OU polling), tente l'INSERT
INSERT INTO deposits (block_id, address, amount, asset_id, status, ts_ms)
VALUES ($1, $2, $3, $4, 'pending', $5)
ON CONFLICT (block_id, address) DO NOTHING
RETURNING block_id;

-- Si retour vide → déjà traité (deux events arrivent en parallèle, ou retry webhook).
-- Sinon → marche du crédit normal.
```

---

## 4. Envoyer un paiement (payout)

### 4.1 Estimer le fee avant de signer

```bash
curl -X POST https://testnet.pms-network.com/v1/estimate-fee \
  -H "Content-Type: application/json" \
  -d '{ "amount": "100", "asset_id": null }'

# Response:
# {
#   "fee": "3.0000001",
#   "transfer_fee": "0",       // fee additionnel issu d'un smart contract OnTransfer
#   "total": "103.0000001",    // amount + fee + transfer_fee
#   "fee_breakdown": []
# }
```

Affiche `total` à l'utilisateur AVANT qu'il signe.

### 4.2 Send-simple (cas usuel — coordinator-side signing)

C'est le path le plus simple : tu envoies la clé privée temporairement, le serveur signe + broadcast.

```bash
curl -X POST https://testnet.pms-network.com/v1/wallet/send-simple \
  -H "Content-Type: application/json" \
  -d '{
    "private_key_b64": "<base64 32-byte privkey>",
    "to": "8e1creator_addr",
    "amount": "100"
  }'

# Response:
# {
#   "block_id": "fef2a248...",
#   "fee": "3.0000001",
#   "transfer_fee": "0"
# }
```

Le `block_id` est l'identifiant public de la TX (par convention PMS, 1 TX = 1 block). Stocke-le dans ta table `payouts` pour le suivi.

### 4.3 Sign offline + broadcast (cas avancé — pas de privkey sur le serveur)

Pour les setups où tu ne veux pas que le serveur voie la clé privée (ex : signing depuis un hardware wallet user-side), utilise le path en deux étapes :

```bash
# Étape 1 — Le serveur prépare la TX non signée
curl -X POST https://testnet.pms-network.com/v1/tx/prepare \
  -H "Content-Type: application/json" \
  -d '{
    "from": "8e1sender",
    "to": "8e1recipient",
    "amount": "100",
    "asset_id": null
  }'

# Response:
# {
#   "unsigned_tx": { "inputs": [...], "outputs": [...], "unlocks": [] },
#   "tx_hash": "<hex SHA-256 du canonical>",  ← message à signer
#   "fee": "3.0000001",
#   "transfer_fee": "0",
#   "inputs_detail": [...]
# }

# Étape 2 — CLIENT-SIDE : signer tx_hash avec ta clé privée ECDSA
#   sig_b64 = base64(ECDSA_DER(secp256k1_sign(privkey, tx_hash)))

# Étape 3 — Broadcaster la TX signée
curl -X POST https://testnet.pms-network.com/wallet/tx/send \
  -H "Content-Type: application/json" \
  -d '{
    "tx": {
      "inputs": [...],
      "outputs": [...],
      "fee": "3.0000001",
      "unlocks": [{ "pubkey_hex": "...", "signature_b64": "..." }]
    },
    "recipients_xpk": ["<x25519 pubkey du destinataire>"]
  }'
```

⚠️ **Cross-chain replay protection (v0.8.0)** : le `tx_hash` à signer est calculé sur `{network_id, inputs, outputs, fee}` — le `network_id` est inclus dans le canonical JSON. Si tu signes une TX sur testnet (`pms-testnet-v1`) et tentes de la rejouer sur mainnet (`pms-mainnet-v1`), elle sera rejetée. Détails dans [[validation-consensus]].

### 4.4 Tracker la TX

```bash
curl https://testnet.pms-network.com/v1/transaction/<block_id>

# Wait until `is_finalized: true` ou `depth >= seuil` selon le montant.
```

---

## 5. Custom token sur custom ledger

Pour créer ton propre token (ex : "MJET" pour "Mon Jeton") sur ton propre ledger isolé.

### 5.1 Créer un ledger

```bash
curl -X POST https://testnet.pms-network.com/admin/ledgers \
  -H "Authorization: Bearer $PMS_ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "id": "monapp",
    "network_id": "monapp-net",
    "prefix": "monapp",
    "symbol": "MJET",
    "owner_pubkey": "<ton ECDSA pubkey hex>"
  }'
```

À partir de maintenant, toutes les opérations sur `monapp` se font via `/l/monapp/*` :

```bash
# Balance d'un user sur le ledger monapp
curl -X POST https://testnet.pms-network.com/l/monapp/v1/balance \
  -d '{ "address": "8e1user", "asset_id": "MJET" }'
```

### 5.2 Déposer du gas (PMS) sur le ledger

Les ledgers custom consomment du PMS comme gas pour les TX. Dépose un bucket :

```bash
curl -X POST https://testnet.pms-network.com/admin/gas-pool/deposit \
  -H "Authorization: Bearer $PMS_ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{ "ledger_id": "monapp", "amount": "10000" }'
```

### 5.3 Créer le token

```bash
curl -X POST https://testnet.pms-network.com/l/monapp/admin/tokens \
  -H "Authorization: Bearer $PMS_ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "asset_id": "MJET",
    "symbol": "MJET",
    "name": "Mon Jeton",
    "decimals": 8,
    "max_supply": "1000000",
    "mint_authority": "<ton ECDSA pubkey hex>"
  }'
```

### 5.4 Mint pour un user

```bash
curl -X POST https://testnet.pms-network.com/l/monapp/admin/tokens/mint \
  -H "Authorization: Bearer $PMS_ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "asset_id": "MJET",
    "to": "8e1user_addr",
    "amount": "100"
  }'
```

### 5.5 Transfer entre users

Identique au transfer PMS, mais avec `asset_id` :

```bash
curl -X POST https://testnet.pms-network.com/l/monapp/v1/wallet/send-simple \
  -d '{
    "private_key_b64": "...",
    "to": "8e1recipient",
    "amount": "10",
    "asset_id": "MJET"
  }'
```

⚠️ **Smart contract `OnTransfer`** : tu peux installer un contrat qui prélève automatiquement un % de transfer fee vers ton wallet à chaque transfert (cf. [[smart-contracts]]). Visible dans `estimate-fee` → `transfer_fee` + `fee_breakdown`.

---

## 6. Sécurité

### 6.1 Vérification HMAC sur les webhooks (obligatoire)

Sans ça, n'importe qui peut spammer ton endpoint et faire croire à des dépôts inexistants.

```ts
// ✅ correct — comparaison constant-time
import { timingSafeEqual } from "node:crypto";
if (!timingSafeEqual(Buffer.from(sigHex, "hex"), Buffer.from(expected, "hex"))) {
  return res.status(401).send("hmac mismatch");
}

// ❌ vulnerable — timing attack
if (sigHex !== expected) { ... }
```

### 6.2 Toujours valider l'adresse de destination AVANT de signer un payout

```rust
use pms_wallet::decode_address;

// 1. Vérifie que l'adresse est bien bech32m + le bon HRP
let (h20_hex, x25519_pub_hex) = decode_address(&recipient_addr)
    .map_err(|e| anyhow!("Invalid recipient address: {e}"))?;

// 2. Si tu opères en mainnet, vérifier que l'adresse est mainnet
//    (le HRP "8e" est partagé testnet/mainnet PMS — mais le network_id
//    dans la TX signée fera le tri au moment du broadcast)

// 3. Seulement maintenant : signer + broadcaster
```

### 6.3 Cold/hot wallet pour les payouts

- **Cold** (offline) : master mnemonic, 99% des fonds en treasury wallet `m/44'/PMS'/99'/0/0`
- **Hot** (serveur) : 1-2 semaines de payouts seulement, dérivé à la demande depuis le master déchiffré
- **Renflouer** le hot wallet manuellement quand < 30% du budget

### 6.4 Tiers de finalité (PROTOCOL.md du SaaS)

Cf. tableau section 3.5. Pour des montants > $1000, **toujours** attendre `is_finalized: true` avant de créditer ou de payer.

### 6.5 Replay protection cross-chain (v0.8.0+)

Toute TX signée pour `pms-testnet-v1` est rejetée par `pms-mainnet-v1` et vice-versa. C'est intrinsèque au signing — l'attaquant ne peut même pas annoncer un faux network_id (le hash diffère). Détails dans [[validation-consensus]].

### 6.6 Encrypted vs plain TX

Par défaut, les TX entre user wallets sont **chiffrées** (X25519+AES-256-GCM). Le serveur PMS ne peut pas voir le contenu. Conséquences pour ton SaaS :

- **Webhook / multi-address SSE** ne peuvent pas surfacer le détail des TX chiffrées (`encrypted: true` dans le body, pas de `amount`/`asset_id`).
- **Pour décrypter**, tu dois utiliser `GET /v1/wallet/{address}/activity` ou `/activity/stream` avec la clé X25519 dérivée de la privée du user (cf. section 2.1 — `user_x25519_sk`).
- **Mint blocks** (faucet, admin mint) sont en clair → visibles partout.

---

## 7. Erreurs courantes

Tous les endpoints v0.8.0 retournent un body uniforme `{ "code": NNNN, "message": "..." }`. Détails complets dans [[../api/error-codes]].

| Code | Sens | Action côté SaaS |
|------|------|-------------------|
| `1001` | Auth manquante | Ajoute le header `Authorization: Bearer $TOKEN` |
| `1002` | Auth invalide | Vérifie ton `PMS_ADMIN_TOKEN` |
| `1010` | TX chiffrée — pas de décryptage côté serveur | Utilise `/v1/wallet/{addr}/activity` avec clé X25519 |
| `2010` | Adresse invalide (checksum/format/HRP) | Re-vérifie l'input user |
| `2020` | Montant invalide | Vérifie le format string décimal, pas de négatif |
| `3001` | Solde insuffisant | Affiche un message "fonds insuffisants" |
| `3010` | UTXO déjà dépensé (race) | Retry avec un fresh `prepare_tx` |
| `3040` | Resource not found (block_id, subscription, etc.) | 404 logique côté SaaS |
| `4001` | Signature mismatch | Probablement mauvais `network_id` au signing — vérifie ton SDK |
| `5010` | Gas pool empty (custom ledger) | Recharge avec `/admin/gas-pool/deposit` |
| `503 read_only` | Engine en read-only mode | Retry après `Retry-After: 30` |

---

## 8. Pattern de déploiement

### 8.1 Architecture recommandée

```
┌─────────────────────────────────────────────────────┐
│ Ton SaaS                                            │
│ ┌─────────────┐    ┌──────────────┐    ┌─────────┐ │
│ │ Frontend    │───▶│ Backend API  │───▶│ Hot     │ │
│ │ (React/Mob) │    │ + DB         │    │ wallet  │ │
│ └─────────────┘    │ + Watcher    │    └────┬────┘ │
│                    └──────┬───────┘         │      │
│                           ▲                 │      │
│                           │ webhook         │      │
└───────────────────────────┼─────────────────┼──────┘
                            │                 │
                            │                 ▼
              ┌─────────────┴────────────────────────┐
              │ PMS engine (testnet.pms-network.com)│
              │  - HD wallet derivation              │
              │  - /v1/blocks/range                  │
              │  - /v1/wallet/send-simple            │
              │  - /admin/webhooks delivery          │
              └──────────────────────────────────────┘
                              │
                              ▼ cold storage
              ┌──────────────────────────────────────┐
              │ Master mnemonic .enc (HSM/sealed)    │
              └──────────────────────────────────────┘
```

### 8.2 Variables d'environnement

```env
# Node PMS
PMS_NODE_URL=https://testnet.pms-network.com
PMS_ADMIN_TOKEN=<...>
PMS_NETWORK_ID=pms-testnet-v1                    # critique — change en pms-mainnet-v1 en prod

# HD wallet
PMS_MASTER_MNEMONIC_FILE=/secrets/mnemonic.enc   # chiffré at-rest
PMS_HD_ACCOUNT=0                                 # ton account BIP44 (multi-tenant : 1, 2, ...)

# Hot wallet (treasury, dérivé de master[m/44'/PMS'/99'/0/0])
PMS_HOT_WALLET_KEY_FILE=/secrets/hot-wallet.enc

# Webhook
PMS_WEBHOOK_SECRET=<32 hex chars retournés par /admin/webhooks>
PMS_WEBHOOK_CALLBACK_URL=https://saas.example.com/pms-webhook

# Sécurité
PMS_CONFIRMATIONS_LOW=1                          # < $10
PMS_CONFIRMATIONS_MED=3                          # $10 – $100
PMS_CONFIRMATIONS_HIGH=6                         # $100 – $1 000
PMS_CONFIRMATIONS_MAX=12                         # > $1 000  (équivalent : is_finalized=true)
PMS_HOT_WALLET_MAX_USD=5000                      # alerte si hot wallet > ce seuil
```

### 8.3 Heartbeat + recovery

Toutes les minutes :

```ts
const status = await fetch("https://testnet.pms-network.com/v1/dag/status").then(r => r.json());
// { tip_count, total_blocks, latest_block_ts_ms, network_id, api_version, dag_version }

// Vérification network_id — défense en profondeur contre une mauvaise config
assert(status.network_id === process.env.PMS_NETWORK_ID, "wrong network!");

// Si le total_blocks n'a pas bougé depuis 5 min ET que tu n'as pas reçu de webhook
// → déclenche un /v1/blocks/range scan en mode recovery pour rattraper.

// Si l'engine a restart (les uptimes Prometheus le diront), re-register tes webhooks
// (in-memory, perdus au restart).
```

---

## 9. Checklist de validation avant production

```
PROTOCOLE PMS — vérifications techniques
─────────────────────────────────────────
[ ] HD wallet (BIP32/BIP44) implémenté et testé sur 1000 adresses uniques
[ ] master mnemonic en cold storage (HSM, fichier .enc, hardware wallet)
[ ] hot wallet renfloué manuellement, jamais > 5% des réserves
[ ] vérification de checksum d'adresse côté serveur (decode_address)
[ ] network_id binding dans le signing (replay protection v0.8.0)

DÉTECTION DES DÉPÔTS
────────────────────
[ ] Webhook OU SSE configuré (idéal : les deux)
[ ] HMAC verification en constant-time
[ ] Polling /v1/blocks/range comme filet de sécurité
[ ] Idempotence via UNIQUE (block_id, address) en DB
[ ] Vérification de finalité (depth ou is_finalized) avant crédit

PAYOUTS
───────
[ ] estimate-fee appelé AVANT que le user signe
[ ] Validation d'adresse destination AVANT signing
[ ] Tracking du block_id dans la table payouts
[ ] Confirmation de finalité avant marquer comme 'paid'

SÉCURITÉ
────────
[ ] Clé privée hot wallet chiffrée at-rest (zeroize après usage)
[ ] PMS_ADMIN_TOKEN dans secrets manager (jamais en git)
[ ] Permissions strictes sur les fichiers .enc (0600)
[ ] Audit log de tous les payouts > $X

OBSERVABILITÉ
─────────────
[ ] Heartbeat /v1/dag/status toutes les minutes
[ ] Alerte si latest_block_ts_ms n'a pas bougé en 5 min
[ ] Dashboard Prometheus/Grafana sur les counters webhook (success/failed)
[ ] Test E2E mensuel : déposer 0.01 PMS, attendre crédit, payer 0.005, attendre confirmation
```

---

## 10. Tests

### 10.1 Test 1 — Dépôt basique

1. Génère une adresse de dépôt pour `user_test_001` via BIP44
2. Envoie 10 PMS depuis un wallet externe (faucet ou autre user)
3. Attends `depth >= 3` ou `is_finalized: true`
4. Vérifie que ton DB a un crédit de 10 PMS pour `user_test_001`
5. Vérifie que `block_id` est marqué `status='credited'`

**PASS** si : crédit exact, une seule fois, après confirmation.

### 10.2 Test 2 — Anti double-crédit (idempotence)

1. Rejoue manuellement le webhook avec le même `block_id` 5 fois
2. Vérifie que le balance user n'augmente PAS au-delà du premier crédit

**PASS** si : INSERT échoue avec `ON CONFLICT DO NOTHING` sur les retries.

### 10.3 Test 3 — Replay cross-chain

1. Sur testnet, signe une TX (récupère le raw signed TX)
2. Tente de broadcaster la même TX signée sur mainnet
3. Vérifie que mainnet renvoie `4001 SignatureMismatch`

**PASS** si : la TX testnet est bien rejetée sur mainnet.

### 10.4 Test 4 — Payout créateur

1. Déclenche un payout de 25 PMS vers wallet créateur
2. Vérifie le `block_id` dans `/v1/transaction/{id}` → `to` correct, `amount` correct
3. Attends finalité
4. Vérifie que le `payout` passe en `status='paid'` dans ta DB

**PASS** si : transaction confirmée, ledger cohérent.

### 10.5 Test 5 — Recovery après crash watcher

1. Stoppe ton watcher au milieu d'une session
2. Génère 10 dépôts pendant qu'il est down
3. Redémarre le watcher
4. Lance le scan `/v1/blocks/range` à partir du dernier `ts_ms` connu
5. Vérifie que les 10 dépôts sont rattrapés

**PASS** si : zéro miss, zéro double-crédit (idempotence en DB).

---

## 11. Ressources

- [[hd-wallet-bip32]] — détails dérivation BIP32/BIP39/BIP44
- [[validation-consensus]] — replay protection cross-chain (Phase 1)
- [[payment-rail-integration]] — RPC endpoints (Phase 3)
- [[webhook-delivery]] — multi-SSE + webhooks (Phase 4)
- [[../api/error-codes]] — grille complète des codes d'erreur
- [[server-engine]] — architecture du node PMS
- [[smart-contracts]] — `OnTransfer` pour les transfer fees auto
- [[multi-ledger]] — création de ledgers custom

---

## Hors scope (Phase suivante)

- **Persistance des webhooks** (Phase 4.5) — actuellement perdus au restart, ré-enregistrement manuel
- **Vrai watch-only** (xpub-only sans master en RAM) — Phase 2.5, exige redesign d'adresse
- **Hardware wallet plugin** Ledger / Trezor — Phase ultérieure dédiée
- **SLIP-44 coin type officiel** — actuellement private-use range, migration future
- **Stripe / fiat on-ramp** — intégration externe, hors moteur PMS
