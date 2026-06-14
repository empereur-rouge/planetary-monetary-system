---
tags: [feature]
created: 2026-06-14
updated: 2026-06-14
version: v0.14.0
---

# Budget d'Émission Partagé

## Résumé

Politique monétaire gouvernée du PMS natif (plan §3.1). **Toutes les voies de mint
de PMS natif sur le ledger `main`** (baseline taux cible, on-ramp fiat, conversion
token→PMS, faucet) puisent dans **un seul budget d'émission par période** — la
règle non-négociable « N voies de mint sans N planches à billets ». Le budget est
borné par un **couloir dur** : `supply × clamp(taux_cible, plancher, plafond) ×
frac_année`. Même si la config pousse le taux cible au-delà du plafond, le budget
est plafonné — c'est la matérialisation en code de « règle inviolable que
l'opérateur ne peut pas franchir par surprise » (plan §2.1).

Deux propriétés de sécurité :
- **P1 — Plafond** : `Σ émission_période ≤ budget_période`. Garanti par le
  check-and-decrement atomique de `EmissionGate::reserve` sous mutex — ferme le
  TOCTOU que le chemin de forge concurrent (sans lock global) laissait ouvert.
- **P2 — Exactement-une-fois** : le compteur est persisté *avant* le forge
  (« counter-first »). Un crash ne peut causer qu'une sous-émission conservatrice
  auto-réparée au rollover d'epoch — jamais de dépassement.

