---
tags: [feature]
created: 2026-02-13
updated: 2026-03-13
version: v0.1.0
---

# Compliance (Gel, Saisie, Inversion)

## Résumé

Le module Compliance fournit des outils d'administration réglementaire pour le réseau PMS : gel d'adresses (freeze/unfreeze), saisie de fonds (seize) et inversion de transactions (reverse). Chaque action de compliance est enregistrée dans le DAG sous forme de bloc signé exclusivement par le Coordinateur, garantissant un audit trail immutable et vérifiable. Les adresses gelées sont bloquées en temps réel au niveau de `prepare_tx` (API) et de `persist_block` (consensus), empêchant toute transaction entrante ou sortante.

## Dates

| | Date |
|---|---|
| Créée | 2026-02-13 |
| Dernière mise à jour | 2026-03-13 |
| Version d'introduction | v0.1.0 |

**Historique des commits touchant le module Compliance :**

| Date | Commit | Description |
|------|--------|-------------|
| 2026-02-13 | `b207bed` | `feat: add bridge, compliance, cube, wallet-factory modules and multi-token enhancements` |
| 2026-03-01 | `3e59ea5` | `fix(fmt): apply cargo fmt to entire workspace` |
| 2026-03-01 | `6114467` | `fix(clippy): resolve all clippy warnings and enforce as CI hard failure` |
| 2026-03-13 | `4fd32c5` | `perf: 12 hot-path optimizations for TPS throughput (v0.2.3)` -- introduction du cache `frozen_set` DashSet |

## Configuration

### Authentification admin (TOML)

Toutes les routes `/admin/compliance/*` exigent un token admin Bearer. La configuration se fait dans le fichier TOML du nœud :

```toml
[auth]
admin_api_token = "env:PMS_ADMIN_TOKEN"   # Lit la variable d'environnement PMS_ADMIN_TOKEN
# OU
admin_api_token = "mon-token-secret"       # Valeur littérale
```

- Si `admin_api_token` n'est pas défini ou est `None`, toutes les requêtes admin sont autorisées (mode développement).
- La comparaison du token utilise `subtle::ConstantTimeEq` pour prévenir les attaques par timing.
- Le préfixe `env:` permet de charger la valeur depuis une variable d'environnement (recommandé en production).

### Restriction par IP (TOML)

```toml
[auth]
allowed_ips = ["192.168.1.0/24", "10.0.0.5/32"]
```

- Les routes admin sont protégées par un middleware IP en plus du token Bearer.
- Liste vide = autorise tout (dev). En production, configurer un whitelist strict.

### Adresse treasury pour les saisies

L'endpoint `/admin/compliance/seize` redirige les fonds saisis vers le treasury. L'adresse est résolue dans cet ordre de priorité :

1. `treasury_wallets.list[0]` (config hot-swap `TreasuryWallets`)
2. `fees.treasury_addresses[0]` (config TOML `[fees]`)
3. `admin.wallet_addresses[0]` (config TOML `[admin]`)

