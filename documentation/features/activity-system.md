---
tags: [feature]
created: 2026-02-28
updated: 2026-03-21
version: v0.6.0
---

# Activity System (Index, Pré-calcul, Cache)

## Résumé

L'Activity System fournit un flux d'activité sémantique par wallet, remplaçant le parcours brut du DAG par un système d'indexation multiniveau dans RocksDB. Chaque bloc persisté est automatiquement indexé par adresse et par catégorie d'activité, avec des items pré-calculés au moment de l'écriture pour éliminer la classification à la lecture. Un cache LRU en mémoire et un endpoint SSE complètent le dispositif pour offrir des lectures instantanées et du streaming temps réel.

Contrairement à l'ancien endpoint `/wallet/history` qui retourne des payloads bruts, l'Activity API classifie chaque événement avec un type sémantique (`fee_received`, `transfer_in`, `nft_mint`, `freeze`, etc.), une direction (`in`, `out`, `info`), un montant net et la contrepartie.

## Dates

| | Date |
|---|---|
| Créée | 2026-02-28 |
| Dernière mise à jour | 2026-03-12 |
| Version d'introduction | v0.2.0 |

### Historique des commits

| Date | Commit | Description |
|------|--------|-------------|
| 2026-02-28 | `eadbce4` | Phase 1 : index per-address (`addr_activity` CF) |
| 2026-02-28 | `43c4297` | Refactor `history_plain_for_address` pour utiliser l'index |
| 2026-03-01 | `454b244` | Phase 2 : index per-type (`addr_type_activity` CF) |
| 2026-03-01 | `b6a9a52` | Fix : ajout indexation dans `append_block_atomic` |
| 2026-03-02 | `f0109ad` | Fix : rendre les transfers chiffrés visibles dans l'activité |
| 2026-03-02 | `b2e475f` | Admin endpoint `POST /admin/reindex-activity` |
| 2026-03-03 | `a092a94` | Phase 3 : pré-calcul items + cache LRU + limite 2000 |
| 2026-03-03 | `8d989b2` | Tests e2e couvrant les 19 types d'activité |
| 2026-03-03 | `ca50b56` | Fix : résolution du sender AVANT le spend UTXO |
| 2026-03-04 | `2bfae93` | Fix : timestamp mismatch pour queries typées chiffrées |
| 2026-03-04 | `d4876fb` | Perf : `multi_get_cf`, bloom filter, batch reindex |
| 2026-03-12 | `f3d79e9` | Fix : classifier les fee outputs TxUtxo comme `fee_received` |

## Architecture

Le système est organisé en 3 phases, chacune ajoutant un niveau d'optimisation :

### Phase 1 : Index per-address (`addr_activity`)

Index brut par adresse. Pour chaque bloc persisté, toutes les adresses impliquées sont extraites du payload et une entrée est écrite dans le CF `addr_activity`.

**Format de clé** : `[addr_bytes][0x00][ts_be:8][block_id_bytes]`

- Le séparateur `0x00` est sans ambiguïté car les adresses bech32 sont en ASCII printable.
- Le timestamp big-endian permet un scan inverse (newest-first) par prefix iteration.
- La valeur est vide (`b""`) -- seule la clé porte l'information.

**Requête** : prefix scan sur `[addr][0x00]` avec `IteratorMode::End`, itère en reverse.

### Phase 2 : Index per-type (`addr_type_activity`)

Index secondaire par adresse + catégorie. Permet les requêtes filtrées (ex: "uniquement les fees de cette adresse") sans scanner l'index brut.

**Format de clé** : `[addr_bytes][0x00][category:1][ts_be:8][block_id_bytes]`

- Le byte `category` est un discriminant `u8` de l'enum `ActivityCategory` (1..9).
- Requête multi-catégorie : k-way merge de plusieurs prefix scans.

### Phase 3 : Pré-calcul + Cache

Les items d'activité sont classifiés et sérialisés au moment de la persistance du bloc, stockés dans le CF `activity_items`. À la lecture, le endpoint peut retourner directement le JSON pré-calculé sans reparser le bloc ni résoudre le sender UTXO.

**Format de clé** : identique à `addr_activity` (`[addr][0x00][ts_be:8][block_id]`).
**Valeur** : `JSON(Vec<StoredActivityItem>)`.

