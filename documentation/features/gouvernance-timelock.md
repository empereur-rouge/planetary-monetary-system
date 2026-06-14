---
tags: [feature]
created: 2026-06-14
updated: 2026-06-14
version: v0.15.0
---

# Gouvernance Timelock

## Résumé

Tout changement de paramètre **gouverné** du moteur (frais, distribution, couloir
d'émission, etc.) passe par le cycle `GovernanceProposal → timelock → GovernanceEnact`,
**ancré dans le DAG** : annoncé publiquement, horodaté, signé Coordinator. C'est le
capstone de crédibilité du [[budget-emission|système d'émission]] — la
matérialisation de « règle inviolable que l'opérateur ne peut pas franchir par
surprise » (plan §2.1, §4).

Trois propriétés :

- **Annonce avant effet** — un `propose` n'applique RIEN ; il publie l'intention.
  `GET /v1/governance/pending` est **public** : tout le monde voit ce qui se
  prépare (droit de sortie avant que le changement ne prenne effet).
- **Timelock inviolable au protocole** — `persist_block` REJETTE tout `enact`
  tant que `now < enact_after` (pas seulement le handler HTTP). Le délai dépend du
  palier : `Operator` 7 j, `Policy` 15 j, `Constitution` 45 j.
- **Audit complet** — `enact`/`cancel` sont eux-mêmes des blocs DAG signés ; le
  statut de chaque proposition (`Pending`/`Enacted`/`Cancelled`) est traçable via
  `GET /v1/governance/history` (public).

**Seam clé** : un chemin DAG-ancré existait déjà (`PlainPayload::ConfigUpdate` →
bloc → `apply_config_update` keyé par block id). La gouvernance le réutilise :
`enact` appelle le **même** `apply_config_update` après expiration du délai.

**État (v0.15.0)** : P1a (cœur protocole + storage) + P1b (endpoints + e2e) livrés.
À venir : P2 (asymétrie tighten-now/loosen-later + table palier-min par paramètre +
rewire `admin_update_config`→propose + tâche auto-enact), P3 (couloir d'émission
sous gouvernance via `SetEmissionCorridor`). Spec : `pms-spec-governance-timelock.md`.

## Configuration

Durées de timelock par palier — actuellement **hardcodées** dans
`GovernanceTier::default_duration_ms` (7/15/45 j × 86 400 000 ms). Surcharge par
config (testnet raccourci pour tester l'enact réel) prévue en phase ultérieure.
Aucune activation requise : les routes sont montées en standard ; les `propose`/
`enact`/`cancel` sont admin-gated (et gated read-only car ils produisent des blocs).

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-config` | `src/governance.rs` | `GovernanceTier`, `GovernanceStatus`, `GovernanceProposalRecord` |
| `pms-types-payload` | `src/payload.rs` | Variantes `PlainPayload::Governance{Proposal,Enact,Cancel}` (coordinator-only) |
| `pms-storage` | `src/governance_store.rs` | Trait `GovernanceStorage` (dans `EngineStorage`) |
| `pms-storage` | `src/rocks_store/governance_storage.rs` | Impl RocksDB (CF `governance_proposals`) + garde d'unicité |
| `pms-core` | `src/net_adapter/persist.rs` | Validation hot-path : timelock inviolable, apply à l'enact, unicité du proposal_id |
| `pms-core` | `src/validations/{authority,check}.rs` | Autorité coordinator-only des 3 variantes |
| `pms-server` | `src/api_fn/governance.rs` | Endpoints REST (propose/enact/cancel/pending/history) |
| `pms-server` | `src/api_error.rs` | Code `3071 GovernanceRejected` (raison surfacée) |

## Fonctions Clés

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `forge_governance_block` | `pms-server/src/api_fn/governance.rs` | Forge + persiste un bloc gouvernance signé Coordinator, mappe `PutResult`→`ApiError` |
| `admin_propose` | `pms-server/src/api_fn/governance.rs` | Annonce ; `proposal_id = SHA-256(update+tier+announced_at)`, `enact_after = now + tier.duration` |
| `list_filtered` | `pms-server/src/api_fn/governance.rs` | Backend partagé de `list_pending`/`list_history` (filtre par statut) |
| `GovernanceTier::default_duration_ms` | `pms-config/src/governance.rs` | Durées de timelock 7/15/45 j |
| persist enact arm | `pms-core/src/net_adapter/persist.rs` | **Rejet si `now < enact_after`** ; sinon `apply_config_update` + `Enacted` |

## Endpoints API

| Méthode | Path | Accès | Description |
|---------|------|-------|-------------|
| POST | `/admin/governance/propose` | admin (gated) | Annonce un changement timelocké |
| POST | `/admin/governance/enact/{id}` | admin (gated) | Applique après expiration (rejeté avant) |
| POST | `/admin/governance/cancel/{id}` | admin (gated) | Annule une proposition `Pending` |
| GET | `/v1/governance/pending` | **public** | Propositions en attente (l'annonce) |
| GET | `/v1/governance/history` | **public** | Propositions enacted / cancelled (audit) |

## Tests

- `pms-storage` : `governance_proposal_roundtrip_and_status`, `tier_durations_golden`.
- `pms-core` (`tests/governance_timelock_test.rs`) : **G3** (enact après expiration
  applique le `ConfigUpdate`), **G2** (enact avant expiration rejeté, config
  inchangée), **DUP** (doublon `proposal_id` rejeté, record intact).
- `pms-server` (`tests/dag_sandbox.rs::test_governance_timelock_endpoints`) : e2e
  HTTP — propose → /pending → enact précoce rejeté (code 3071 + raison timelock) →
  cancel → /history.

## Interactions
Liens : [[budget-emission]] (la politique monétaire que la gouvernance protège),
[[smart-contracts]] (la frontière natif-vs-contrat que l'invariant garde),
[[config-system]] (`ConfigUpdate`/`apply_config_update` réutilisés), [[trust-model]]
(ce que le Coordinator peut/ne peut pas), [[storage-rocksdb]] (CF + migration).
