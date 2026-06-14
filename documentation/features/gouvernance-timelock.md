---
tags: [feature]
created: 2026-06-14
updated: 2026-06-14
version: v0.17.0
---

# Gouvernance Timelock

## Résumé

Tout changement de paramètre **gouverné** du moteur (frais, distribution, couloir
d'émission, etc.) passe par le cycle `GovernanceProposal → timelock → GovernanceEnact`,
**ancré dans le DAG** : annoncé publiquement, horodaté, signé Coordinator. C'est le
capstone de crédibilité du [[budget-emission|système d'émission]] — la
matérialisation de « règle inviolable que l'opérateur ne peut pas franchir par
surprise » (plan §2.1, §4).

**P2 (v0.16.0)** ajoute la *politique* : un **palier minimum par paramètre** (on ne
peut pas déclasser un changement Constitution en « Operator 7 j »), l'**asymétrie
tighten-now / loosen-later** (resserrer un risque est instantané, le desserrer exige
le délai plein), le **kill-switch `mint_enabled`** enfin câblé, une **tâche
d'auto-enact**, et la **fermeture du contournement** : `POST /admin/config` ne
s'applique plus en direct — il passe par la gouvernance.

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
| `pms-server` | `src/api_fn/governance.rs` | Endpoints REST + `do_propose`/`do_enact` (cœur partagé) |
| `pms-server` | `src/api_error.rs` | Codes `3071 GovernanceRejected`, `5031 MintDisabled` (kill-switch) |
| `pms-config` | `src/governance_policy.rs` | **(P2)** `min_tier` (table §2), `direction`, `required_timelock_ms`, `validate_tier` |
| `pms-server` | `src/emission.rs` | **(P2)** kill-switch `mint_enabled` dans `EmissionGate::reserve` |
| `pms-server` | `src/api/tasks.rs` | **(P2)** `spawn_governance_enact_task` / `governance_enact_tick` (auto-enact, coordinator-only) |
| `pms-server` | `src/admin.rs` | **(P2)** `admin_update_config` → forge un `GovernanceProposal` (fin du bypass) |

## Fonctions Clés

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `forge_governance_block` | `pms-server/src/api_fn/governance.rs` | Forge + persiste un bloc gouvernance signé Coordinator, mappe `PutResult`→`ApiError` |
| `admin_propose` | `pms-server/src/api_fn/governance.rs` | Annonce ; `proposal_id = SHA-256(update+tier+announced_at)`, `enact_after = now + tier.duration` |
| `list_filtered` | `pms-server/src/api_fn/governance.rs` | Backend partagé de `list_pending`/`list_history` (filtre par statut) |
| `GovernanceTier::default_duration_ms` | `pms-config/src/governance.rs` | Durées de timelock 7/15/45 j |
| persist enact arm | `pms-core/src/net_adapter/persist.rs` | **Rejet si `now < enact_after`** ; sinon `apply_config_update` + `Enacted` |
| `min_tier` / `direction` / `required_timelock_ms` | `pms-config/src/governance_policy.rs` | **(P2)** palier-min par param + asymétrie tighten/loosen (timelock 0 si resserrage) |
| `governance_enact_tick` | `pms-server/src/api/tasks.rs` | **(P2)** enacte les `Pending` au timelock écoulé (coordinator-only, check read-only) |

## Politique (P2) — palier-min + asymétrie

- **Palier minimum** ([`min_tier`](../../crates/pms-config/src/governance_policy.rs)) :
  fees = `Operator` ; burn / distribution / pouvoir-de-mint = `Policy` ; **couloir
  d'émission (`SetEmissionCorridor`) = `Constitution` (P3)**. La validation persist
  impose `tier ≥ min_tier`.
- **Couloir d'émission gouverné (P3)** : `ConfigUpdate::SetEmissionCorridor
  { ceiling_bps, floor_bps, target_bps, epoch_duration_sec }` mirroré dans
  `RuntimeConfig` (4 champs `Option`, `None` = fallback boot `FeesSettings`).
  `EmissionGate` lit le couloir via `params_from_runtime`. Baisser ceiling+target =
  resserrage (instantané) ; toute hausse = desserrage (45 j). Rend le plafond
  10 %/an modifiable SEULEMENT par un processus Constitution annoncé + timelocké —
  la promesse plan §2.1. ⚠️ une fois gouverné, le couloir du `config.toml` est
  shadowé (la gouvernance est la source de vérité). Voir [[budget-emission]].
- **Asymétrie tighten/loosen** : `direction(update, config)` ; un **resserrage**
  (couper/réduire le mint, baisser un plafond) a un timelock **nul**
  (`enact_after == announced_at`, enact immédiat) ; un **desserrage** garde le délai
  plein du palier. Calculé par le protocole depuis la config courante — le proposant
  ne peut pas réclamer un timelock court pour un desserrage.
- **Kill-switch `mint_enabled`** : `EmissionGate::reserve` refuse toute émission
  (`MintDisabled`, code `5031`) quand `mint_enabled = false`. Faucet + bridge natif
  vérifient aussi (defense-in-depth).
- **Auto-enact** : une tâche scanne toutes les 60 s et enacte les propositions dont
  le timelock est écoulé (coordinator-only, idempotent, check read-only).
- **Fin du bypass** : `POST /admin/config` forge désormais un `GovernanceProposal`
  (palier auto = `min_tier`) au lieu d'appliquer en direct. Resserrage = instantané,
  desserrage = timelocké. Plus aucun chemin n'applique la config hors-DAG.

## Endpoints API

| Méthode | Path | Accès | Description |
|---------|------|-------|-------------|
| POST | `/admin/governance/propose` | admin (gated) | Annonce un changement timelocké |
| POST | `/admin/governance/enact/{id}` | admin (gated) | Applique après expiration (rejeté avant) |
| POST | `/admin/governance/cancel/{id}` | admin (gated) | Annule une proposition `Pending` |
| GET | `/v1/governance/pending` | **public** | Propositions en attente (l'annonce) |
| GET | `/v1/governance/history` | **public** | Propositions enacted / cancelled (audit) |
| POST | `/admin/config` | admin (gated) | **(P2)** forge un `GovernanceProposal` (palier auto). Resserrage → appliqué (`applied`), desserrage → `proposed` timelocké. Plus d'application instantanée hors-DAG |
| GET | `/admin/config` | admin (recovery) | Lecture de la `RuntimeConfig` courante |

## Tests

- `pms-storage` : `governance_proposal_roundtrip_and_status`, `tier_durations_golden`.
- `pms-core` (`tests/governance_timelock_test.rs`) : **G3** (enact après expiration
  applique le `ConfigUpdate`), **G2** (enact avant expiration rejeté, config
  inchangée), **DUP** (doublon `proposal_id` rejeté, record intact).
- `pms-server` (`tests/dag_sandbox.rs::test_governance_timelock_endpoints`) : e2e
  HTTP — propose → /pending → enact précoce rejeté (code 3071 + raison timelock) →
  cancel → /history.
- **(P2)** `pms-config` (`governance_policy` units) : `min_tier`/`direction`/
  `required_timelock_ms` golden + batch all-tighten/mixed/empty.
- **(P2)** `pms-core` (`governance_timelock_test.rs`) : **G4** (palier trop bas
  rejeté), **G3/G5** (tighten instantané applique `max_mint`), **G2**, **DUP**.
- **(P2)** `pms-server` : `emission_budget_test::t11` (**G6** kill-switch refuse les
  4 voies), `governance_autoenact_test` (tick enacte l'éligible, ignore le futur),
  `dag_sandbox::test_governance_tighten_instant_endpoints` (asymétrie e2e),
  `dag_sandbox::test_admin_config_governance_rewire` (bypass fermé).
- **(P3)** `pms-config` : `emission_corridor_is_constitution_and_directional` ;
  `pms-server` : `emission_budget_test::t12` (**G9** couloir gouverné — 20 %→5 %
  via `SetEmissionCorridor` change le budget de période).

## Interactions
Liens : [[budget-emission]] (la politique monétaire que la gouvernance protège),
[[smart-contracts]] (la frontière natif-vs-contrat que l'invariant garde),
[[config-system]] (`ConfigUpdate`/`apply_config_update` réutilisés), [[trust-model]]
(ce que le Coordinator peut/ne peut pas), [[storage-rocksdb]] (CF + migration).
