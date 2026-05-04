---
tags: [feature, api, integration]
created: 2026-05-04
updated: 2026-05-04
version: v0.8.0
---

# Payment-rail RPC (SaaS integration)

## Résumé

Quatre endpoints REST conçus pour qu'un SaaS streaming white-label intègre PMS comme moyen de paiement (achat de tokens, abonnements, payouts créateurs) sans hacker autour de l'API existante. Conçus pour le pattern "watcher polling + webhook futur" de [PROTOCOL.md](https://github.com/empereur-rouge/pms-network-server) — détecter les dépôts entrants, afficher les fees avant signature, scanner après crash.

| Endpoint | Méthode | But |
|----------|---------|-----|
| `/v1/dag/status` | GET | Snapshot DAG (équivalent `get_block_height` linéaire) |
| `/v1/estimate-fee` | POST | Pure compute — affiche le fee total avant signature |
| `/v1/transaction/{block_id}` | GET | Lookup unifié (parsé, pas de raw block JSON) |
| `/v1/blocks/range` | GET | Scan paginé time-ordered, idempotent |

Tous **read-only**, **pas d'auth** (publics). Pour les routes admin / write voir [[server-engine]] et [[api-key-authentication]].

## `GET /v1/dag/status`

```jsonc
GET /v1/dag/status
→ 200 {
  "tip_count": 5,
  "last_milestone": "abc123..." | null,
  "total_blocks": 12345678,
  "latest_block_ts_ms": 1777917396921,
  "network_id": "pms-mainnet-v1",
  "api_version": 11,
  "dag_version": "2.0.0"
}
```

`total_blocks` + `latest_block_ts_ms` = curseur monotone. Le watcher SaaS poll cet endpoint pour détecter de l'activité sans charger les blocks réels.

## `POST /v1/estimate-fee`

```jsonc
POST /v1/estimate-fee
{ "amount": "100", "asset_id": null }
→ 200 {
  "fee": "3.0000001",
  "transfer_fee": "0",
  "total": "103.0000001",
  "fee_breakdown": []
}
```

`fee` = `FeePolicy::compute_fee` (linéaire ou par paliers selon config). `transfer_fee` = somme des `OnTransfer` smart contracts qui matchent (cf. [[smart-contracts]]). `total = amount + fee + transfer_fee`. `fee_breakdown` détaille chaque contrat triggered (utile pour la UI).

Erreurs : `400 code=2020` pour montant négatif ou malformé.

## `GET /v1/transaction/{block_id}`

```jsonc
GET /v1/transaction/e8dbd3d80ef58436981bebd2ec88809403c15d993492b1b760a315f1ffa55458
→ 200 {
  "tx_hash": "<block_id>",            // 1 TX = 1 block en PMS
  "block_id": "<block_id>",
  "from": "8e1ug0qne..." | null,      // résolu via input UTXO parent
  "to": "8e130fx2q..." | null,
  "amount": "42",
  "asset_id": null,
  "fee": "0",
  "timestamp_ms": 1777917397695,
  "is_finalized": false,
  "depth": 0,                          // nb descendants distincts (capé à 64)
  "status": "pending" | "confirmed" | "finalized",
  "inputs": [{txid, index, address, amount, asset_id}],
  "outputs": [{address, amount, asset_id}]
}
```

Supporte 3 familles de payload :
- **`TxUtxo`** plain (legacy / dev) — détail complet
- **`Mint`** (faucet, admin mint) — `from = null`, `inputs = []`
- **`Reward`** (fee distribution, block reward) — `from = null`, outputs `fee_outputs ++ reward_outputs`

Erreurs spéciales :
- **`Encrypted` payload → 403 code=1010** avec message pointant vers `GET /v1/wallet/{address}/activity` (qui décrypte avec la clé du destinataire stockée). Le serveur ne décrypte JAMAIS sans clé.
- **block_id inconnu → 404 code=3040**

## `GET /v1/blocks/range`

```jsonc
GET /v1/blocks/range?limit=100&after_ts=...&after_id=...
→ 200 {
  "blocks": [{ "id": "...", "ts_ms": 1777917396270 }, ...],
  "next_cursor": {
    "ts_ms": 1777917396103,
    "id": "...",
    "has_more": true
  } | null
}
```

Most-recent-first. `limit` borné à 1000 (défaut 100). Curseur **exclusif** : passer `cursor.ts_ms` comme `after_ts` et `cursor.id` comme `after_id` retourne strictement plus anciens. **Idempotent** : la même requête deux fois retourne les mêmes blocks (clé pour rattraper après crash watcher sans double-traitement).

S'appuie sur le CF `by_time` existant (cf. [[storage-rocksdb]]).

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-server` | [`api_fn/dag.rs`](../../crates/pms-server/src/api_fn/dag.rs) | `get_dag_status` |
| `pms-server` | [`api_fn/estimate_fee.rs`](../../crates/pms-server/src/api_fn/estimate_fee.rs) | `estimate_fee` (nouveau module) |
| `pms-server` | [`api_fn/transaction_lookup.rs`](../../crates/pms-server/src/api_fn/transaction_lookup.rs) | `get_transaction_by_block_id` (nouveau module) |
| `pms-server` | [`api_fn/blocks.rs`](../../crates/pms-server/src/api_fn/blocks.rs) | `blocks_range` (existant + nouveau handler) |
| `pms-server` | [`api/routes.rs`](../../crates/pms-server/src/api/routes.rs) | Wire routes dans `dag_routes` |
| `pms-interface` | [`net_adapter.rs`](../../crates/pms-interface/src/net_adapter.rs) | `count_descendants` / `is_finalized` / `last_milestone` ajoutés au trait |
| `pms-core` | [`net_adapter/mod.rs`](../../crates/pms-core/src/net_adapter/mod.rs) | Impl trait : délégué à `self.dag.*` |

## Tests d'intégration (sandbox)

Dans [`crates/pms-server/tests/dag_sandbox.rs`](../../crates/pms-server/tests/dag_sandbox.rs). Run :

```bash
cargo test --release -p pms-server --test dag_sandbox -- --ignored --nocapture --test-threads=1 \
  test_dag_status_endpoint test_estimate_fee_endpoint \
  test_transaction_lookup_endpoint test_blocks_range_endpoint
```

| Test | Vérifie |
|------|---------|
| `test_dag_status_endpoint` | `total_blocks` croît après mints, `latest_block_ts_ms` set, version fields présents |
| `test_estimate_fee_endpoint` | `total = amount + fee + transfer_fee` ; rejet 400 sur montant négatif/malformé |
| `test_transaction_lookup_endpoint` | Mint lookup OK, encrypted → 403 code=1010, unknown → 404 code=3040 |
| `test_blocks_range_endpoint` | Pagination idempotente, page 2 disjointe de page 1 (curseur exclusif) |

## Limitations connues

- **`tx_hash` = `block_id` par convention PMS** (1 TX = 1 block). Le hash de signing du contenu canonique de la TX est interne et n'est pas exposé — il sert uniquement à la vérification de signature (cf. [[validation-consensus]] Phase 1 cross-chain replay).
- **Pas de webhook delivery** (Phase 4 prévue). Le watcher poll `/v1/blocks/range` ou utilise le SSE existant `/v1/wallet/{addr}/activity/stream`.
- **`depth` capé à 64** descendants — assez pour les tiers de finalité du PROTOCOL.md (`< $10` : 1 conf ; `< $100` : 3 ; `< $1000` : 6 ; `> $1000` : 12).

## Interactions

- [[validation-consensus]] — Phase 1 cross-chain replay protection : `network_id` dans `dag/status` reflète la chaîne courante et doit matcher ce que les SDK utilisent pour signer.
- [[hd-wallet-bip32]] — Pattern recommandé : la SaaS dérive une adresse de dépôt par user via BIP44, puis poll `/v1/blocks/range` + lookup chaque block reçu via `/v1/transaction/{id}` pour détecter les dépôts.
- [[activity-system]] — Pour les wallets dont la SaaS a la clé X25519, `GET /v1/wallet/{address}/activity` reste plus efficace (decrypt côté serveur, indexé par adresse).
- [[storage-rocksdb]] — `blocks/range` réutilise le CF `by_time` (existant) et `id2ts` pour le timestamp lookup.
