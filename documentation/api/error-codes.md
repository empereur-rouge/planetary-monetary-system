---
tags: [api, reference, security]
created: 2026-05-01
updated: 2026-05-01
version: v0.7.24
---

# API Error Codes

**Source de vérité : [crates/pms-server/src/api_error.rs](../../crates/pms-server/src/api_error.rs)**.

Chaque erreur HTTP renvoyée par l'API porte un **code numérique stable**
(`code: NNNN` dans le body JSON) et un **message public vague** pour les
catégories sensibles (anti-énumération). Le détail interne précis (adresse,
montant, raison de la failed signature, ...) est loggé via `tracing::warn!`
ou `tracing::error!` avec `target: "api_error"` — visible côté opérateur,
**jamais** côté client.

## Wire format

```json
{
  "code": 3001,
  "message": "Operation failed"
}
```

Certains codes ajoutent des champs additionnels pour ergonomie SDK :

| Code | Champs additionnels |
|------|---------------------|
| `1020` | `error: "read_only"`, `reason: "memory\|disk\|rocksdb\|manual"`, `retry_after_seconds: 30` (+ header `Retry-After: 30`) |
| `1040` | `scope_required: "<scope>"`, `your_scopes: [...]` |

## Catégories

| Range | Catégorie | Verbosité publique |
|-------|-----------|---------------------|
| `1xxx` | Auth / authz | **Vague** — ne pas révéler quel check a failed |
| `2xxx` | Validation requête | **Spécifique** — utile aux clients légitimes, no info leak |
| `3xxx` | État / business logic | **Vague** — anti-enumeration des balances / UTXOs / état |
| `4xxx` | Crypto / sécurité | **Vague** — anti-enumeration des pubkeys / nonces |
| `5xxx` | Resource / quota | **Vague** — sauf rate limit qui peut être explicite |
| `9xxx` | Internal | **Vague** — jamais de stack trace / RocksDB error |

## Grille complète

### 1xxx — Auth / authz

| Code | HTTP | Variant | Message public | Détail interne (logs) |
|------|------|---------|----------------|------------------------|
| `1001` | 401 | `MissingAuth` | `Authentication required` | `no Authorization / X-Admin-Token / X-API-Key header` |
| `1002` | 401 | `InvalidAuth` | `Authentication failed` | `token comparison failed` |
| `1010` | 403 | `AdminRequired` | `Insufficient privileges` | `user-scope token presented to admin route` |
| `1020` | 503 | `ReadOnly { reason }` | `Service temporarily unavailable (<reason>): retry in 30s` | `read-only mode armed (reason: <r>)` |
| `1030` | 403 | `IpNotAllowed { ip }` | `Insufficient privileges` | `IP not in admin allowlist: <ip>` |
| `1040` | 403 | `InsufficientScope { required, granted }` | `Insufficient permissions (required: <scope>)` | `insufficient scope: required=<r> granted=<g>` |

### 2xxx — Validation (spécifique OK)

| Code | HTTP | Variant | Message public | Détail interne |
|------|------|---------|----------------|-----------------|
| `2001` | 400 | `MalformedJson(reason)` | `Malformed JSON: <reason>` | (reason) |
| `2010` | 400 | `InvalidAddress { addr }` | `Invalid address format` | `address=<addr>` |
| `2020` | 400 | `InvalidAmount { reason }` | `Invalid amount: <reason>` | (reason) |
| `2030` | 400 | `InvalidField { field, reason }` | `Invalid field '<field>': <reason>` | `field=<f> reason=<r>` |
| `2040` | 413 | `TooLarge { kind, limit }` | `<kind> exceeds limit of <limit> bytes` | `<kind> > <limit> bytes` |

> Note : `InvalidAddress` est un cas borderline. Le champ `addr` n'est PAS
> dans le message public (anti-enumeration des adresses) mais l'opérateur
> le voit dans le log interne pour debug.

### 3xxx — État / business (vague)

| Code | HTTP | Variant | Message public | Détail interne |
|------|------|---------|----------------|-----------------|
| `3001` | 422 | `InsufficientBalance` | `Operation failed` | `insufficient balance: addr=<a> asset=<id> requested=<r> available=<av>` |
| `3010` | 422 | `AlreadySpent { txid, index }` | `Operation failed` | `output already spent: <txid>#<i>` |
| `3020` | 404 | `UnknownLedger(id)` | `Resource not found` | `unknown ledger: <id>` |
| `3030` | 422 | `ContractDisabled(id)` | `Operation unavailable` | `contract disabled: <id>` |
| `3040` | 404 | `NotFound { kind, id }` | `<kind> not found` | `<kind> not found: <id>` |
| `3050` | 409 | `AlreadyExists { kind, id }` | `<kind> already exists` | `<kind> already exists: <id>` |
| `3060` | 422 | `AddressFrozen(addr)` | `Operation forbidden` | `address frozen: <addr>` |
| `3070` | 409 | `Conflict(reason)` | `Operation conflict` | (reason) |
| `3071` | 409 | `GovernanceRejected { reason }` | (reason, **surfacée**) | (reason) |

> **Pourquoi `3071 GovernanceRejected` surface sa raison** (contrairement aux
> autres `3xxx` vagues) : l'enact/cancel de gouvernance est une surface
> **opérateur authentifiée**, et les raisons de rejet (timelock non écoulé,
> statut non-pending, id inconnu, doublon) ne portent **aucun secret financier**
> — uniquement des timestamps et des noms de statut. L'opérateur DOIT savoir
> *pourquoi* son action a été refusée. Tout chemin financier/crypto reste vague.