**Cache LRU** : `ActivityCache` en mémoire (DashMap), 10 000 entrées max, TTL 30s. Les requêtes avec `x25519_sk_hex` (déchiffrement) contournent le cache car elles sont spécifiques à l'utilisateur.

**Fallback** : si aucun item pré-calculé n'existe (blocs historiques avant la Phase 3), le endpoint fetche le bloc complet et le classifie à la volée via `classify_activity()`.

## Configuration

| Paramètre | Valeur par défaut | Localisation | Description |
|-----------|-------------------|--------------|-------------|
| Limite max par requête | 2000 | `activity/handler.rs` (`q.limit.unwrap_or(50).min(2000)`) | Plafond du paramètre `limit` |
| Limite par défaut | 50 | `activity/handler.rs` | Valeur si `limit` absent |
| Batch size (scan interne) | 500 | `activity/handler.rs` (`BATCH_SIZE`) | Nombre d'entrées fetchées par itération de scan |
| Cache max entries | 10 000 | `api/state.rs`, `main.rs` | Taille max du `ActivityCache` |
| Cache TTL | 30s | `api/state.rs`, `main.rs` | Durée de vie des entrées cache |
| Cache eviction | 10% des expirées | `activity/cache.rs` | Stratégie d'éviction quand plein |
| Reindex batch flush | 1000 | `store.rs` (`reindex_all_activity_items`, `FLUSH_EVERY`) | WriteBatch flush pendant reindex |

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-storage` | `crates/pms-storage/src/helpers/mod.rs` | Re-exports des sous-modules helpers |
| `pms-storage` | `crates/pms-storage/src/helpers/encoding.rs` | Fonctions de construction/parsing de clés (`key_addr_activity`, `prefix_addr_activity`, etc.) |
| `pms-storage` | `crates/pms-storage/src/helpers/activity_keys.rs` | Fonctions de clés d'activité typées (`key_addr_type_activity`, `prefix_addr_type_activity`, etc.) |
| `pms-storage` | `crates/pms-storage/src/helpers/classify.rs` | Classification pour storage (`classify_for_storage`, `precompute_all_items`), enum `ActivityCategory`, extraction d'adresses |
| `pms-storage` | `crates/pms-storage/src/activity_item.rs` | Struct `StoredActivityItem` sérialisable en JSON |
| `pms-storage` | `crates/pms-storage/src/lib.rs` | Ré-export de `activity_item::StoredActivityItem` |
| `pms-storage` | `crates/pms-storage/src/traits.rs` | Trait `DagStorage` avec méthodes `recent_ids_by_address` et `recent_ids_by_address_and_categories` |
| `pms-storage` | `crates/pms-storage/src/rocks_store/store.rs` | Implémentation RocksDB : CF declarations, scan paginé, write entries |
| `pms-storage` | `crates/pms-storage/src/rocks_store/activity_index.rs` | Reindex activity, activity item queries |
| `pms-storage` | `crates/pms-storage/src/rocks_store/atomic.rs` | `apply_addr_activity_indices()` dans le WriteBatch atomique |
| `pms-storage` | `crates/pms-storage/src/rocks_store/migration.rs` | Migrations 3->4 (`addr_activity`), 4->5 (`addr_type_activity`), 5->6 (fee output reindex) |
| `pms-storage` | `crates/pms-storage/tests/addr_activity_test.rs` | Tests unitaires pour indexation, pagination, reindex |
| `pms-server` | `crates/pms-server/src/api_fn/activity/mod.rs` | Re-exports du module activity |
| `pms-server` | `crates/pms-server/src/api_fn/activity/handler.rs` | Handler HTTP GET `get_wallet_activity()`, `ActivityCache` |
| `pms-server` | `crates/pms-server/src/api_fn/activity/stream.rs` | Handler SSE `stream_wallet_activity()` |
| `pms-server` | `crates/pms-server/src/api_fn/activity/cache.rs` | `ActivityCache` struct (LRU in-memory, DashMap + TTL) |
| `pms-server` | `crates/pms-server/src/api_fn/activity/classify.rs` | Logique de classification async/sync, `classify_activity()`, `classify_activity_sync()` |
| `pms-server` | `crates/pms-server/src/api_fn/mod.rs` | Déclaration `pub mod activity` |
| `pms-server` | `crates/pms-server/src/api/routes.rs` | Routes (`/v1/wallet/{address}/activity`, `.../activity/stream`) |
| `pms-server` | `crates/pms-server/src/api/state.rs` | Champ `activity_cache` dans `AppState` |
| `pms-server` | `crates/pms-server/src/admin.rs` | Handlers admin `POST /admin/reindex-activity` et `POST /admin/reindex-activity-items` |
| `pms-server` | `crates/pms-server/tests/activity_e2e.rs` | Tests e2e couvrant les 19 types d'activité |
| `pms-event` | `crates/pms-event/src/events.rs` | Variant `PmsEvent::BlockPersisted` utilisé par le SSE stream |
| `pms-wallet` | `crates/pms-wallet/src/history.rs` | `involves_address()`, `history_plain_for_address()` (utilise l'index `addr_activity`), decryption helpers |

## Fonctions Clés

### Couche Storage (`pms-storage`)

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `extract_involved_addresses()` | `helpers/classify.rs` | Extrait toutes les adresses impliquées d'un `PlainPayload` (sans catégorie) |
| `extract_involved_with_category()` | `helpers/classify.rs` | Extrait les paires `(adresse, ActivityCategory)` avec distinction fee/reward/transfer |
| `classify_for_storage()` | `helpers/classify.rs` | Classifie un payload en `Vec<StoredActivityItem>` pour une adresse donnée (sync, sender pré-résolu) |
| `precompute_all_items()` | `helpers/classify.rs` | Appelle `classify_for_storage` pour chaque adresse impliquée, retourne `HashMap<addr, Vec<StoredActivityItem>>` |
| `is_fee_output_only()` | `helpers/classify.rs` | Détecte si une adresse est exclusivement un collecteur de fees dans un TxUtxo (montant outputs == tx.fee) |
| `key_addr_activity()` | `helpers/activity_keys.rs` | Construit la clé `[addr][0x00][ts_be][block_id]` |
| `prefix_addr_activity()` | `helpers/activity_keys.rs` | Construit le prefix `[addr][0x00]` pour scan |
| `parse_addr_activity_key()` | `helpers/activity_keys.rs` | Parse une clé addr_activity en `(ts_ms, block_id)` |
| `key_addr_type_activity()` | `helpers/activity_keys.rs` | Construit la clé `[addr][0x00][cat][ts_be][block_id]` |
| `prefix_addr_type_activity()` | `helpers/activity_keys.rs` | Construit le prefix `[addr][0x00][cat]` pour scan typé |
| `parse_addr_type_activity_key()` | `helpers/activity_keys.rs` | Parse une clé addr_type_activity en `(cat, ts_ms, block_id)` |
| `ActivityCategory::from_filter_type()` | `helpers/classify.rs` | Mappe un filtre API string vers un `ActivityCategory` |
| `apply_addr_activity_indices()` | `atomic.rs` | Écrit les 3 CFs (addr_activity, addr_type_activity, activity_items) dans un WriteBatch atomique |
| `recent_ids_by_address()` | `store.rs` | Scan paginé reverse du CF `addr_activity` pour une adresse |
| `recent_ids_by_address_and_categories()` | `store.rs` | Scan paginé du CF `addr_type_activity` avec k-way merge multi-catégorie |
| `recent_activity_items_by_address()` | `store.rs` | Comme `recent_ids_by_address` mais retourne aussi les `StoredActivityItem` pré-calculés |
| `recent_activity_items_by_address_and_categories()` | `store.rs` | Comme ci-dessus avec filtre par catégories |
| `write_addr_activity_entries()` | `store.rs` | Écriture standalone (hors batch) des index pour un bloc avec adresses pré-calculées |
| `write_addr_activity_entries_with_categories()` | `store.rs` | Écriture standalone des 3 CFs avec catégories et items pré-calculés |
| `reindex_all_activity()` | `activity_index.rs` | Reconstruit `addr_activity` + `addr_type_activity` pour tous les blocs |
| `reindex_all_activity_items()` | `activity_index.rs` | Reconstruit le CF `activity_items` pour tous les blocs |

### Couche Serveur (`pms-server`)

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `get_wallet_activity()` | `activity/handler.rs` | Handler HTTP GET : cache lookup, scan index, fast path (pré-calculé) + fallback (block fetch + classify) |
| `stream_wallet_activity()` | `activity/stream.rs` | Handler SSE : subscribe à l'EventBus, filtre par adresse en mémoire, classification sync |
| `classify_activity()` | `activity/classify.rs` | Classification async avec résolution UTXO du sender |
| `classify_activity_sync()` | `activity/classify.rs` | Classification sync (SSE) : classification par outputs uniquement, sans lookup UTXO |
| `resolve_sender()` | `activity/classify.rs` | Résolution du sender TxUtxo via le cache UTXO (`adapter.get_utxo()`) |
| `parse_type_filter()` | `activity/handler.rs` | Parse le paramètre `?type=a,b,c` en `Vec<&str>` |
| `ActivityCache::new()` | `activity/cache.rs` | Constructeur du cache LRU (DashMap + TTL) |
| `ActivityCache::get()` / `put()` | `activity/cache.rs` | Lecture/écriture cache avec éviction TTL |
| `ActivityCache::invalidate_address()` | `activity/cache.rs` | Invalide toutes les entrées cache pour une adresse |
| `admin_reindex_activity()` | `admin.rs` | Handler admin POST pour réindexer `addr_activity` + `addr_type_activity` |
| `admin_reindex_activity_items()` | `admin.rs` | Handler admin POST pour réindexer le CF `activity_items` |

## Endpoints API

### Publics

| Méthode | Path | Description |
|---------|------|-------------|
| GET | `/v1/wallet/{address}/activity` | Activité paginée, filtrable par type/asset, avec déchiffrement optionnel |
| GET | `/v1/wallet/{address}/activity/stream` | SSE streaming temps réel de l'activité |
| GET | `/l/{ledger_id}/v1/wallet/{address}/activity` | Variante [[multi-ledger]] (ajoute `ledger_id` dans la réponse) |
| GET | `/l/{ledger_id}/v1/wallet/{address}/activity/stream` | Variante [[multi-ledger]] SSE |

### Admin (authentifiés)

| Méthode | Path | Description |
|---------|------|-------------|
| POST | `/admin/reindex-activity` | Reconstruit `addr_activity` + `addr_type_activity` pour tous les blocs |
| POST | `/admin/reindex-activity-items` | Reconstruit le CF `activity_items` pour tous les blocs |

### Paramètres de requête (GET activity)

| Paramètre | Type | Défaut | Description |
|-----------|------|--------|-------------|
| `type` | string | _(tous)_ | Filtre par type(s) d'activité, séparés par virgule |
| `limit` | number | 50 | Nombre max d'éléments (max: 2000) |
| `after_ts` | number | - | Curseur de pagination : timestamp ms (exclusif) |
| `after_id` | string | - | Curseur de pagination : block ID |
| `asset_id` | string | - | Filtre par asset ID |
| `x25519_sk_hex` | string | - | Clé privée X25519 hex pour déchiffrer les payloads chiffrés |

### Paramètres de requête (SSE stream)

| Paramètre | Type | Défaut | Description |
|-----------|------|--------|-------------|
| `type` | string | _(tous)_ | Filtre par type(s) d'activité |
| `x25519_sk_hex` | string | - | Clé privée X25519 hex pour déchiffrement en temps réel |

### Format de réponse (GET)

```json
{
  "address": "pms1abc...",
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
      "payload": { ... }
    }
  ],
  "count": 1,
  "next_after_ts": 1740000000000,
  "next_after_id": "a1b2c3d4...",
  "has_more": false
}
```

### Format SSE

```
event: activity
data: {"block_id":"a1b2...","ts_ms":1740000000000,"activity_type":"transfer_in",...}

