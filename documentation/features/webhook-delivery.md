---
tags: [feature, api, integration]
created: 2026-05-04
updated: 2026-05-04
version: v0.8.0
---

# Watcher API — Multi-address SSE + Webhook delivery

## Résumé

Deux mécanismes complémentaires pour qu'un SaaS détecte de l'activité sur N adresses sans tenir N connexions SSE individuelles :

1. **Multi-address SSE** (`GET /v1/activity/stream?addresses=a,b,c`) — un seul stream watch jusqu'à 1000 adresses. Idéal pour les serveurs persistents (Node.js, Rust, Go) qui peuvent garder une connexion ouverte.
2. **Webhook subscription** (`POST /admin/webhooks` + delivery worker) — le SaaS enregistre un `callback_url` et reçoit des POST signés HMAC-SHA256 à chaque event matchant. Idéal pour les architectures serverless (Lambda, Cloud Functions) qui ne peuvent pas garder un stream ouvert.

Les deux s'appuient sur le même `pms_event::EventBus` interne et le même set de `involved_addresses` extrait par `persist_block`.

## Multi-address SSE

```jsonc
GET /v1/activity/stream?addresses=8e1addr_a,8e1addr_b,8e1addr_c
→ 200 (text/event-stream)

event: activity
data: {"block_id":"...","ts_ms":1777920321494,"activity_type":"mint","direction":"in","amount":"10","payload":{...,"address":"8e1addr_a"}}

event: activity
data: {"block_id":"...","ts_ms":1777920321498,"activity_type":"mint","direction":"in","amount":"10","payload":{...,"address":"8e1addr_b"}}
```

- Cap : `MAX_ADDRESSES_PER_STREAM = 1000`. >1000 → 400.
- Encrypted payloads → événement `activity_type: "encrypted"` avec juste l'adresse matchée. Le SaaS upgrade vers `GET /v1/wallet/{address}/activity/stream` (single-address) avec la clé X25519 si décryptage nécessaire.
- Filtre `?type=mint,send,recv` optionnel (mêmes valeurs que le single-address stream).

## Webhook subscription

### `POST /admin/webhooks`

```jsonc
POST /admin/webhooks
Authorization: Bearer <admin_token>
{
  "addresses": ["8e1addr_user_001", "8e1addr_user_002"],
  "callback_url": "https://saas.example.com/pms-webhook",
  "secret": "optional_caller_supplied"   // server generates 32 random bytes if omitted
}
→ 201 {
  "subscription_id": "<32 hex chars>",
  "secret": "<32 random hex chars OR caller-supplied>",   // RETURNED ONCE
  "addresses_count": 2
}
```

**Le secret n'est exposé qu'à la création.** `GET /admin/webhooks` ne le re-renvoie jamais (`#[serde(skip_serializing)]`). Le SaaS doit le persister immédiatement.

### `GET /admin/webhooks`

```jsonc
GET /admin/webhooks
→ 200 [
  {
    "subscription_id": "...",
    "addresses": ["8e1addr_user_001", ...],
    "callback_url": "https://saas.example.com/pms-webhook",
    "created_ts_ms": 1777920321494,
    "success_count": 142,
    "failed_count": 3
  }
]
```

`success_count` / `failed_count` sont des compteurs cumulés depuis le boot (perdus au restart). Permettent à l'opérateur de monitorer la santé du callback.

### `DELETE /admin/webhooks/{id}`

```jsonc
DELETE /admin/webhooks/abc123...
→ 200 { "subscription_id": "abc123...", "status": "unsubscribed" }
→ 404 code=3040 si l'id n'existe pas
```

## Delivery contract

Pour chaque `BlockPersisted` event dont l'`involved_addresses` ∩ `subscription.addresses` est non-vide, **un POST par adresse matchée** :

```http
POST <callback_url>
Content-Type: application/json
X-PMS-Signature: sha256=<hex(hmac_sha256(secret, body))>
X-PMS-Subscription-Id: <subscription_id>
X-PMS-Delivery-Id: <16 hex random — unique per attempt>
X-PMS-Delivery-Attempt: <1..=5>

{
  "subscription_id": "abc123...",
  "block_id": "f8d9c5b3...",
  "address": "8e1addr_user_001",
  "ts_ms": 1777920321494,
  "encrypted": false,
  "ledger_id": "main"
}
```

Le SaaS doit splitter `X-PMS-Signature` sur `=` (algo, hex), recompute `HMAC-SHA256(secret, raw_body_bytes)`, et comparer en temps constant. Le préfixe `sha256=` permet une rotation future d'algorithme (sha512, ed25519) sans casser les clients existants — convention identique à Stripe / GitHub. Toute lib crypto standard (Node.js `crypto.createHmac`, Python `hmac`, Go `crypto/hmac`) reproduit le même hex.

### Retry / failure

- 2xx → `success_count++`, fin
- 5xx / network error / non-2xx → retry après `2^(attempt-1)` secondes (1, 2, 4, 8, 16 s)
- `MAX_DELIVERY_ATTEMPTS = 5` puis `failed_count++` + `tracing::error!`. La SaaS doit alors récupérer via `GET /v1/blocks/range` (cf. [[payment-rail-integration]]) après détection (par exemple via heartbeat).