> **Pourquoi `InsufficientBalance` est vague** : un attaquant qui sonde une
> adresse pourrait déduire les balances en envoyant des transferts à
> différents montants ("3001 → balance < 1000 ; 200 → balance ≥ 1000"). Avec
> "Operation failed" il n'apprend rien de plus que "ça n'a pas marché".

### 4xxx — Crypto / sécurité (vague, anti-enumeration)

| Code | HTTP | Variant | Message public | Détail interne |
|------|------|---------|----------------|-----------------|
| `4001` | 401 | `SignatureMismatch { reason }` | `Authentication failed` | (reason — quelle entrée, quel pubkey, quelle courbe) |
| `4002` | 401 | `ReplayDetected { reason }` | `Authentication failed` | (reason — block_id déjà persisté, nonce reuse) |
| `4010` | 400 | `CryptoFailure { reason }` | `Invalid request` | (reason — X25519 / AES-GCM envelope corruption) |
| `4020` | 401 | `AuthorizationSignatureInvalid { reason }` | `Authentication failed` | (reason — coordinator/treasury master sig fail) |

> **Pourquoi 401 et pas 400** sur `SignatureMismatch` : un attaquant ne doit
> pas pouvoir distinguer "mon header auth est OK mais ma signature de bloc
> est cassée" de "mon header auth est faux". Même status, message similaire.

### 5xxx — Resource / quota

| Code | HTTP | Variant | Message public | Détail interne |
|------|------|---------|----------------|-----------------|
| `5001` | 429 | `RateLimited` | `Rate limit exceeded` | `rate limit exceeded` |
| `5010` | 503 | `GasPoolEmpty(ledger_id)` | `Service temporarily unavailable` | `gas pool empty for ledger: <id>` |
| `5020` | 503 | `SubscriptionInactive(ledger_id)` | `Subscription required` | `subscription inactive: <id>` |
| `5030` | 503 | `EmissionBudgetExhausted{reason}` | `Emission budget exhausted for this period` | `voie=<v> requested=<x> remaining=<y> budget=<z>` (montants jamais publics) |
| `5031` | 503 | `MintDisabled` | `Native mint is disabled` | `native PMS mint disabled by governance (mint_enabled=false)` — kill-switch d'émission, distinct du 5030 (épuisement, récupère à l'epoch suivant) : halt délibéré jusqu'à réactivation par la gouvernance |

### 9xxx — Internal (jamais explicite)

| Code | HTTP | Variant | Message public | Détail interne |
|------|------|---------|----------------|-----------------|
| `9001` | 500 | `StorageError { reason }` | `Internal error` | (RocksDB error verbatim) |
| `9002` | 500 | `ConsensusError { reason }` | `Internal error` | (validation / DAG engine error) |
| `9999` | 500 | `Internal { reason }` | `Internal error` | (catch-all, opérateur grep ces logs pour identifier les handlers à migrer) |

## Métriques opérateur

- `pms_api_errors_total{code="NNNN"}` — counter par code. Cardinalité bornée
  à ~30 codes, safe pour Prometheus.
- `pms_admin_auth_failures_total{reason}` — legacy, kept for dashboards.
- `pms_read_only_rejections_total{reason}` — counter de write requests
  rejetées avec 1020.

### Alertes recommandées

```promql
# Tout 9xxx > 0 = handler non migré ou bug réel — investiguer
rate(pms_api_errors_total{code=~"9..."}[5m]) > 0

# Sustained crypto / replay = brute force ou client mal configuré
rate(pms_api_errors_total{code=~"4..."}[5m]) > 1

# Sustained read-only = pression mémoire / disque / RocksDB chronique
pms_engine_read_only == 1
```

## Migration progressive

**v0.7.24 (état actuel)** : framework + 4 sites migrés
- `require_writable` middleware → `1020 ReadOnly`
- `require_local_or_admin` middleware → `1001 / 1002 / 1030`
- `require_admin_token` middleware → `1001 / 1002`
- `require_api_key` middleware → `1001 / 1002 / 1040 InsufficientScope`

**Prochaines vagues prioritaires** (high-value financial paths) :

1. `wallet_send_simple` — codes attendus : `2010 InvalidAddress`, `2020 InvalidAmount`, `3001 InsufficientBalance`, `4001 SignatureMismatch`, `3060 AddressFrozen`
2. `wallet_send_tx` — idem
3. `prepare_tx` — idem (sauf SignatureMismatch — c'est un draft)
4. `nft_mint` / `nft_burn` — `3040 NotFound`, `3001 InsufficientBalance`, `4001 SignatureMismatch`
5. `admin_mint_token` / `admin_create_token` — `3050 AlreadyExists`, `4020 AuthorizationSignatureInvalid`
6. `submit_block` — `4001 SignatureMismatch`, `4002 ReplayDetected`, `9002 ConsensusError`
7. `compliance/freeze`, `seize`, `reverse` — `4020 AuthorizationSignatureInvalid`, `3010 AlreadySpent`

## Règle d'ajout

1. Ajouter une variant à `ApiError` dans
   [crates/pms-server/src/api_error.rs](../../crates/pms-server/src/api_error.rs).
2. L'inscrire dans `code()`, `http_status()`, `public_message()`,
   `internal_detail()` — les 4 match sont exhaustifs, le compilateur
   vous force à les remplir.
3. L'ajouter au test `codes_are_unique` (qui catch les doublons).
4. Mettre à jour cette fiche.
5. Si la variant transporte des champs sensibles, ajouter un cas dans
   `public_message_never_leaks_internal_detail` pour vérifier qu'aucun
   substring du `internal_detail` n'apparaît dans le `public_message`.