event: warning
data: {"warning":"lagged by 5 events"}
```

## RocksDB Column Families

### `addr_activity`

Index non-typé par adresse. Chaque entrée représente un bloc impliquant une adresse donnée.

| Attribut | Détail |
|----------|--------|
| **Clé** | `[addr_bytes][0x00][ts_be:8][block_id_bytes]` |
| **Valeur** | `b""` (vide) |
| **Itération** | Reverse prefix scan sur `[addr][0x00]` pour newest-first |
| **Introduit** | Migration 3->4 (commit `eadbce4`) |

### `addr_type_activity`

Index typé par adresse + catégorie. Permet le filtrage par type d'activité sans scanner l'index brut.

| Attribut | Détail |
|----------|--------|
| **Clé** | `[addr_bytes][0x00][category:1][ts_be:8][block_id_bytes]` |
| **Valeur** | `b""` (vide) |
| **Itération** | Reverse prefix scan sur `[addr][0x00][cat]` ; k-way merge pour multi-catégorie |
| **Introduit** | Migration 4->5 (commit `454b244`) |

### `activity_items`

Items d'activité pré-calculés. Stockés au moment de la persistance du bloc pour éviter la classification à la lecture.

| Attribut | Détail |
|----------|--------|
| **Clé** | `[addr_bytes][0x00][ts_be:8][block_id_bytes]` (même format que `addr_activity`) |
| **Valeur** | `JSON(Vec<StoredActivityItem>)` |
| **Lookup** | Point-get par clé connue (pas de scan) |
| **Introduit** | Commit `a092a94` |

### `StoredActivityItem` (structure JSON)

```rust
pub struct StoredActivityItem {
    pub activity_type: String,      // "mint", "transfer_in", "fee_received", etc.
    pub direction: String,          // "in", "out", "info"
    pub amount: Option<String>,     // Montant net (absent pour NFT/compliance)
    pub asset_id: Option<String>,   // Asset ID (None = PMS natif)
    pub counterparty: Option<String>, // Adresse de la contrepartie
    pub payload: serde_json::Value, // Payload brut
}
```

Note : `block_id`, `ts_ms`, et `ledger_id` ne sont PAS stockés dans `StoredActivityItem` car ils sont dérivés de la clé et du contexte de la requête.

## Activity Categories

9 catégories définies dans `ActivityCategory` (`helpers/classify.rs`), chacune mappée à un discriminant `u8` :

| Discriminant | Catégorie | Types d'activité mappés | Description |
|:---:|-----------|------------------------|-------------|
| 1 | `Mint` | `mint` | Émission de tokens |
| 2 | `Transfer` | `transfer_in`, `transfer_out`, `transfer_self` | Transferts de tokens |
| 3 | `Fee` | `fee_received` | Réception de frais de transaction |
| 4 | `Reward` | `reward` | Récompenses de bloc |
| 5 | `Nft` | `nft_mint`, `nft_transfer_in`, `nft_transfer_out`, `nft_burn`, `nft_use` | Opérations [[nft-system|NFT]] |
| 6 | `TokenCreate` | `token_create` | Création de token |
| 7 | `Bridge` | `bridge_lock_in`, `bridge_mint` | Opérations [[bridge|cross-ledger]] |
| 8 | `Compliance` | `freeze`, `unfreeze`, `seized`, `seize_received` | Opérations [[compliance|réglementaires]] |
| 9 | `Reverse` | `reverse_received` | Transactions inversées |

### Types d'activité détaillés (19 types)

| Type | Direction | Catégorie | Description |
|------|-----------|-----------|-------------|
| `mint` | in | Mint | Réception d'un mint |
| `transfer_in` | in | Transfer | Réception de tokens d'un tiers |
| `transfer_out` | out | Transfer | Envoi de tokens à un tiers |
| `transfer_self` | info | Transfer | Consolidation (envoi à soi-même) |
| `fee_received` | in | Fee | Fee reçue (coordinateur/trésor) |
| `reward` | in | Reward | Récompense de bloc |
| `nft_mint` | in | Nft | NFT créé par l'adresse |
| `nft_transfer_in` | in | Nft | NFT reçu |
| `nft_transfer_out` | out | Nft | NFT envoyé |
| `nft_burn` | out | Nft | NFT brûlé |
| `nft_use` | info | Nft | NFT utilisé |
| `token_create` | info | TokenCreate | Token créé par l'adresse |
| `bridge_lock_in` | in | Bridge | Fonds verrouillés pour bridge (destinataire) |
| `bridge_mint` | in | Bridge | Fonds bridge reçus |
| `freeze` | info | Compliance | Compte gelé |
| `unfreeze` | info | Compliance | Compte dégelé |
| `seized` | out | Compliance | Fonds saisis |
| `seize_received` | in | Compliance | Fonds saisis reçus (trésor) |
| `reverse_received` | in | Reverse | Fonds rendus par transaction inversée |

### Détection des fee outputs (TxUtxo)

Un cas spécial : dans un `TxUtxo`, si une adresse reçoit exactement le montant des frais (`output.amount == tx.fee`), elle est classifiée comme `fee_received` au lieu de `transfer_in`. Cela permet de distinguer le collecteur de fees des vrais destinataires du transfert. La détection est implémentée par `is_fee_output_only()`.

## Interactions

### Avec le système de wallet

- `pms_wallet::history::involves_address()` est utilisé comme filtre rapide pour déterminer si un payload concerne une adresse avant classification complète.
- `pms_wallet::history::history_plain_for_address()` utilise l'index `addr_activity` pour localiser les blocs impliquant une adresse (commit `43c4297`).
- `pms_wallet::history::try_decrypt_encrypted_reward()` est utilisé pour déchiffrer les `EncryptedReward` quand une clé privée X25519 est fournie.

### Avec les payloads chiffrés

- Les payloads `Encrypted` et `EncryptedReward` ne peuvent pas être indexés au moment du reindex car ils nécessitent la clé privée du destinataire.
- Au moment de la création d'un bloc chiffré, le coordinateur extrait les adresses et catégories du payload en clair **avant** le chiffrement et appelle `write_addr_activity_entries_with_categories()` pour indexer les CFs (commit `f0109ad`).
- À la lecture, si `x25519_sk_hex` est fourni, le déchiffrement est tenté. En cas d'échec, un item `"encrypted"` est retourné comme placeholder.
- Les requêtes avec `x25519_sk_hex` ne sont PAS cachées (elles sont spécifiques à l'utilisateur).

### Avec la persistance des blocs

- `append_block_atomic()` dans `atomic.rs` appelle `apply_addr_activity_indices()` pour écrire les 3 CFs d'activité dans le même WriteBatch atomique que le bloc.
- `append_block_atomic_with_utxo()` dans `store.rs` fait de même (ligne 2041).
- Les index d'activité sont donc **toujours cohérents** avec le bloc stocké (atomicité RocksDB).

### Avec le SSE streaming

- L'event `PmsEvent::BlockPersisted` (défini dans `pms-event`) transporte le `block_id`, `ts_ms`, `involved_addresses` et `payload_json`.
- Le handler SSE (`stream_wallet_activity`) subscribe à l'EventBus via `bus.subscribe()` et filtre les événements en mémoire par adresse -- zéro accès DB.
- La classification SSE est synchrone (`classify_activity_sync`) car elle ne peut pas faire de lookup UTXO async. Le sender est résolu uniquement par les outputs.
- En cas de retard du client, un événement `warning` avec le nombre d'événements manqués est émis (`RecvError::Lagged`).

### Avec les migrations DB

- **Migration 3->4** (`mig_3_to_4`) : backfill du CF `addr_activity` pour tous les blocs existants.
- **Migration 4->5** (`mig_4_to_5`) : backfill du CF `addr_type_activity` pour tous les blocs existants.
- **Migration 5->6** (`mig_5_to_6`) : ré-indexation des TxUtxo pour corriger les fee outputs (Transfer -> Fee) et re-calcul des `activity_items`.
- Version actuelle du schéma : `CURRENT_VER = 8` (les migrations 6->7 et 7->8 concernent d'autres fonctionnalités).

### Avec le système admin

- `POST /admin/reindex-activity` : appelle `store.reindex_all_activity()`, reconstruit `addr_activity` + `addr_type_activity`. Idempotent.
- `POST /admin/reindex-activity-items` : appelle `store.reindex_all_activity_items()`, reconstruit le CF `activity_items` par batches de 1000. Idempotent.
- Les deux endpoints sont protégés par authentification admin (`is_admin_authorized`).
- Retournent des `ReindexStats { total_blocks, indexed, skipped_encrypted, skipped_no_payload }`.