Si aucune adresse n'est trouvée, la saisie échoue avec `400 Bad Request`.

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-server` | `crates/pms-server/src/api_fn/compliance.rs` | Handlers des 7 endpoints API compliance (freeze, unfreeze, seize, reverse, frozen list, log, shadow balance) |
| `pms-server` | `crates/pms-server/src/api.rs` | Enregistrement des routes `/admin/compliance/*` dans le routeur Axum |
| `pms-server` | `crates/pms-server/src/api_fn/transaction.rs` | Enforcement du gel dans `prepare_tx` (vérification sender + recipient) |
| `pms-server` | `crates/pms-server/tests/compliance_test.rs` | Tests d'intégration (storage, enforcement, endpoints read-only, edge cases) |
| `pms-storage` | `crates/pms-storage/src/compliance_store.rs` | Trait `ComplianceStorage` (interface abstraite) |
| `pms-storage` | `crates/pms-storage/src/rocks_store/compliance_registry.rs` | Implémentation RocksDB du trait `ComplianceStorage` + structs `FrozenEntry`/`ComplianceLogEntry` |
| `pms-storage` | `crates/pms-storage/src/rocks_store/store.rs` | Déclaration des CFs `compliance_frozen`/`compliance_log`, cache `frozen_set: DashSet`, `load_frozen_cache()` |
| `pms-core` | `crates/pms-core/src/validations/check.rs` | Validation des blocs Freeze/Unfreeze/Seize/Reverse (signature Coordinateur obligatoire) |
| `pms-core` | `crates/pms-core/src/validations/apply.rs` | Application RAM des UTXOs pour Seize/Reverse (`mark_spent_ram`) |
| `pms-core` | `crates/pms-core/src/net_adapter.rs` | Application des opérations compliance dans `persist_block` (freeze/unfreeze registry, compliance log, freeze check sur TxUtxo/BridgeLock, deltas UTXO pour Seize/Reverse) |
| `pms-core` | `crates/pms-core/src/core_adapter.rs` | Résolution des UTXOs dépensés par Seize/Reverse dans le UTXO tracker |
| `pms-types-payload` | `crates/pms-types-payload/src/payload.rs` | Variants `PlainPayload::Freeze`, `Unfreeze`, `Seize`, `Reverse` |
| `pms-errors` | `crates/pms-errors/src/types.rs` | Types d'erreur `AddressFrozen(String)`, `OutputAlreadySpent { txid, index }` |
| `pms-wallet` | `crates/pms-wallet/src/history.rs` | Extraction d'adresses et filtrage d'historique pour les payloads compliance |
| `pms-storage` | `crates/pms-storage/src/helpers.rs` | Activity indexing : catégories `Compliance` et `Reverse`, mapping des paires adresse/activité |
| `pms-utils` | `crates/pms-utils/src/block_id.rs` | Labels de type de bloc pour Freeze/Unfreeze/Seize/Reverse |
| `pms-ledger` | `crates/pms-ledger/src/instance.rs` | Appel de `load_frozen_cache()` au bootstrap du ledger |

## Fonctions Clés

### Handlers API (`crates/pms-server/src/api_fn/compliance.rs`)

| Fonction | Description |
|----------|-------------|
| `is_admin_authorized()` | Vérifie le token Bearer admin avec comparaison constant-time (`subtle::ConstantTimeEq`) |
| `admin_freeze()` | Gèle une adresse : crée un bloc `Freeze` signé par le Coordinateur, persiste dans le DAG |
| `admin_unfreeze()` | Dégèle une adresse : crée un bloc `Unfreeze` avec référence au `freeze_block_id` original |
| `admin_seize()` | Saisit les UTXOs d'une adresse (tout ou sélection) : crée un bloc `Seize` transférant les fonds au treasury, met à jour le cache UTXO RAM |
| `admin_reverse()` | Inverse une transaction TxUtxo : vérifie que tous les outputs sont non-dépensés, reconstruit les UTXOs originaux par inspection de la chaîne, crée un bloc `Reverse` |
| `admin_list_frozen()` | Retourne la liste de toutes les adresses actuellement gelées |
| `admin_compliance_log()` | Retourne le journal d'audit complet de toutes les actions compliance |
| `admin_shadow_balance()` | Calcule et retourne les balances détaillées (par asset) de tous les comptes gelés |

### Storage (`crates/pms-storage/src/rocks_store/compliance_registry.rs`)

| Fonction | Description |
|----------|-------------|
| `is_frozen()` | Lookup O(1) dans le `frozen_set` DashSet (aucun I/O RocksDB sur le hot path) |
| `freeze_address()` | Écrit un `FrozenEntry` dans le CF `compliance_frozen`, insère dans `frozen_set`, log l'action dans `compliance_log` |
| `unfreeze_address()` | Supprime l'entrée du CF `compliance_frozen` et retire du `frozen_set` |
| `get_freeze_entry()` | Lecture directe dans le CF `compliance_frozen` par clé (adresse) |
| `list_frozen()` | Itère sur toutes les entrées du CF `compliance_frozen` |
| `log_compliance_action()` | Écrit un `ComplianceLogEntry` (action, block_id, target_address, details, timestamp_ms) dans le CF `compliance_log` |
| `list_compliance_log()` | Itère sur toutes les entrées du CF `compliance_log` |
| `load_frozen_cache()` | Charge toutes les adresses gelées du CF `compliance_frozen` dans le `frozen_set` DashSet au bootstrap. Après cet appel, `is_frozen()` ne touche jamais RocksDB. |

### Validation (`crates/pms-core/src/validations/check.rs`)

| Fonction | Description |
|----------|-------------|
| `require_coordinator_signature()` | Vérifie que le bloc est signé par le Coordinateur. Utilisée pour tous les payloads Compliance. |
| Validation `Freeze` | Exige signature Coordinateur + adresse non vide |
| Validation `Unfreeze` | Exige signature Coordinateur + adresse et `freeze_block_id` non vides |
| Validation `Seize` | Exige signature Coordinateur + inputs/outputs non vides + `from_address` non vide |
| Validation `Reverse` | Exige signature Coordinateur + `original_block_id`, inputs et outputs non vides |

### Enforcement dans `persist_block` (`crates/pms-core/src/net_adapter.rs`)

| Point d'enforcement | Description |
|---------------------|-------------|
| Section `1.compliance` | Applique les opérations du registre : `freeze_address()`, `unfreeze_address()`, `log_compliance_action()` pour Seize et Reverse |
| Section `4.compliance` | Rejette les blocs `TxUtxo` et `BridgeLock` impliquant des adresses gelées (sender ou recipient) |
| Section UTXO delta | Calcule les deltas UTXO pour `Seize` (dépense inputs, crée outputs treasury) et `Reverse` (dépense outputs originaux, recrée UTXOs pour senders originaux) |

## Endpoints API

| Méthode | Path | Description |
|---------|------|-------------|
| POST | `/admin/compliance/freeze` | Gèle une adresse (bloque envoi et réception) |
| POST | `/admin/compliance/unfreeze` | Dégèle une adresse précédemment gelée |
| POST | `/admin/compliance/seize` | Saisit les UTXOs d'une adresse vers le treasury |
| POST | `/admin/compliance/reverse` | Inverse une transaction TxUtxo (si outputs non dépensés) |
| GET | `/admin/compliance/frozen` | Liste toutes les adresses gelées |
| GET | `/admin/compliance/log` | Journal d'audit de toutes les actions compliance |
| GET | `/admin/compliance/shadow_balance` | Balances détaillées des comptes gelés (par asset, avec UTXO count) |

### Détail des codes de retour

| Endpoint | 200 | 400 | 401 | 404 | 409 | 422 | 500 |
|----------|-----|-----|-----|-----|-----|-----|-----|
| `freeze` | Adresse gelée | Adresse vide | Non autorisé | - | Déjà gelée | Bloc rejeté | Erreur interne |
| `unfreeze` | Adresse dégelée | - | Non autorisé | Pas gelée | Bloc dupliqué | Bloc rejeté | Erreur interne |
| `seize` | Fonds saisis | Pas de treasury | Non autorisé | Pas d'UTXOs | Bloc dupliqué | Bloc rejeté | Erreur interne |
| `reverse` | TX inversée | Pas TxUtxo | Non autorisé | Bloc introuvable | Bloc dupliqué | Output dépensé | Erreur interne |

## Payload Types (DAG)

Les 4 types de payload compliance enregistrés dans le DAG :

```rust
// crates/pms-types-payload/src/payload.rs
PlainPayload::Freeze {
    address: String,
    reason: String,
}

PlainPayload::Unfreeze {
    address: String,
    reason: String,
    freeze_block_id: String,   // Référence au bloc Freeze original
}

PlainPayload::Seize {
    from_address: String,
    inputs: Vec<TxInput>,      // UTXOs saisis
    outputs: Vec<TxOutput>,    // Redistribution vers treasury
    reason: String,
}

PlainPayload::Reverse {
    original_block_id: String, // ID du bloc TX inversé
    inputs: Vec<TxInput>,      // Outputs de la TX originale (consommés)
    outputs: Vec<TxOutput>,    // UTXOs recréés pour les senders originaux
    reason: String,
}
```

## RocksDB Column Families

| CF Name | Clé | Valeur | Description |
|---------|-----|--------|-------------|
| `compliance_frozen` | `address` (bytes) | `FrozenEntry` (JSON) | Registre des adresses gelées. Chaque entrée contient : address, block_id, reason, frozen_at_ms |
| `compliance_log` | `block_id` (bytes) | `ComplianceLogEntry` (JSON) | Journal d'audit immutable. Chaque entrée contient : action, block_id, target_address, details (JSON), timestamp_ms |

### Structs de données

```rust
// crates/pms-storage/src/rocks_store/compliance_registry.rs

struct FrozenEntry {
    address: String,
    block_id: String,      // ID du bloc Freeze
    reason: String,
    frozen_at_ms: i64,     // Timestamp Unix ms
}

struct ComplianceLogEntry {
    action: String,           // "freeze", "unfreeze", "seize", "reverse"
    block_id: String,         // ID du bloc compliance
    target_address: Option<String>,
    details: serde_json::Value,
    timestamp_ms: i64,
}
```

### Cache in-memory : `frozen_set`

- **Type** : `DashSet<String>` (lock-free concurrent set)
- **Localisation** : champ `frozen_set` de `RocksStore` (`crates/pms-storage/src/rocks_store/store.rs`)
- **Peuplement** : `load_frozen_cache()` appelé au bootstrap du ledger (`crates/pms-ledger/src/instance.rs`)
- **Mise à jour** : write-through sur `freeze_address()` (insert) et `unfreeze_address()` (remove)
- **Performance** : transforme les lookups `is_frozen()` de O(N) RocksDB en O(1) DashSet. Critique pour le hot path de validation des transactions.

## Enforcement du gel

Le gel est enforcé à **deux niveaux** pour garantir la sécurité :

### Niveau 1 : API (`prepare_tx`)

Fichier : `crates/pms-server/src/api_fn/transaction.rs` (lignes ~441-452)

Avant la construction de la transaction, le handler `prepare_tx` vérifie :
- `is_frozen(from)` -- bloque l'envoi depuis une adresse gelée (HTTP 403)
- `is_frozen(to)` -- bloque la réception vers une adresse gelée (HTTP 403)

### Niveau 2 : Consensus (`persist_block`)

Fichier : `crates/pms-core/src/net_adapter.rs` (section `4.compliance`)

Lors de la persistence d'un bloc reçu par le réseau P2P :
- Pour les blocs `TxUtxo` : vérifie les adresses sender (via lookup UTXO des inputs) et recipient (outputs)
- Pour les blocs `BridgeLock` : vérifie les adresses sender des inputs
- Résultat : `PutResult::Rejected("compliance: sender/recipient address is frozen: ...")`

Cette double vérification garantit que même si un bloc frauduleux contourne l'API, il sera rejeté au niveau du consensus.

## Interactions

### Avec le [[activity-system|système d'activité]]

- Les actions compliance génèrent des entrées d'activité indexées par adresse.
- Catégories d'activité : `Compliance` (code 8) pour freeze/unfreeze/seize, `Reverse` (code 9) pour reverse.
- Types de filtre API : `"freeze"`, `"unfreeze"`, `"seized"`, `"seize_received"`, `"reverse_received"`.
- Fichiers concernés : `crates/pms-storage/src/helpers.rs`, `crates/pms-server/src/api_fn/activity.rs`

### Avec le système UTXO

- **Seize** : consomme les UTXOs de la cible et crée de nouveaux UTXOs pour le treasury (même logique de delta que `TxUtxo`).
- **Reverse** : consomme les outputs de la TX originale et recrée des UTXOs pour les senders originaux.
- **Freeze/Unfreeze** : pas de delta UTXO (opérations registre uniquement).
- La mise à jour du cache RAM UTXO est faite via `apply_utxo_delta()` dans les handlers `admin_seize` et `admin_reverse`.

### Avec le système de wallet / historique

- Les payloads Freeze/Unfreeze/Seize/Reverse sont intégrés dans l'extraction d'adresses de `pms-wallet/src/history.rs`.
- `involves_address()` et `involves_any_address()` gèrent correctement les 4 variants.
- L'historique d'un wallet gelé affichera les événements freeze/unfreeze/seize/reverse_received.

### Avec le [[bridge|Bridge]]

- Les `BridgeLock` (verrouillage de fonds pour transfert cross-chain) sont soumis au freeze check dans `persist_block`.
- Un compte gelé ne peut pas initier de bridge lock.

### Avec la validation de blocs

- Tous les blocs compliance exigent la signature du Coordinateur (`require_coordinator_signature()`).
- Les nœuds non-coordinateurs ne peuvent pas créer de blocs compliance -- ils seront rejetés à la validation.

### Avec le block ID / labelling

- Les blocs compliance sont étiquetés dans `block_id.rs` et `net_adapter.rs` avec les labels : `"Freeze"`, `"Unfreeze"`, `"Seize"`, `"Reverse"`.
