---
tags: [feature]
created: 2026-06-12
updated: 2026-06-12
version: v0.10.0
---

# Primitives Protocole DAG (time-lock, spend conditions, mint contraint, demurrage, réserves)

## Résumé

Cinq primitives bas-niveau du protocole DAG (plan §2, v0.10.0) qui enrichissent
les UTXOs et le contrôle d'émission, validées dans le hot path
(`validate_transaction_full` / `persist_block`) :

1. **Time-lock (2.1)** — `TxOutput.locked_until` (UNIX ms) : output indépensable
   avant l'échéance. Erreur `OutputTimeLocked`.
2. **Spend conditions (2.2)** — `TxOutput.spend_condition` : `PubKey` (binding
   C-1 classique), `MultiSig{m, pubkeys}` (M-of-N, adresse canonique `msig1…`
   engageant la policy), `HashLock{hash_hex}` (préimage SHA-256). L'`Unlock`
   porte `cosigners` + `preimage_hex`.
3. **Mint contraint (2.3/2.4)** — `mint_authority`, `max_supply` et `decimals`
   de `TokenMetadata` sont enforced au niveau protocole pour chaque mint
   d'asset custom **enregistré** (avant : API-only). L'enregistrement
   `TokenCreate` est l'opt-in des contraintes ; un asset sans metadata garde
   le comportement historique (refunds de contrats type edenite-cube-burn).
4. **Demurrage (2.5)** — `TokenMetadata.demurrage_bps_per_day` (opt-in) :
   valeur effective d'un UTXO = nominal − décote par jour plein depuis
   `created_at` (estampillé système). Conservation `out ≤ effective_in`.
5. **Preuve de réserves (2.6)** — bloc `ReserveSnapshot` coordinator-only
   ancrant `state_root` (SHA-256 du CF utxo complet) + supply par asset.

Tous les champs sont **optionnels + serde-compatibles** : aucun changement de
`signing_message` pour les transactions existantes, aucune migration RocksDB
(DAG_VERSION 3.0.0 → 3.1.0 auto-migrating, `CURRENT_VER` inchangé).

## Configuration

| Clé | Défaut | Rôle |
|---|---|---|
| `[reserves].enabled` | `false` | Active la tâche périodique ReserveSnapshot |
| `[reserves].interval_secs` | `3600` | Intervalle entre snapshots |
| `POST /admin/tokens/create` → `demurrage_bps_per_day` | absent | Décote/jour de l'asset (≤ 10000) |
| `POST /admin/tokens/create` → `max_supply` | absent | Cap de supply enforced au mint |

## Crates et Fichiers

| Crate | Fichier | Rôle |
|---|---|---|
| `pms-types-transaction` | `src/transaction.rs` | `TxOutput{locked_until, spend_condition, created_at}`, `SpendCondition`, `Unlock{cosigners, preimage_hex}`, `Cosigner` |
| `pms-types-payload` | `src/payload.rs` | `TokenMetadata.demurrage_bps_per_day`, `PlainPayload::ReserveSnapshot` |
| `pms-core` | `src/validations/conditions.rs` | MultiSig/HashLock : structure, quorum, adresse canonique |
| `pms-core` | `src/validations/demurrage.rs` | `effective_value` (jours pleins, plancher 0) |
| `pms-core` | `src/validations/mint.rs` | `validate_custom_asset_mints` (authority/decimals/cap) |
| `pms-core` | `src/validations/transactions.rs` | `check_input_time_locks`, conservation demurrage-aware |
| `pms-core` | `src/utxo.rs` | `CompactOutput` étendu (cache RAM), exclusion sélection coins, `current_time_ms` |
| `pms-core` | `src/net_adapter/persist.rs` | Hot path : time-lock/conditions/mint/demurrage + estampillage `created_at` |
| `pms-storage` | `src/rocks_store/utxo.rs` | `UtxoValue{lkd, cond, cat}` + `encode_output` (point d'écriture unique) |
| `pms-storage` | `src/token_store.rs` | Trait `TokenRegistryStorage` |
| `pms-storage` | `src/rocks_store/reserve_snapshot.rs` | Pointeur dernier snapshot (CF `last_ms`) |
| `pms-server` | `src/api_fn/reserves.rs` | Calcul state_root, ancrage, endpoints |
| `pms-server` | `src/api/tasks.rs` | `spawn_reserve_snapshot_task` (gated read-only) |
| `pms-errors` | `src/types.rs` | `OutputTimeLocked`, `InvalidSpendCondition`, `SpendConditionNotMet` |

## Fonctions Clés

| Fonction | Fichier | Description |
|---|---|---|
| `check_input_time_locks` | `validations/transactions.rs` | Rejette un input `locked_until` futur (les 2 chemins) |
| `multisig_address(m, pubkeys)` | `validations/conditions.rs` | Adresse canonique `msig1` + SHA-256(policy) — ordre/casse indifférents |
| `validate_output_conditions` | `validations/conditions.rs` | Structure des conditions à la CRÉATION (TxUtxo + Mint) |
| `check_spend_authorization` | `validations/conditions.rs` | C-1 généralisé : PubKey/MultiSig/HashLock par input |
| `validate_custom_asset_mints` | `validations/mint.rs` | Authority + decimals + supply cap per-asset (pure, testable) |
| `effective_value` | `validations/demurrage.rs` | Valeur post-décote d'un UTXO à l'instant t |
| `check_asset_conservation_with_demurrage` | `validations/transactions.rs` | `out ≤ effective_in` (demurrage) / `==` strict (M-7) |
| `compute_reserves` | `api_fn/reserves.rs` | state_root + totaux par asset (itérateur RocksDB consistant) |
| `perform_reserve_snapshot` | `api_fn/reserves.rs` | Calcul + ancrage bloc + pointeur (tâche & endpoint) |
| `UtxoValue::encode_output` | `rocks_store/utxo.rs` | Sérialisation UNIQUE du CF `utxo` — anti-divergence |

## Endpoints API

| Méthode | Path | Description |
|---|---|---|
| GET | `/v1/reserves/latest` | Dernier snapshot ancré (public) |
| POST | `/admin/reserves/snapshot` | Snapshot immédiat (produit un bloc — `admin_writable`) |
| POST | `/admin/reserves/verify` | Recompute + compare à l'ancré (`admin_recovery`) |
| POST | `/admin/tokens/create` | + champs `demurrage_bps_per_day`, `max_supply` |

## Sécurité — invariants

- La condition de dépense est lue depuis l'UTXO **stocké**, jamais depuis les
  données du dépensier ; l'adresse multisig engage la policy complète.
- Quorum MultiSig : pubkeys normalisées + dédupliquées (pas de quorum-stuffing) ;
  chaque cosignature est vérifiée cryptographiquement sur le message canonique.
- `created_at` est écrasé par le système au persist (anti-antidatage du demurrage).
- `m == 0` rejeté à la création ET à la dépense (un UTXO forgé ne devient pas
  anyone-can-spend).
- Le mint contraint est en PLUS du gate Coordinator (défense en profondeur).
- Piège « champ perdu en cache » éliminé structurellement : `UtxoDelta.create`
  et `add_utxo` transportent le `TxOutput` complet.

## Tests

`cargo test --release -p pms-core --test timelock --test spend_conditions --test mint_constraints --test demurrage_validation -- --nocapture`
+ sandbox : `cargo test --release -p pms-server --test dag_sandbox test_reserve_snapshot_anchor_and_verify -- --ignored --nocapture`

## Interactions

[[utxo-system]], [[token-system]], [[validation-consensus]], [[block-payloads]],
[[economics]], [[storage-rocksdb]], [[compliance]]