### encrypted = true

Un block avec payload chiffré : le serveur ne peut pas décrypter, donc émet juste la notification de matching avec `encrypted: true`. La SaaS suit avec `GET /v1/wallet/{address}/activity` qui décrypte avec la clé du destinataire stockée côté serveur (cf. [[wallet-encryption]]).

## Storage in-memory

Les subscriptions vivent en `DashMap` (`WebhookStore`) — perdues au restart. Ce design choice évite :
- Un nouveau column family RocksDB
- Un bump `CURRENT_VER` schema (migration)
- Des questions de sérialisation (la `secret` ne devrait jamais toucher disk en clair)

Le pattern attendu :
1. SaaS boot → recharge ses subscriptions depuis SA propre DB
2. SaaS POST `/admin/webhooks` (idempotence : génère un nouveau `subscription_id` à chaque fois)
3. SaaS heartbeat sur `/v1/version` toutes les minutes ; si l'engine restart (uptime reset), re-subscribe

Persistance possible en Phase 4.5 si demande réelle — ajouterait un CF `webhooks` chiffré (le secret ne peut pas être stocké en clair sur disk).

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-server` | [`api_fn/activity/stream.rs`](../../crates/pms-server/src/api_fn/activity/stream.rs) | `stream_multi_address_activity` + `MAX_ADDRESSES_PER_STREAM` |
| `pms-server` | [`api_fn/webhooks.rs`](../../crates/pms-server/src/api_fn/webhooks.rs) | `WebhookStore`, `Subscription`, handlers, `run_delivery_loop`, `hmac_signature` |
| `pms-server` | [`api/state.rs`](../../crates/pms-server/src/api/state.rs) | `AppState.webhook_store` |
| `pms-server` | [`api/serve.rs`](../../crates/pms-server/src/api/serve.rs) | Init + spawn `run_delivery_loop` au boot |
| `pms-server` | [`api/routes.rs`](../../crates/pms-server/src/api/routes.rs) | Routes : SSE multi-address dans `activity_routes`, webhooks dans `admin_recovery` |

## Configuration

Aucune config TOML ajoutée. Constantes runtime dans `webhooks.rs` :

| Constante | Valeur | But |
|-----------|--------|-----|
| `MAX_ADDRESSES_PER_STREAM` | 1000 | Cap multi-SSE |
| `MAX_ADDRESSES_PER_SUBSCRIPTION` | 1000 | Cap webhook |
| `MAX_SUBSCRIPTIONS` | 10_000 | Cap global |
| `MAX_DELIVERY_ATTEMPTS` | 5 | Retry budget (≈31 s avant abandon) |
| `MAX_INFLIGHT_DELIVERIES` | 512 | Cap concurrent in-flight (anti-DoS slow callback) |

## Tests

Unit (run via `cargo test -p pms-server --lib api_fn::webhooks`) :
- `hmac_signature_is_deterministic_and_distinguishes_inputs` — la signature reproduit le test vector + change avec secret/body
- `store_matching_returns_intersection_per_subscription` — l'intersection adresses-event × adresses-subscription est correcte

Sandbox (run via `cargo test --release -p pms-server --test dag_sandbox -- --ignored --nocapture --test-threads=1 <test_name>`) :
- `test_multi_address_sse_filters_correctly` — 2 adresses watched + 1 unwatched → exactement 2 events. 1001 adresses → 400.
- `test_webhook_subscribe_list_unsubscribe_roundtrip` — CRUD complet, secret jamais re-exposé après création, 404 sur re-delete.

## Hors scope

- **Persistance des webhooks** — Phase 4.5 si demande réelle. Le secret nécessite chiffrement at-rest.
- **Test E2E delivery** avec un vrai HTTP receiver capturé — la roadmap Phase 4.5 ajoutera un mock server in-process pour vérifier le HMAC sur le wire complet. L'invariant `hmac_signature` est déjà testé en unit.
- **Replay protection sur les deliveries** (idempotence côté receiver) — le SaaS doit dédupliquer via `delivery_id` ou `block_id + address`.
- **Subscription update** (PATCH `/admin/webhooks/{id}`) — pas implémenté. Le SaaS unsubscribe + re-subscribe.

## Interactions

- [[payment-rail-integration]] — Phase 3 RPC endpoints (`/v1/blocks/range` notamment) sert de fallback quand la livraison webhook est cassée.
- [[event-system]] — `pms_event::EventBus` partagé : 1 event = 1 publication, N+M consumers (single-address SSE + multi-address SSE + delivery loop).
- [[activity-system]] — la classification (`classify_activity_sync`) est partagée entre les 2 streams SSE.
- [[wallet-encryption]] — encrypted payloads → événement générique côté multi-SSE / webhook ; le SaaS upgrade vers `/v1/wallet/{addr}/activity` qui décrypte avec la clé stockée.