**Phase 1 (v0.12.0)** : mécanisme + câblage du **baseline inflation mint** (qui
minte désormais le **résidu** du budget, `budget − déjà-émis`).
**Phase 2 (v0.13.0)** : **voie A on-ramp** fiat→PMS (`POST /admin/onramp`) — 2e
voie branchée sur le **même** budget (preuve qu'il est partagé), via
l'orchestrateur partagé `emit_native_gated` (baseline + on-ramp y passent).
**Phase 3 (v0.14.0)** : **voie B conversion token→PMS** en **smart contract** —
un burn de token custom (`POST /v1/wallet/token/burn`) déclenche un contrat
`OnTokenBurn{asset}` → action `MintNative{R}` → mint PMS au taux R, **sous le même
budget**. Synchrone dans le handler, **réserve-avant-burn** (atomicité / sûreté
des fonds). Le moteur ne hardcode jamais le token — le contrat porte la politique.
Voies contribution/contenu à venir. Voir `pms-spec-emission-budget.md` (spec) et
[[smart-contracts]] (le système de contrats).

## Configuration

Section `[fees]` (struct `FeesSettings`) :

| Champ | Défaut | Rôle |
|-------|--------|------|
| `annual_inflation_percent` | `3.0` | Taux cible (% / an) |
| `annual_ceiling_percent` | `10.0` | **Plafond DUR** du couloir (% / an) |
| `annual_floor_percent` | `0.0` | Plancher du couloir (% / an) |
| `emission_epoch_duration_sec` | `86400` | Durée d'une période (epoch) |
| `daily_inflation_enabled` | `false` | Active la tâche baseline |
| `daily_inflation_interval_sec` | `86400` | Cadence de la tâche baseline |

> **Testnet** : `emission_epoch_duration_sec = 120` pour garder une émission
> fréquente (chaque tick de 120 s = un epoch, budget time-proportionnel). Sans
> ça, l'epoch par défaut (1 j) ne minterait qu'une fois par jour malgré le tick.

Le couloir (`ceiling`/`floor`/`target`/`epoch`) était **boot-only** ; depuis
v0.17.0 il est **gouverné** via `ConfigUpdate::SetEmissionCorridor` (palier
Constitution, 45 j sur toute hausse) — mirroré dans `RuntimeConfig`, fallback boot
`FeesSettings` au premier démarrage. `EmissionGate` le lit via `params_from_runtime`.
Voir [[gouvernance-timelock]]. ⚠️ une fois gouverné, le couloir du `config.toml`
est shadowé (la gouvernance est la source de vérité).

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-server` | `src/emission.rs` | `EmissionGate`, `compute_epoch_budget`, `effective_rate_pct`, `EmissionEpochState`, `Voie` |
| `pms-server` | `src/emission_mint.rs` | Orchestrateur partagé `emit_native_gated` (reserve→forge→persist→release+jauges) |
| `pms-server` | `src/api_fn/onramp.rs` | Handler `POST /admin/onramp` (voie A fiat→PMS) |
| `pms-server` | `src/api_fn/token_burn.rs` | Handler `POST /v1/wallet/token/burn` (voie B : burn + conversion contract-driven) |
| `pms-contracts` | `src/engine.rs` | `evaluate_token_burn` + action `MintNative` (voie B : politique du taux R) |
| `pms-server` | `src/fee_distribution/inflation.rs` | Baseline mint en résidu, via l'orchestrateur |
| `pms-server` | `src/api_error.rs` | Code `5030` `EmissionBudgetExhausted` |
| `pms-server` | `src/api/state.rs` | Champ `emission_gate: Arc<EmissionGate>` sur `AppState` |
| `pms-server` | `src/metrics.rs` | Gauges/counters `pms_emission_*` |
| `pms-storage` | `src/rocks_store/emission_storage.rs` | Persistance `emission_epoch_state` (CF `node_fee_pool`) |
| `pms-config` | `src/config.rs` | Champs `annual_ceiling_percent`, `annual_floor_percent`, `emission_epoch_duration_sec` |

## Fonctions Clés

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `compute_epoch_budget` | `emission.rs` | Pur : `supply × clamp(taux)/100 × (epoch/an)` |
| `effective_rate_pct` | `emission.rs` | Pur : clamp du taux cible au couloir `[plancher, plafond]` |
| `EmissionGate::reserve` | `emission.rs` | Réserve atomique sous mutex (P1) + write counter-first (P2) |
| `EmissionGate::release` | `emission.rs` | Rollback de réservation si le forge échoue |
| `EmissionGate::load` | `emission.rs` | Recovery au boot depuis le store (jamais re-sommé) |
| `record/latest_emission_epoch_state` | `emission_storage.rs` | Persistance du singleton (clé dédiée, pas de nouveau CF) |
| `emit_native_gated` | `emission_mint.rs` | Orchestrateur partagé : reserve→forge→persist→release |
| `perform_daily_inflation_mint` | `inflation.rs` | Baseline : minte le résidu, split creator:treasury |
| `admin_onramp` | `onramp.rs` | Voie A : mint fiat→PMS sous budget, code 5030 si épuisé |

## Endpoints API

| Méthode | Path | Description |
|---------|------|-------------|
| `POST` | `/admin/onramp` | Mint PMS natif (voie A fiat→PMS), montant explicite sous budget. `{to, amount, payment_ref}` → `{block_id, minted}` ; `503/5030` si budget épuisé. `admin_writable`. |
| `POST` | `/v1/wallet/token/burn` | Burn de token (voie B) : `{private_key_b64, asset_id?, amount}` → burn + (si contrat `OnTokenBurn`) conversion en PMS au taux R sous budget. Réponse `{burned, converted_pms, mint_block_id}`. `auth_write`. |

## Métriques

| Métrique | Type | Description |
|----------|------|-------------|
| `pms_emission_budget_total{ledger}` | gauge | Budget de la période courante |
| `pms_emission_budget_consumed{ledger}` | gauge | Émis dans la période |
| `pms_emission_budget_remaining{ledger}` | gauge | Restant (alerte si proche 0) |
| `pms_emission_effective_rate{ledger}` | gauge | Taux effectif — **canari** : alerte si > plafond |
| `pms_emission_minted_total{ledger,voie}` | counter | Cumul émis par voie |
| `pms_emission_rejections_total{voie}` | counter | Mints refusés faute de budget (P1) |

## Tests

- **Purs** (`emission.rs`, `#[cfg(test)]`) : budget 2 % → `0.05479452`, **couloir
  50 %→clamp 10 % = `0.27397260`** (cœur de P1), clamp bidirectionnel, round-trip JSON.
- **Gate** (`crates/pms-server/tests/emission_budget_test.rs`) : exhaustion→rejet,
  TOCTOU concurrent (exactement 1 réussit), crash-reload, rollover forward-only,
  résidu, rollback. Lancer : `cargo test -p pms-server --test emission_budget_test -- --nocapture`.
- **On-ramp e2e** (`crates/pms-server/tests/dag_sandbox.rs::test_onramp_voie_a_emission_budget`) :
  faucet bootstrap → on-ramp 10 PMS (balance créditée) → on-ramp > budget → 503/5030,
  balance inchangée. Lancer : `cargo test --release -p pms-server --test dag_sandbox test_onramp_voie_a_emission_budget -- --ignored --nocapture`.
- **ApiError** (`api_error::tests`) : `codes_are_unique` (5030 unique) +
  `public_message_never_leaks_internal_detail` (montants jamais publics).

## Interactions

Lié à : [[economics]] (fee burn / `burn_rate_bps` — le burn déflationniste reste
côté fees, distinct de l'émission), [[fee-distribution]] (distribution de fees =
recyclage, **hors** budget d'émission), [[node-rewards]] (CF `node_fee_pool`
réutilisé pour la persistance), [[protocol-primitives]] (preuve de réserves —
proof-of-issuance à venir), [[storage-rocksdb]], [[metrics-monitoring]].
