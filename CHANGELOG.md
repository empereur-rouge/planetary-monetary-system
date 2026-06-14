# Changelog

All notable changes to the PMS DAG will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.17.1] - Unreleased — Fix test rot : bridge e2e protocol_version

### Fixed
- **fix(test/bridge)** — `bridge_e2e_test.rs` forgeait ses blocs Mint avec
  `protocol_version: 2` (hardcodé), alors que `config.dev.toml` est passé à `3`
  → `CoreAdapter::persist_block` rejetait avec « wrong network_id or
  protocol_version » et les 4 tests (`bridge_full_lifecycle`,
  `bridge_directional_atob`, `bridge_multiple_transfers`,
  `bridge_insufficient_balance`) étaient ROUGES. **Rot pré-existant** (confirmé
  identique sur v0.14.0, antérieur à toute la gouvernance §4) découvert par le
  run complet `scripts/run-tests.sh`. Aligné le `protocol_version` des `LedgerDef`
  du test sur la config (2 → 3). Suite désormais 100 % verte (29/29 crates).

---

## [0.17.0] - Unreleased — Gouvernance P3 : couloir d'émission sous gouvernance

Dernière phase de la gouvernance (`pms-spec-governance-timelock.md` §5) : le
**couloir d'émission** (`supply × clamp(target, floor, ceiling) × frac_année`),
jusqu'ici **boot-only** dans `FeesSettings`, devient **gouverné**. Il est mirroré
dans `RuntimeConfig` et modifiable par un `GovernanceProposal` de palier
**Constitution** (45 j). Cela rend enfin vraie la promesse `plan.md` §2.1 : le
plafond de 10 %/an n'est plus modifiable « par surprise » — seulement par un
processus annoncé + timelocké de 45 j, tracé dans le DAG.

### Added
- **feat(config)** — `ConfigUpdate::SetEmissionCorridor { ceiling_bps, floor_bps,
  target_bps, epoch_duration_sec }` ([runtime.rs](crates/pms-config/src/runtime.rs)) +
  4 champs `Option` mirroir dans `RuntimeConfig` (`emission_{ceiling,floor,target}_bps`,
  `emission_epoch_duration_sec`). `None` = pas encore gouverné → fallback boot
  (`FeesSettings`). bps pour garder `ConfigUpdate: Eq`. `apply_update` valide
  `floor <= target <= ceiling` et `epoch > 0`.
- **feat(governance)** — `SetEmissionCorridor` = palier **Constitution** dans
  `governance_policy::min_tier` ; direction asymétrique : baisser ceiling+target =
  resserrage (instantané), toute hausse = desserrage (45 j) ; premier passage sous
  gouvernance (couloir `None`) = desserrage (timelock plein, conservateur).
- **feat(emission)** — `params_from_runtime(settings, runtime)`
  ([emission_mint.rs](crates/pms-server/src/emission_mint.rs)) remplace
  `params_from_settings` : `EmissionGate` lit le couloir depuis `RuntimeConfig`
  (gouverné) avec fallback boot. Câblé dans `emit_native_gated` + `token_burn`
  (réservation + jauges).
- **test(unit)** — `governance_policy::emission_corridor_is_constitution_and_directional`
  (Constitution + lower=Tighten / raise=Loosen / first-set=Loosen, golden).
- **test(G9)** — `emission_budget_test::t12_emission_corridor_governs_budget` :
  `SetEmissionCorridor` 20 % → budget période 0.54794521 ; baisse à 5 % → 0.13698630
  (le couloir gouverne réellement le budget de `EmissionGate::reserve`).
- **chore(version)** — `Cargo.toml` 0.16.0 → **0.17.0** ; `DAG_VERSION` 3.5.0 →
  **3.6.0** (variante additive, pas de wipe) ; `API_VERSION` 20 → **21**.

---

## [0.16.0] - Unreleased — Gouvernance P2 : palier-min + asymétrie tighten/loosen

Deuxième phase de la gouvernance (`pms-spec-governance-timelock.md` §2-§3) : on
encode QUEL timelock s'applique à QUEL changement. Deux règles, validées au
protocole (donc inviolables au-delà du handler) :
- **Palier minimum par paramètre** — chaque `ConfigUpdate` exige un palier
  d'impact minimum (fees = Operator ; burn / distribution / pouvoir de mint =
  Policy ; le couloir d'émission = Constitution en P3). On ne peut pas déclasser
  un changement Policy en « Operator 7 j ».
- **Asymétrie tighten-now / loosen-later** — resserrer (couper / réduire le mint)
  est instantané (`enact_after == announced_at`) ; desserrer (reprendre / hausser)
  garde le délai plein du palier. On n'attend pas 45 j pour stopper une fuite.

**P2a** : table param→palier-min + direction + validation. **P2b** : câbler le
kill-switch `mint_enabled` (dormant jusqu'ici). **P2c (cette version)** : tâche
auto-enact + rewire `admin_update_config`→propose (fin du contournement du timelock).

### Added — P2c (auto-enact + fin du bypass `admin_update_config`)
- **feat(server/task)** — `spawn_governance_enact_task`
  ([crates/pms-server/src/api/tasks.rs](crates/pms-server/src/api/tasks.rs)) : scanne
  toutes les 60 s le CF `governance_proposals`, enacte les `Pending` dont le timelock
  est écoulé (`enact_after <= now`). Check `read_only.is_armed()` (règle tâche de
  fond produisant des blocs). Enact idempotent (statut ≠ Pending rejeté). Enregistrée
  dans `serve.rs`.
- **feat(api)** — **`POST /admin/config` ne s'applique PLUS instantanément** : il
  forge un `GovernanceProposal` ([admin.rs](crates/pms-server/src/admin.rs)) avec le
  palier auto-assigné (`min_tier`). Asymétrie : un **resserrage** est enacté
  immédiatement (200 `applied`, ancré DAG), un **desserrage** devient une proposition
  timelockée (202 `proposed`). C'est la **fermeture du contournement** qui rendait le
  timelock sans effet (un opérateur pouvait changer la config en direct).
- **refactor(governance)** — `do_propose`/`do_enact`/`ProposeOutcome`
  ([governance.rs](crates/pms-server/src/api_fn/governance.rs)) extraits + `pub`,
  partagés par les endpoints, le rewire admin, et la tâche auto-enact (un seul point
  de vérité pour le forge + l'asymétrie).
- **change(read-only)** — `POST /admin/config` passe de `admin_recovery` à
  `admin_writable` (il produit un bloc) ; **GET** reste recovery. CLAUDE.md mis à jour.
- **test(task)** — `governance_autoenact_test.rs` : le tick enacte une proposition
  éligible (tighten, `max_mint`→1, Enacted) et IGNORE une proposition future (loosen,
  reste Pending, config inchangée).
- **test(e2e)** — `test_admin_config_governance_rewire` (dag_sandbox) : via HTTP,
  tighten `/admin/config` → appliqué instantanément ; loosen → proposition timelockée,
  **config inchangée (bypass fermé)**, visible dans `/v1/governance/pending`.

### Added — P2b (kill-switch `mint_enabled`)
- **feat(emission)** — `EmissionGate::reserve`
  ([crates/pms-server/src/emission.rs](crates/pms-server/src/emission.rs)) refuse
  TOUTE réservation avec `EmissionError::MintDisabled` quand `mint_enabled = false`
  (chokepoint unique des voies budgétées : baseline, on-ramp, conversion token→PMS).
  Le champ `mint_enabled` était **dormant** (jamais lu) depuis sa création.
- **feat(emission)** — la **faucet** ([wallet_factory.rs](crates/pms-server/src/api_fn/wallet_factory.rs))
  et le **bridge** de PMS natif (`asset_id = None`, [bridge.rs](crates/pms-server/src/api_fn/bridge.rs))
  vérifient aussi `mint_enabled` (defense-in-depth : ces voies natives ne passent
  pas par `EmissionGate`).
- **feat(api/errors)** — nouveau code **`5031 MintDisabled`** (503)
  ([api_error.rs](crates/pms-server/src/api_error.rs)) : distinct du `5030`
  (budget épuisé, récupère à l'epoch suivant) — halt délibéré jusqu'à réactivation
  par la gouvernance. Mappé dans on-ramp + conversion.
- **test(G6)** — `t11_mint_disabled_killswitch_rejects_all_voies`
  ([emission_budget_test.rs](crates/pms-server/tests/emission_budget_test.rs)) :
  `SetMintEnabled{false}` (chemin gouverné) → `reserve` refuse les 4 voies
  (onramp/baseline/conversion/faucet) avec `MintDisabled`, rien réservé,
  réactivation rouvre le mint.
- **chore(version)** — `API_VERSION` reste 20 (gouvernance P2), doc étendue au 5031.

### Notes — couverture du kill-switch (audit complétude)
- **Couvert** : baseline, on-ramp, conversion token→PMS (via `EmissionGate::reserve`),
  faucet + bridge natif (checks dédiés).
- **Résiduels DORMANTS non gatés** (documentés, à fermer si activés) : (1) un contrat
  `AccumulateRefund` configuré pour rembourser du **PMS natif** (`asset_id=None`) —
  aujourd'hui aucun contrat ne le fait (edenite rembourse le token EDN custom) ;
  (2) le fee de mint de `admin_mint_token` en PMS natif — dormant (`mint_fee` = 0
  dans toutes les configs). La réconciliation lock↔mint du bridge (anti over-mint)
  est un durcissement séparé, hors P2.

### Added — P2a (politique de gouvernance)
- **feat(config)** — module `governance_policy`
  ([crates/pms-config/src/governance_policy.rs](crates/pms-config/src/governance_policy.rs)) :
  `min_tier(update)` (table §2), `direction(update, current)` (Tighten/Loosen),
  `required_timelock_ms(update, current, tier)` (0 si tighten, durée pleine sinon),
  `validate_tier(update, tier)` (rejet si palier déclaré < minimum). `GovernanceTier`
  dérive `Ord` (l'ordre des variants = impact croissant).
- **feat(core/validation)** — la validation persist d'un `GovernanceProposal`
  ([persist.rs](crates/pms-core/src/net_adapter/persist.rs)) impose désormais :
  (1) `tier >= min_tier(update)` (G4) ; (2) `enact_after == announced_at +
  required_timelock_ms(...)` re-dérivé depuis la config COURANTE (le proposant ne
  peut pas réclamer un timelock court pour un desserrage). Déterministe au
  replay/sync (la config des ancêtres est appliquée avant la proposition).
- **feat(api)** — `POST /admin/governance/propose` rejette en amont (`3071`) un
  palier trop bas et dérive `enact_after` via l'asymétrie (instantané pour un
  resserrage) — même fonction de vérité que la validation persist.
- **test(unit)** — `governance_policy` : `min_tier_table_golden`, `direction_*`,
  `required_timelock_asymmetry_golden`, `validate_tier_rejects_below_minimum`,
  `batch_tighten_only_if_all_tighten`, `tier_ordering`.
- **test(protocole)** — `governance_timelock_test.rs` : **G4** (palier trop bas
  rejeté), **G3/G5** (tighten enacté instantanément applique `max_mint_per_block`
  1_000_000→1 + Enacted), **G2** (loosen avant délai rejeté), **DUP**.
- **test(e2e)** — `test_governance_tighten_instant_endpoints` (dag_sandbox) :
  asymétrie tighten-now via HTTP — `enact_after == announced_at`, enact immédiat
  appliqué (Enacted).
- **chore(version)** — `Cargo.toml` 0.15.0 → **0.16.0** ; `DAG_VERSION` 3.4.0 →
  **3.5.0** (validation renforcée, MINOR, pas de wipe) ; `API_VERSION` 19 → **20**.

### Notes
- **Anti-backdating volontairement NON câblé au protocole** : un check
  `announced_at ≈ horloge` casserait le sync P2P / replay (la validation est
  ré-exécutée plus tard, horloge avancée → rejet des blocs historiques). Dans le
  modèle single-writer, seul le Coordinator (de confiance) forge les propositions
  et son handler estampille `announced_at = now` ; la garantie « pas de surprise »
  repose sur la TRANSPARENCE (le bloc proposal est observable dans le DAG en temps
  réel). La relation `enact_after == announced_at + durée` reste validée
  (déterministe).

---

## [0.15.0] - Unreleased — Gouvernance timelock (plan §4, cœur protocole)

Première phase de la gouvernance timelock (`pms-spec-governance-timelock.md`) :
tout changement de config gouverné passe par `GovernanceProposal → timelock →
GovernanceEnact`, **ancré dans le DAG** (annoncé, horodaté, signé Coordinator).
Le délai est **inviolable au protocole** (un enact avant expiration est rejeté).
**P1a (cette version)** : cœur protocole + storage + validation. P1b (à venir) :
endpoints HTTP + rewire `admin_update_config` → propose + tâche auto-enact + e2e.

### Added — P1a (cœur protocole)
- **feat(protocol)** — variantes `PlainPayload::Governance{Proposal,Enact,Cancel}`
  ([crates/pms-types-payload/src/payload.rs](crates/pms-types-payload/src/payload.rs)),
  coordinator-only. Proposal porte `{update: ConfigUpdate, tier, reason,
  announced_at, enact_after}` ; enact applique le `ConfigUpdate` après le délai ;
  cancel annule une proposition `Pending`.
- **feat(config)** — `GovernanceTier` (Operator 7 j / Policy 15 j / Constitution
  45 j) + `GovernanceStatus` + `GovernanceProposalRecord`
  ([crates/pms-config/src/governance.rs](crates/pms-config/src/governance.rs)).
- **feat(storage)** — trait `GovernanceStorage` (CF `governance_proposals`,
  ajouté à `EngineStorage`) : put/get/set_status/list. Migration `mig_10_to_11`,
  `CURRENT_VER` 10→11.
- **feat(core/validation)** — hot path persist
  ([persist.rs](crates/pms-core/src/net_adapter/persist.rs)) : proposal → stocke
  `Pending` ; **enact → REJETÉ si `now < enact_after`** (timelock inviolable) ou
  statut ≠ Pending, sinon applique via `apply_config_update` + marque `Enacted` ;
  cancel → `Cancelled`. Le check statut rend l'enact idempotent.
- **test(storage)** — `governance_proposal_roundtrip_and_status` + `tier_durations_golden`
  (durées 7/15/45 j golden ; round-trip + transitions de statut).
- **chore(version)** — `DAG_VERSION` 3.3.0 → **3.4.0** (payloads additifs, pas de
  wipe), `CURRENT_VER` 10 → **11** (CF), `Cargo.toml` 0.14.0 → **0.15.0**.

### Added — P1b (endpoints HTTP + e2e timelock)
- **feat(api)** — endpoints de gouvernance
  ([crates/pms-server/src/api_fn/governance.rs](crates/pms-server/src/api_fn/governance.rs)) :
  - `POST /admin/governance/propose` (admin, gated read-only) — annonce un
    `GovernanceProposal` (bloc DAG signé Coordinator), `enact_after = now +
    tier.duration` (7/15/45 j). N'applique RIEN. Renvoie `{proposal_id, block_id,
    enact_after_ms}`. `proposal_id = SHA-256(update + tier + announced_at)`.
  - `POST /admin/governance/enact/{id}` (admin, gated) — applique après expiration.
  - `POST /admin/governance/cancel/{id}` (admin, gated) — annule une `Pending`.
  - `GET /v1/governance/pending` + `GET /v1/governance/history` (**public**) —
    l'annonce et l'audit (droit de sortie : tout le monde voit ce qui se prépare).
- **feat(api/errors)** — nouveau code **`3071 GovernanceRejected`**
  ([api_error.rs](crates/pms-server/src/api_error.rs)) : raison **surfacée verbatim**
  (timelock non écoulé / statut non-pending / id inconnu / doublon) — surface
  opérateur authentifiée, raisons non-sensibles. Distingue le « pourquoi » d'un
  `Conflict` opaque.
- **feat(core/validation)** — garde d'unicité du `proposal_id` au niveau DAG
  ([persist.rs](crates/pms-core/src/net_adapter/persist.rs)) : un id déjà connu
  est rejeté (`already exists`), pas d'overwrite aveugle du record existant.
- **test(e2e)** — `test_governance_timelock_endpoints`
  ([dag_sandbox.rs](crates/pms-server/tests/dag_sandbox.rs)) : propose → annonce
  publique (G8) → enact précoce **REJETÉ avec code 3071 + raison timelock** (G2) →
  cancel → /history (G7).
- **test(protocole)** — `governance_timelock_test.rs`
  ([crates/pms-core/tests/governance_timelock_test.rs](crates/pms-core/tests/governance_timelock_test.rs)),
  `enact_after` contrôlé (impossible via endpoint, durées hardcodées) :
  **G3** enact après expiration applique le `ConfigUpdate` (fee_rate 300→4242,
  statut Enacted) ; **G2** enact avant expiration rejeté, config inchangée ;
  **DUP** doublon de `proposal_id` rejeté, record d'origine intact.
- **chore(version)** — `API_VERSION` 18 → **19** (routes gouvernance).

### Notes
- **Pas encore branché côté admin** : `admin_update_config` reste instantané ;
  son rewire en `propose` (timelocké) dépend de la table palier-min par paramètre
  (P2). Idem la tâche d'auto-enact (P2). Les durées de timelock sont hardcodées
  (`GovernanceTier::default_duration_ms`) ; surcharge config (testnet raccourci)
  en phase ultérieure.
- Spec : `pms-spec-governance-timelock.md` (§9 plan d'implémentation).

---

## [0.14.0] - Unreleased — Voie B : burn de token + évaluation contrat OnTokenBurn

**Voie B livrée et prouvée e2e** (conversion token custom→PMS, « scrip » = ex.
edenite) en **smart contract**. Conforme à l'invariant d'architecture (seul le PMS
natif est codé en dur ; tout le custom passe par contrat) — le moteur ne connaît
jamais « edenite ». **P1** : primitive de burn protocole. **P2** : évaluation
contrat `OnTokenBurn` + action `MintNative` + event. **P3** : câblage synchrone
burn→contrat→mint PMS sous budget (réserve-avant-burn, sûreté des fonds).

### Added — P3 (conversion synchrone burn→contrat→mint PMS)
- **feat(emission/conversion)** — `wallet_burn_token`
  ([crates/pms-server/src/api_fn/token_burn.rs](crates/pms-server/src/api_fn/token_burn.rs))
  exécute la voie B **synchronement** : évalue `OnTokenBurn{asset}` → **réserve le
  PMS sur le budget AVANT de brûler** (atomicité : budget épuisé ⇒ rejet complet,
  aucun burn, zéro perte de fonds) → brûle → minte le PMS au burner via
  `EmissionGate`. Gating read-only par la route. Réponse `{burned, converted_pms, mint_block_id}`.
- **feat(emission)** — `Voie::BridgeScrip` → **`Voie::TokenConversion`** (label
  métrique `token_conversion`) — la voie B est une conversion contract-driven, pas
  un bridge.
- **feat(metrics)** — `pms_emission_conversion_orphaned_total` : canari du cas rare
  burn-réussi / mint-échoué (réconciliation opérateur ; doit rester à 0).
- **test(sandbox)** — `test_voie_b_token_conversion` : token gold + contrat
  `OnTokenBurn{gold}`→`MintNative 3/2` → burn 10 gold ⇒ **15 PMS mintés** au burner
  sous budget ; gold supply −10. End-to-end HTTP, contract-driven.
- **docs** — `plan.md` §3.3-3.4 reframé (voie B = smart contract, plus un bridge
  hardcodé) ; conversion synchrone documentée (event `TokenBurnProcessed` =
  observabilité, aucun listener ne re-déclenche → pas de double-mint).

### Added — P2 (évaluation contrat OnTokenBurn → mint natif)
- **feat(contracts)** — action `ContractAction::MintNative { rate_numerator, rate_denominator }`
  ([crates/pms-types-contract/src/lib.rs](crates/pms-types-contract/src/lib.rs)) +
  validation (den≠0, num>0). Le contrat porte la POLITIQUE (le taux R) ; le mint
  natif est exécuté par le moteur sous budget.
- **feat(contracts)** — `evaluate_token_burn` + `MintNativeResult`
  ([crates/pms-contracts/src/engine.rs](crates/pms-contracts/src/engine.rs)) :
  miroir de `evaluate_nft_burn` pour les triggers `OnTokenBurn{asset_id}` →
  instructions de mint PMS natif (`burn × R`). Évaluation `OnTokenBurn`
  désormais implémentée (avant : « not yet implemented »). Endpoint
  `/admin/contracts/simulate` dry-run la voie B.
- **feat(storage)** — `ContractStorage::find_token_burn_contracts(asset_id, ledger_id)`
  (trait + impls InMemory & RocksStore) — match exact sur `OnTokenBurn{asset_id}`.
- **feat(event)** — `PmsEvent::TokenBurnProcessed { block_id, ledger_id, burner_address, asset_id, amount }`
  ([crates/pms-event/src/events.rs](crates/pms-event/src/events.rs)), émis par le
  handler de burn après persist (sur le bus contrat main, comme les burns NFT).
- **test(contracts)** — `test_simulate_token_burn_mint_native` : `OnTokenBurn{edenite}`
  + `MintNative 3/2` → 100 edenite ⇒ 150 PMS (golden). 27/27 pms-contracts verts.

### Added — P1 (primitive de burn protocole)
- **feat(protocol)** — nouvelle variante `PlainPayload::TokenBurn { tx, asset_id, amount, owner }`
  ([crates/pms-types-payload/src/payload.rs](crates/pms-types-payload/src/payload.rs)) :
  destruction permanente de token (la supply baisse). Owner-signé (le burner
  dépense ses propres UTXOs), bloc forgé par le Coordinator. Payload **PLAIN**
  (proof-of-burn transparent). `amount = Σ inputs − Σ change` détruit ; le change
  revient au burner.
- **feat(core/validation)** — `validate_token_burn_async`
  ([crates/pms-core/src/validations/transactions.rs](crates/pms-core/src/validations/transactions.rs)) :
  validation hot-path complète (signatures C-2, ownership C-1, time-lock 2.1,
  anti-double-spend) avec **conservation-burn** (`inputs = change + amount`,
  même asset, tout au `owner`, fee=0) au lieu de la conservation stricte M-7.
  Réutilise les helpers partagés (`fetch_input_outputs`, `verify_tx_signatures`,
  `check_spend_authorization`, `check_input_time_locks`).
- **feat(server)** — route `POST /v1/wallet/token/burn`
  ([crates/pms-server/src/api_fn/token_burn.rs](crates/pms-server/src/api_fn/token_burn.rs)) :
  handler de burn (coin selection + signature + forge), `ApiError` typé. Gated
  `auth_write` (read-only).
- **feat(activity)** — `ActivityCategory::Burn` + type `token_burn` (direction
  `out`) dans les index d'activité (storage + serveur).
- **test(sandbox)** — `test_token_burn_reduces_supply`
  ([crates/pms-server/tests/dag_sandbox.rs](crates/pms-server/tests/dag_sandbox.rs)) :
  burn 100 PMS → balance 1000→900 (change rendu), supply −100 (vraiment détruit),
  over-burn → 422 (`3001`), balance inchangée. End-to-end HTTP.

### Changed
- **chore(version)** — `DAG_VERSION` 3.2.0 → **3.3.0** (variante de payload
  additive, backward-compatible → migration auto, **pas de wipe**). `API_VERSION`
  17 → **18** (route burn). `Cargo.toml` 0.13.0 → **0.14.0** (MINOR).
- **docs(spec)** — `pms-spec-emission-budget.md` : voie B reframée en **smart
  contract** (`OnTokenBurn{asset_id}` → action mint-natif-sous-budget), plus un
  bridge hardcodé.

### Notes
- **Inerte côté contrats pour l'instant** : `OnTokenBurn` n'est pas encore évalué
  par le moteur ([engine.rs:563](crates/pms-contracts/src/engine.rs#L563)) ; le
  burn détruit le token mais ne déclenche aucun mint. Phase 2 : `TokenBurnProcessed`
  event + `evaluate_token_burn` + action `MintNative`. Phase 3 : sink + mint PMS
  via `EmissionGate`.

---

## [0.13.0] - Unreleased — Voie A on-ramp fiat→PMS (plan §3.2, sous budget partagé)

Deuxième voie de mint, branchée sur le **même** budget d'émission que la baseline
(v0.12.0) — la preuve que le budget est bien *partagé* (« N voies sans N planches
à billets »). Au passage, extraction de l'orchestrateur partagé `emit_native_gated`
(réclamé par l'audit altitude) que la baseline ET l'on-ramp utilisent.

### Added
- **feat(emission/onramp)** — `POST /admin/onramp` ([crates/pms-server/src/api_fn/onramp.rs](crates/pms-server/src/api_fn/onramp.rs)) :
  mint de PMS natif fiat→PMS (voie A) à travers le budget partagé. Montant
  **explicite**, refusé si > budget restant (P1). `payment_ref` = preuve de
  paiement attestée par l'opérateur (fiat encaissé hors-DAG, plan §6/§9),
  inscrite dans les métadonnées du bloc pour l'audit. Route `admin_writable`
  (gated read-only).
- **feat(emission)** — orchestrateur partagé `emit_native_gated`
  ([crates/pms-server/src/emission_mint.rs](crates/pms-server/src/emission_mint.rs)) :
  réserve (P1) → publie les jauges → forge → persiste → **release** sur tout
  échec. Réutilise `tx_helpers::{get_block_parents, forge_and_sign_block,
  persist_and_broadcast}`. Baseline et on-ramp y passent tous deux — un seul
  endroit pour le « dance » reserve/release (plus de copier-coller par voie).
- **feat(api-error)** — code stable `5030` `EmissionBudgetExhausted` (503,
  tranche resource/quota, message public vague, montants jamais exposés). Grille
  mise à jour dans [documentation/api/error-codes.md](documentation/api/error-codes.md).
- **test(sandbox)** — `test_onramp_voie_a_emission_budget` ([crates/pms-server/tests/dag_sandbox.rs](crates/pms-server/tests/dag_sandbox.rs)) :
  bootstrap faucet → on-ramp 10 PMS (balance créditée) → on-ramp > budget → 503
  code `5030`, balance inchangée (pas de sur-émission). End-to-end HTTP.

### Changed
- **refactor(server/inflation)** — `perform_daily_inflation_mint`
  ([crates/pms-server/src/fee_distribution/inflation.rs](crates/pms-server/src/fee_distribution/inflation.rs))
  passe par `emit_native_gated` (−200 lignes de forge hand-rollé). Comportement
  inchangé (résidu du budget, split creator:treasury), mais gagne au passage
  `pms_blocks_total` + `tps_tracker.record_block()` que l'ancien chemin oubliait.
- **chore(version)** — `API_VERSION` 16 → **17** (nouvelle route `/admin/onramp`
  + code `5030`). `Cargo.toml` 0.12.0 → **0.13.0** (MINOR). Pas de bump
  `DAG_VERSION`/`CURRENT_VER` (bloc `Mint` inchangé, CF inchangé).

### Notes
- **Faucet non gaté** : le faucet (dev/testnet) reste hors budget — utile pour
  amorcer la supply (le budget % est nul à supply 0). Sur mainnet le faucet est
  refusé, donc pas un trou.
- **Hors phase** (note-for-later des revues) : closure `build_outputs` faillible
  (`Result`), seam bas-niveau reserve/release pour la voie B (pont scrip, qui ne
  passe pas par `emit_native_gated`), couloir timelocké.

---

## [0.12.0] - Unreleased — Budget d'émission partagé (plan §3.1, phase 1)

Première brique de la politique monétaire gouvernée : un **budget d'émission par
période** que toutes les voies de mint de PMS natif sur le ledger main devront
partager (« N voies de mint sans N planches à billets »). Cette phase 1 met en
place le mécanisme et le câble sur le **baseline inflation mint** (taux cible),
désormais borné par un **couloir dur**. Voir [pms-spec-emission-budget.md](pms-spec-emission-budget.md)
et la fiche [[budget-emission]].

### Added
- **feat(emission)** — module [crates/pms-server/src/emission.rs](crates/pms-server/src/emission.rs) :
  `EmissionGate` (gate partagé sous `tokio::Mutex`), `compute_epoch_budget`
  (fonction pure `supply × clamp(taux, plancher, plafond) × frac_année`),
  `effective_rate_pct` (clamp du couloir), `Voie` (baseline/onramp/bridge/faucet),
  `EmissionEpochState` (compteur de période persisté). **P1 (plafond)** :
  `reserve()` fait un check-and-decrement atomique sous le mutex — ferme le
  TOCTOU que le chemin de forge concurrent (sans lock global) ne pouvait pas
  garantir. **P2 (exactement-une-fois)** : le compteur est persisté *avant* le
  forge (« counter-first »), un crash ne peut causer qu'une sous-émission
  conservatrice auto-réparée au rollover.
- **feat(storage)** — [crates/pms-storage/src/rocks_store/emission_storage.rs](crates/pms-storage/src/rocks_store/emission_storage.rs) :
  persistance du singleton `emission_epoch_state` (clé dédiée dans le CF
  `node_fee_pool`, pattern `reserve_snapshot` — **pas de nouveau CF ni de
  migration `CURRENT_VER`**).
- **feat(config)** — `FeesSettings` : `annual_ceiling_percent` (plafond dur,
  défaut 10 %/an), `annual_floor_percent` (plancher, défaut 0), et
  `emission_epoch_duration_sec` (durée d'epoch, défaut 86400 = 1 j). Tous
  `#[serde(default)]` — rétro-compatibles. `annual_inflation_percent` existant
  sert de taux cible.
- **feat(metrics)** — `pms_emission_budget_total/consumed/remaining`,
  `pms_emission_effective_rate` (canari du couloir : alerte si > plafond),
  `pms_emission_minted_total{voie}`, `pms_emission_rejections_total{voie}`.
- **test(emission)** — 5 tests purs (golden hardcodés : budget 2 % → `0.05479452`,
  **couloir 50 %→clamp 10 % = `0.27397260`**, round-trip JSON) +
  6 tests gate ([crates/pms-server/tests/emission_budget_test.rs](crates/pms-server/tests/emission_budget_test.rs) :
  exhaustion→rejet, TOCTOU concurrent, crash-reload, rollover forward-only,
  résidu, rollback). Prouvent P1 et P2 sur le vrai code.

### Changed
- **feat(server/inflation)** — `perform_daily_inflation_mint`
  ([crates/pms-server/src/fee_distribution/inflation.rs](crates/pms-server/src/fee_distribution/inflation.rs))
  passe désormais par le gate : il minte le **résidu** du budget
  (`budget − déjà-émis-par-les-voies`) au lieu de `supply × taux / 365`
  inconditionnel. **Conséquences** : (a) l'émission est plafonnée par le couloir
  même si la config pousse le taux au-delà ; (b) l'émission devient
  **proportionnelle au temps réel** (corrige le `/365` hardcodé découplé de
  l'intervalle) ; (c) le `burn_percent` n'est **plus appliqué** à l'inflation —
  le taux cible EST le taux de croissance net (le burn déflationniste reste dans
  le chemin des fees via `burn_rate_bps`) ; la répartition creator:treasury est
  renormalisée sur 100 % du montant minté.
- **chore(config/testnet)** — `config.testnet.toml` pose `emission_epoch_duration_sec = 120`
  pour conserver une émission fréquente (chaque tick de 120 s = un epoch),
  sinon l'epoch par défaut (1 j) ne minterait qu'une fois par jour.

### Notes
- **AppState** : nouveau champ `emission_gate: Arc<EmissionGate>` (8 sites de
  construction patchés : prod, internal API, testkit ×3, tests ×3).
- **Versions** : `Cargo.toml` 0.11.3 → **0.12.0** (MINOR). **Pas** de bump
  `DAG_VERSION` (bloc inchangé : `PlainPayload::Mint`), `CURRENT_VER` (CF réutilisé),
  ni `API_VERSION` (aucune route — l'on-ramp viendra en phase suivante).
- **Hors phase 1** (voir spec §9) : voies on-ramp/scrip gatées, orchestrateur
  `emit_gated` partagé, couloir timelocké (option gouvernance).

---

## [0.11.3] - Unreleased — Pruning DAG : protéger tous les tips actifs (audit S9, contrat durci)

### Changed
- **fix(core/prune/tips)** — `ConcurrentDag::prune_oldest`
  ([crates/pms-core/src/concurrent_dag/pruning.rs](crates/pms-core/src/concurrent_dag/pruning.rs))
  protège désormais **tous les tips actifs**, pas seulement le dernier. Avant :
  l'élagage retirait les blocs les plus anciens par ordre d'insertion, tips
  inclus (seul le tout dernier tip était épargné) — un tip de branche dont
  l'agent concurrent avait fini sa chaîne tôt se retrouvait « ancien » et était
  amputé du DAG RAM. Désormais les tips sont sautés (poussés en queue) tant que
  les blocs **non-tip** suffisent à atteindre la borne `max_blocks` — le cas
  nominal, la frontière étant une petite fraction du DAG et l'historique
  (non-tips) vivant de toute façon sur disque. **La RAM reste bornée** : le
  nombre de blocs retirés est inchangé (`current_len - max_blocks`), seule leur
  identité bascule vers l'historique non-tip ancien. Soupape anti-croissance :
  si les non-tips sont insuffisants (flood de tips orphelins d'agents morts —
  la source historique de croissance illimitée), les **tips les plus anciens**
  sont élagués pour combler le déficit, en gardant toujours **≥ 1** tip
  (continuité fee-distribution / parent-selection).
- **Dual-layer** : vérifié côté RocksDB — `trim_tips`
  ([crates/pms-storage/src/rocks_store/maintenance.rs](crates/pms-storage/src/rocks_store/maintenance.rs))
  garde déjà les `tip_limit` (256) tips réels les plus récents (+ nettoyage des
  zombies, plancher ≥ 1), et `remove_tip` protège le dernier tip. Les deux
  couches préservent donc les frontières actives avec un plancher ≥ 1 ; aucun
  changement de code RocksDB requis (couverture existante :
  `tips_respect_limit_with_trim_rocks`, `remove_tip_protects_last_tip_rocks`,
  `trim_tips_always_keeps_at_least_one_rocks`).

### Fixed
- **test(core)** — `test_concurrent_inserts_with_pruning`
  ([crates/pms-core/src/concurrent_dag/tests.rs](crates/pms-core/src/concurrent_dag/tests.rs))
  était **flaky** (échouait ~2-5/10 runs, même isolé) : il assertait que les 4
  tips de branche survivaient à l'élagage alors que l'ancien `prune_oldest` ne
  protégeait que le dernier. Sous entrelacement concurrent, un thread finissant
  tôt voyait son tip élagué → `thread N tip must survive` échouait par
  intermittence. Découvert par l'audit (test à faux signal). Désormais
  déterministe (12/12, 6/6 en parallèle) — `len=200`, `tips=[0,1,2,3]`. Ajout
  d'`println!` de diagnostic (règle « show test output ») et de la borne exacte.

### Added
- **test(core)** — `prune_under_tip_flood_evicts_oldest_tips_keeps_recent_and_bounds_ram`
  (nouveau, `concurrent_dag/tests.rs`) verrouille la soupape anti-croissance :
  51 blocs (1 non-tip + 50 tips), `max_blocks=10` → les 40 tips les plus
  anciens + le genesis sont élagués, les 10 plus récents survivent, RAM bornée
  à 10, ≥ 1 tip préservé.

## [0.11.2] - Unreleased — Tests failover/replay/déterminisme (audit S9, proof-of-reserves)

### Added
- **test(core)** — nouveau `crates/pms-core/tests/replay_determinism.rs` (audit
  Section 9). Verrouille l'invariant maître de preuve de réserves : **rejouer le
  DAG depuis le store reconstruit EXACTEMENT les mêmes soldes**.
  - `replay_from_store_reconstructs_identical_balances` : un adapter persiste
    3 mints + 1 transfert (A dépense ses 1000 → D, fee 0), puis un second
    adapter est reconstruit via LE VRAI chemin de redémarrage prod
    (`ConcurrentDag::bootstrap_from_store` + `RocksStore::iter_all_utxos` →
    `ShardedUtxoSet::add` → `rebuild_indexes`, identique à
    `pms-ledger/src/instance.rs`). Les soldes relus du disque égalent les
    soldes live ET des valeurs golden hardcodées — A=0 (l'UTXO dépensé ne
    ré-apparaît PAS : no ghost), B=2500, C=777, D=1000 (transfert non perdu),
    supply native=4277 (transfert fee 0 ⇒ supply inchangée). Attente de
    persistance par polling (pas de `sleep` fixe) + `flush_wal` → robuste au
    timing du background-persist sur FS externe.
  - `same_block_sequence_is_application_deterministic` : deux adapters
    indépendants appliquant la même séquence donnent des soldes identiques
    (déterminisme d'application isolé, sans disque ni timing).
- Reste de la Section 9 (crash-recovery mid-write par injection de panne ;
  failover Coordinator multi-writer) non couvert ici — relève du niveau
  serveur/P2P, à traiter séparément.

## [0.11.1] - Unreleased — Fix idempotence: double-apply du delta UTXO sur re-soumission (audit S4)

### Fixed
- **fix(core/persist/idempotence)** — `CoreAdapter::persist_block`
  ([crates/pms-core/src/net_adapter/persist.rs](crates/pms-core/src/net_adapter/persist.rs))
  appliquait le `UtxoDelta` (`apply_diff`) **AVANT** la déduplication
  `contains_block → AlreadyExists`. Conséquence : re-soumettre un bloc déjà
  présent (re-gossip réseau, retry client, replay malveillant) **ré-appliquait
  son delta UTXO** — la supply doublait / les UTXOs étaient re-crédités à chaque
  re-soumission, tout en renvoyant `AlreadyExists` qui MASQUAIT la mutation et
  faisait diverger l'état RAM du disque (le persist disque, gated par le même
  early-return, n'était lui jamais ré-écrit). Même classe que le double-apply
  v0.7.20 (`admin_mint_token`), mais au niveau du hot path pour TOUT payload
  plain à delta. **Fix** : le check `contains_block → AlreadyExists` est déplacé
  AVANT `apply_diff`, en préservant l'ordre délibéré « UTXO-update-first »
  (anti-double-spend) pour les blocs réellement nouveaux. Reproduit puis
  verrouillé par `duplicate_block_is_idempotent_no_double_apply`
  (`persist #1 → Inserted bal=1000 | persist #2 → AlreadyExists bal=1000`).
  Résiduel connu : une fenêtre concurrente étroite (deux threads persistant le
  MÊME id de bloc neuf, passant tous deux `contains_block` avant insertion)
  reste — bornée par le gap check→insert préexistant ; sa fermeture complète
  exige un verrou par-id ou un réordonnancement insert-avant-apply (qui
  casserait l'ordre anti-double-spend), à traiter en hardening séparé.

### Added
- **test(core)** — nouveau `crates/pms-core/tests/dag_integrity.rs` (audit
  Section 4, méthode red-first) : 3 invariants d'intégrité du DAG soumis au
  VRAI chemin de persistance (`CoreAdapter::persist_block`) —
  `block_id_not_matching_canonical_hash_rejected` (M-6 : id ≠ hash canonique
  rejeté), `duplicate_block_is_idempotent_no_double_apply` (le test qui a
  démasqué le double-apply ci-dessus) et
  `block_with_unknown_parent_not_silently_applied` (parent inexistant rejeté
  par `enforce_parent_existence`, motif de rejet asserté spécifiquement —
  anti-faux-test rule #6).

## [0.11.0] - Unreleased — Mint adossé à une réserve collatérale (plan 2.3 v2)

### Added
- **feat(protocol)**: mint collatéralisé — `TokenMetadata` gagne
  `collateral_address` (adresse de réserve, même ledger),
  `collateral_asset_id` (None = natif) et `collateral_ratio_bps` (requis avec
  l'adresse, validé > 0 au registry). Quand défini, le hot path enforce à
  CHAQUE mint l'invariant CONTINU : `(circulating + minted) ×
  ratio_bps / 10000 <= somme des UTXOs de réserve ENCORE time-lockés`
  (`locked_until > now`, s'appuie sur le time-lock 2.1). Les UTXOs au lock
  expiré ou sans lock ne comptent PAS (l'émetteur pourrait les retirer) ;
  l'invariant porte sur l'émission TOTALE — pas de référence d'UTXO dans le
  payload, donc pas de double-comptage d'une même réserve entre mints, et
  une réserve qui expire bloque les mints suivants jusqu'au re-lock.
  Nouvelle erreur `ValidationError::InsufficientCollateral`. Helper pur
  `sum_locked_collateral` dans `validations/mint.rs`.
- **feat(api)**: `POST /admin/faucet` accepte `locked_until` (timestamp UNIX
  ms) — mint un UTXO time-locké (vesting, constitution de réserve de
  collatéral). `POST /admin/tokens/create` accepte `collateral_address` /
  `collateral_asset_id` / `collateral_ratio_bps`.
- **test(core)**: 6 tests unit dans `mint_constraints.rs` (couverture
  exacte/dépassement/ratio 150 %/réserve vide/filtrage expiré+sans-lock+
  mauvais-asset) + sandbox e2e `test_collateralized_mint_lifecycle`
  (réserve 1000 lockée 24h + 500 non lockés ignorés → exactement 1000
  mintables à 1:1, over-mint et dust rejetés 422).

### Changed
- **Versions** : workspace `0.10.0` → `0.11.0` ; `DAG_VERSION` `3.1.0` →
  `3.2.0` (règle de validation additive, auto-migrating) ; `API_VERSION`
  `15` → `16`. P2P `protocol_version` inchangé (`3`) — champs serde
  additifs tolérés au wire. `CURRENT_VER` schéma inchangé.

---

## [0.10.0] - Unreleased — Primitives protocole DAG : time-lock, spend conditions, mint contraint, demurrage, preuve de réserves (plan §2)

Implémentation complète de la section 2 du plan protocole (`plan.md`). Les
prérequis bloquants C-1/C-2 étaient déjà couverts par l'audit v0.9.0
(`validate_transaction_full` canonique) — vérifié avant toute feature.

### Added
- **feat(protocol) 2.1 — Time-lock natif sur UTXO** : `TxOutput.locked_until`
  (timestamp UNIX ms, optionnel). Un input encore verrouillé est rejeté
  (`ValidationError::OutputTimeLocked{input_index, until, now}`) dans les DEUX
  chemins de validation (hot path `validate_transaction_full` + legacy
  `utxo_sufficient_funds`). La sélection de coins exclut les UTXOs verrouillés.
  Constructeurs `TxOutput::new` / `new_locked` stabilisent ~155 sites.
- **feat(protocol) 2.2 — Spend conditions** : `SpendCondition` portée par
  l'output (pattern scriptPubKey) — `PubKey` (binding C-1 historique),
  `MultiSig{m, pubkeys}` (quorum M-of-N, adresse canonique `msig1…` qui engage
  la policy : SHA-256 domain-séparé `pms-multisig-v1`, set trié), `HashLock
  {hash_hex}` (révélation de préimage SHA-256). `Unlock` gagne `cosigners`
  (signatures vérifiées crypto dans `verify_tx_signatures`, dédupliquées anti
  quorum-stuffing) et `preimage_hex`. Erreurs `InvalidSpendCondition` (création,
  message spécifique) / `SpendConditionNotMet` (dépense, vague anti-enumeration).
  Module `pms-core/src/validations/conditions.rs`.
- **feat(protocol) 2.3/2.4 — Mint contraint per-asset** : le hot path enforce
  désormais `TokenMetadata` pour chaque asset custom minté ENREGISTRÉ —
  `signer == mint_authority` (`UnauthorizedTokenMint`, en PLUS du gate
  Coordinator), granularité `decimals` (`InvalidAmount`), `circulating +
  minted <= max_supply` (`MaxSupplyExceeded`, supply cache, sommé
  multi-outputs). Avant : enforcement API-only, contournable par tout
  producteur de bloc. L'enregistrement via `TokenCreate` est l'OPT-IN des
  contraintes : un asset sans metadata garde le comportement historique
  (gate Coordinator seul) — indispensable pour les refunds de contrats
  (edenite-cube-burn) qui mintent des assets non enregistrés. Nouveau trait
  `pms_storage::TokenRegistryStorage`.
- **feat(protocol) 2.5 — Demurrage opt-in par asset** :
  `TokenMetadata.demurrage_bps_per_day` (exposé sur `POST /admin/tokens/create`,
  validé ≤ 10000). Chaque UTXO créé est estampillé `created_at` par le SYSTÈME
  au persist (anti-antidatage). Valeur effective calculée à la lecture :
  `amount − amount×bps×jours_pleins/10000` (plancher 0). Conservation
  `out ≤ effective_in` pour les assets à demurrage (décote brûlée implicitement,
  la supply circulante décroît) ; conservation STRICTE M-7 inchangée sinon.
  UTXOs pré-upgrade (`created_at=None`) ne décotent pas. Module
  `pms-core/src/validations/demurrage.rs`.
- **feat(protocol) 2.6 — Preuve de réserves ancrée** :
  `PlainPayload::ReserveSnapshot{state_root, total_supply, utxo_count,
  computed_at_ms}` coordinator-only. `state_root` = SHA-256 domain-séparé
  (`pms-reserves-v1`) de l'itération ordonnée clé+valeur du CF `utxo` (un seul
  itérateur RocksDB = vue point-in-time consistante). Tâche périodique
  `spawn_reserve_snapshot_task` (config `[reserves] enabled/interval_secs`,
  désactivée par défaut, check read-only). Endpoints : `GET /v1/reserves/latest`
  (public), `POST /admin/reserves/snapshot` (admin_writable), `POST
  /admin/reserves/verify` (admin_recovery). Pointeur de commodité dans le CF
  `last_ms` (pas de nouveau CF, pas de migration).
- **test(core)**: nouvelles suites `timelock.rs` (6), `spend_conditions.rs` (13),
  `mint_constraints.rs` (10), `demurrage_validation.rs` (7) + unit tests
  conditions/demurrage + round-trip RocksDB (`rocks_utxo.rs`) + sandbox e2e
  `test_reserve_snapshot_anchor_and_verify`.

### Changed
- **refactor(storage+core)**: `UtxoDelta.create` et `NetDagAdapter::add_utxo`
  transportent désormais le `TxOutput` COMPLET (au lieu de tuples
  addr/amount/asset) — élimine structurellement le piège « champ d'output perdu
  entre validation et stockage ». Point d'écriture unique du CF `utxo` :
  `UtxoValue::encode_output` (champs `lkd`/`cond`/`cat` optionnels,
  rétro-compatibles sans migration).
- **`validate_transaction_full`** prend `now_ms` (horloge explicite, testable)
  et `demurrage_rates` (résolus du token registry par le hot path).
- **Versions** : workspace `0.9.8` → `0.10.0` ; `DAG_VERSION` `3.0.0` → `3.1.0`
  (additif, auto-migrating — les UTXOs/blocs existants restent valides) ;
  `API_VERSION` `14` → `15` (endpoints reserves + champ demurrage) ;
  `protocol_version` P2P `2` → `3` (nouveaux champs wire `TxOutput` + variante
  `ReserveSnapshot`). `CURRENT_VER` schéma DB inchangé (`10`) — aucun nouveau CF.

### Limitations connues (documentées)
- Les handlers custodiaux (`send-simple`, `send_asset`) ne calculent pas encore
  la décote demurrage côté serveur : les assets à demurrage se dépensent via
  des transactions client-signées (`/wallet/tx/send`) dont le client calcule la
  valeur effective (formule en jours pleins, reproductible).
- Le chemin encrypted (`wallet_send_tx` pré-chiffrement) applique la
  conservation stricte — assets à demurrage supportés sur le chemin plain.

---

## [0.9.8] - Unreleased — `encrypted_utxo_delta_test` réhabilité (trouvé par le runner) + `sign_tx_inputs` promu au testkit

Le runner v0.9.7 (`scripts/run-tests.sh`) a immédiatement fait son travail : il a
trouvé des tests rouges manqués (même classe de rot tx v0.9.0 que les transferts
activity).

### Fixed
- **test(server)**: `encrypted_utxo_delta_test.rs` (2) et `wallet_send_fees.rs` (3 :
  `wallet_send_tx_injects_fee_and_admin_can_decrypt_fee_utxo`,
  `wallet_send_tx_fee_is_materialized_and_zeroed_and_visible_to_admin`,
  `wallet_send_tx_does_not_duplicate_fee_output_if_already_present`) postaient des
  tx via `/wallet/tx/send` avec `"unlocks": []` → rejetées par la validation
  canonique v0.9.0 ("transaction authorization invalid"). Tx désormais signées
  (par le wallet propriétaire de l'UTXO ; dans la chaîne encrypted, TX2 signée par
  Bob, propriétaire de l'output de TX1 dépensé).
- **test(server)**: `healthz_enriched::healthz_returns_structured_json_with_four_checks`
  attendait 4 checks `/healthz` alors que `read_only_mode` (safety-valve v0.7.23) en
  a ajouté un 5e — assertion stale, silencieusement rouge. Mis à jour à 5 checks +
  assert de `read_only_mode`.

### Changed
- **testkit**: `sign_tx_inputs(wallet, tx, network_id)` promu de helper local
  (activity_e2e) à helper PARTAGÉ `pms_testkit::sign_tx_inputs` (dans `block.rs`).
  `activity_e2e` l'importe désormais au lieu de le dupliquer. Tout test forgeant
  une `TxUtxo` doit l'utiliser (cf. CLAUDE.md § Anti-faux-tests).
- **Workspace** `0.9.7` → `0.9.8`. API_VERSION inchangé (`14`).

---

## [0.9.7] - Unreleased — Garde-fous anti-régression (règles tests + runner)

Pour empêcher la ré-apparition des problèmes corrigés en v0.9.3→v0.9.6 (faux
tests, tests morts non-compilants, rot après durcissement de validation, footgun
`Default` dérivé vs serde).

### Added
- **docs(CLAUDE.md)**: section `### Anti-faux-tests` (taxonomie des 7 anti-patterns
  bannis + vérification obligatoire « lancer isolé et montrer la sortie » + garder
  les tests verts quand on durcit validation/auth/protocole + ne pas hand-builder
  `Settings` + lancer par-crate).
- **docs(CLAUDE.md)**: Critical Pattern `### Config Default — derived vs serde` :
  toute struct config avec `#[serde(default = "fn")]` doit impl `Default`
  MANUELLEMENT (sinon `Struct::default()` diverge → cf. footgun P2pConfig v0.9.6).
- **tooling**: `scripts/run-tests.sh` — lance la suite crate-par-crate (PMS_CONFIG
  posé, simulateur hors-workspace inclus), résumé PASS/FAIL, exit non-nul si échec.
  Évite le flake I/O du `cargo test --workspace` sur FS externe. La réponse en une
  commande à « est-ce que la suite est verte ? ».

### Changed
- **Workspace** `0.9.6` → `0.9.7`. API_VERSION inchangé (`14`).

---

## [0.9.6] - Unreleased — Fix footgun `P2pConfig::default()` (réhabilitation P2P)

### Fixed
- **fix(config/P2pConfig)**: `P2pConfig::default()` utilisait le `Default` DÉRIVÉ
  qui mettait `max_connections = 0` et `per_peer_queue_cap = 0`. Conséquences :
  le listener P2P rejetait TOUTE connexion (`conn_semaphore` à 0 permis →
  connexion fermée avant le handshake → « eof before hello » côté client), et
  `mpsc::channel(0)` paniquait à la connexion d'un peer. En production les valeurs
  venaient du défaut serde (256 / 2000), mais tout code construisant
  `P2pConfig::default()` en mémoire (tests P2P, `Server` API-only) tombait dans le
  piège. `impl Default` manuel délégant désormais aux mêmes fonctions que les
  `#[serde(default = ...)]` → cohérent avec une config chargée sans bloc `[p2p]`.
- **test(network)**: les 4 tests de `pms-network/tests/anti_abuse.rs`
  (`ping_pong_still_works`, `oversize_message_is_dropped_connection`,
  `too_many_parse_errors_kicks_peer`, `rate_limit_drops_or_closes_under_burst`)
  passent désormais (ils échouaient sur « eof before hello » à cause du footgun).
  Toute la suite `pms-network` est verte.

### Changed
- **test(server)**: `network_batching.rs` n'a plus besoin de son contournement
  explicite de `per_peer_queue_cap`/`max_connections` — utilise
  `P2pConfig::default()` (maintenant correct).
- **Workspace** `0.9.5` → `0.9.6`. API_VERSION inchangé (`14`).

---

## [0.9.5] - Unreleased — Réhabilitation des tests d'activité (validation tx canonique v0.9.0)

Dernière couche de la réhabilitation : les tests d'activité basés sur des
transferts forgeaient des `TxUtxo` sans unlocks et étaient rejetés par la
validation canonique v0.9.0. Tests-only (aucun changement de prod).

### Fixed
- **test(activity)**: les 6 tests transfert d'`activity_e2e.rs` passent désormais.
  - Helper `sign_tx_inputs(wallet, tx, network_id)` : un `Unlock` par input signé
    sur `tx.signing_message(network_id)` (exigé par `validate_transaction_full` :
    `unlocks.len()==inputs.len()`, pubkey↔owner, signature valide).
  - `activity_transfer_with_change` / `activity_transfer_self` /
    `activity_reverse_received_appears` (forge directe) : tx signée par le wallet
    propriétaire de l'UTXO avant forge. `reverse_received` dépendait du bloc
    "original" — qui ne persistait plus faute d'unlocks.
  - `activity_transfer_in_encrypted` / `activity_fee_received_appears` (handler
    `/wallet/tx/send`, qui ne signe PAS côté serveur) : la tx est signée
    côté test et les unlocks embarqués dans le body.
  - `activity_bridge_lock_in_appears` : passe par `setup_admin_ctx` +
    `make_test_ctx_with_admin` pour que `node_wallet` soit l'admin/coordinateur
    (mint autorisé + BridgeLock coordinator-only autorisé).
- **test(core)**: `utxo_lru_test::lru_utxos_by_address_with_fallback` réécrit en
  `lru_utxos_by_address_and_get_fallback`. L'ancien supposait que
  `utxos_by_address` retrouve les UTXO ÉVINCÉS via fallback, alors que `add()`
  retire intentionnellement les entrées évincées de l'`address_index` (design
  v0.6.3, mémoire bornée). Le vrai fallback est sur `get()` (cache miss → store).
  De plus `new(256)` donne 1 UTXO/shard (les 2 OID partageaient le préfixe txid).
  Réécrit pour tester le comportement RÉEL : (A) sans éviction → 2 UTXO ; (B) le
  fallback `get()` retrouve un UTXO évincé, mais `utxos_by_address` ne voit plus
  que les entrées indexées.

### Changed
- **Workspace** `0.9.4` → `0.9.5`. API_VERSION inchangé (`14`).

### Known issues (couches pré-existantes restantes — hors scope)
- Sous `cargo test --workspace` (parallélisme maximal sur FS externe), des tests
  P2P timing-sensibles de `pms-network` (`anti_abuse.rs` : "eof before hello") et
  des lectures de config peuvent échouer par contention I/O. Ils passent isolés
  et sur un FS local. Mitigations : `PMS_CONFIG=$(pwd)/etc/config/config.dev.toml`
  et/ou parallélisme réduit. `anti_abuse` (handshake P2P) échoue aussi sur `main`
  isolé — réhabilitation P2P séparée à venir.

---

## [0.9.4] - Unreleased — Réhabilitation de tests pré-existants cassés + fix robustesse config

Suite de l'audit v0.9.3 : correction des tests RED découverts à l'exécution
(les agents read-only les avaient ratés faute de compiler/exécuter). Inclut un
vrai fix de robustesse prod sur le chargement de config.

### Fixed
- **fix(config)**: `load_config` canonicalise désormais le `repo_root` au lieu
  d'utiliser un chemin absolu contenant `../..`. Certains backends (config-rs
  sur FS externe / chemins avec espaces, ex.
  `/Volumes/Crutial X9 .../crates/pms-config/../../etc/config`) ne résolvaient
  pas ce chemin de façon fiable, et comme la source est `required(false)` le
  fichier était silencieusement ignoré → `missing field rocks` →
  **toute la suite de tests devenait flaky/rouge** par intermittence. Fallback
  sur le chemin brut si la canonicalisation échoue. Fichier :
  `crates/pms-config/src/settings.rs`.
- **test(core)**: `bootstrap_from_store` forgeait des blocs à 2 parents
  (`top_tips(2)`) → rejetés par l'enforcement single-writer durci en v0.9.0
  ("must have exactly 1 parent"). Passé en chaîne single-parent (`top_tips(1)`),
  qui teste tout aussi bien la recovery + children_count au bootstrap.
- **test(compliance)**: les 6 tests HTTP admin (`/admin/compliance/{log,frozen,
  shadow_balance}` + freeze/unfreeze) renvoyaient `401` — ils POSTaient sans
  token alors que les handlers re-vérifient `is_admin_authorized` en interne.
  Configurent maintenant `PMS_ADMIN_TOKEN_DEV` avant `make_test_ctx` + envoient
  le Bearer. 14/14 verts.
- **test(activity)**: `activity_freeze_appears` / `activity_unfreeze_appears` /
  `activity_seize_and_seize_received` (admin-gated) idem — passent désormais.

### Changed
- **Workspace** `0.9.3` → `0.9.4`. API_VERSION inchangé (`14`).

### Known issues (rot pré-existant v0.9.0, remédiation séparée à venir)
- Les tests d'activité **basés sur des transferts** (`activity_transfer_self`,
  `activity_transfer_with_change`, `activity_transfer_in_encrypted`,
  `activity_fee_received_appears`, `activity_bridge_lock_in_appears`,
  `activity_reverse_received_appears`) échouent sur la validation tx canonique
  v0.9.0 ("transaction authorization invalid" / "inputs/unlocks count
  mismatch") : ils forgent des tx sans unlocks valides. Réhabilitation = passe
  dédiée (re-signer les tx de test). De même `lru_utxos_by_address_with_fallback`
  et une non-déterminisme d'ordre/env entre tests parallèles restent à traiter.

---

## [0.9.3] - Unreleased — Test-suite audit : faux tests éliminés, gaps critiques comblés

Audit complet de la suite de tests (793 fonctions) : détection des tests qui
passent quoi qu'il arrive, qui testent une copie du code de prod, ou dont
l'assertion est trop molle pour attraper une régression. Aucun changement de
comportement de prod sauf l'ajout additif d'`Amount::checked_sub`.

### Added
- **test(api)**: `crates/pms-server/tests/version_endpoint.rs` — frappe le vrai
  `GET /v1/version` via le router et assert `api_version=14`, semver
  `software_version`, et les champs `dag_version`/`schema_version`/
  `protocol_version` (l'unit test ne comparait que `API_VERSION` à elle-même).
- **test(storage)**: test de préservation de données à travers les migrations
  (`migration.rs`) — écrit des blocs à la version N, vide les CF d'index, rejoue
  `ensure_schema`, et prouve que les blocs survivent + que `mig_2→3`/`mig_3→4`
  reconstruisent `by_time`/`id2ts`/`addr_activity`.
- **test(fees)**: no-loss-on-failure de `FeePool::merge_from` (swap atomique →
  échec persist → restauration, incl. fenêtre concurrente 160+7=167) ; split
  treasury 65/35 + fallback treasury-vide→coordinateur + skip dust via
  `compute_fee_outputs`.
- **test(consensus)**: signature présente-mais-invalide (bien formée mais sur un
  autre message) et usurpation de pubkey rejetées par `verify_block_signature` ;
  mint non-autorisé rejeté par le gate persist (`submit_block_auth.rs`).
- **test(crypto)**: isolation X25519 — une clé étrangère ne déchiffre pas le bloc
  d'un autre (`history_test.rs`), avec contrôle positif du destinataire prévu.
- **test(wallet)**: pagination via le vrai handler `get_plain_history`
  (`history_separation_test.rs`).
- **token**: `Amount::checked_sub` — soustraction vérifiée renvoyant `None` si le
  résultat serait négatif (l'opérateur `Sub` brut produit un solde négatif
  silencieux). Tests d'edge-cases ajoutés : underflow, arrondi banker's,
  div-par-zéro (panic), overflow mul (panic), round-trip parse/format.
- **testkit**: `make_test_app_with_ip_allowlist(admin_token, cidrs)` pour piloter
  le vrai middleware `require_local_or_admin`.

### Changed
- **test(security)**: `ip_allowlist.rs` réécrit pour tester le VRAI middleware
  (avant : une COPIE locale `is_ip_allowed`/`test_admin_middleware`) — assert le
  code d'erreur (1030 IP vs 1001 token), l'ordre IP-avant-token, et le bypass
  loopback. `network_batching.rs` réécrit avec un peer inbound réel (duplex) qui
  collecte les `Inv` (avant : 0 assertion, `last_inv_size` jamais lu).
  `single_writer_enforcement.rs` : test de comparaison de clé tautologique (et
  qui affirmait l'INVERSE de la prod, case-sensitive) remplacé par un appel réel
  à `validate_payload_authority`.
- **test**: assertions renforcées — `wallet_balance_fast` assert la valeur 25
  (pas `json1==json2`, qui passerait avec deux "0") ; `fee_consistency` golden
  `0.1500001`/`0.00106489` (pas `f(x)==f(x)`) ; simulator pin
  `DEFAULT_DIVISOR==13700` + reward golden + clés obfusquées documentées ;
  `node_rewards_test` recentré sur les primitives de stockage (ne ré-implémente
  plus la formule de distribution).
- **Workspace** `0.9.2` → `0.9.3`. API_VERSION inchangé (`14` — aucune route REST
  modifiée).

### Removed
- **test**: `block_rewards_test.rs`, `fee_treasury_test.rs` (docker `#[ignore]`,
  0 assertion, supersédés par les tests de fee distribution du `dag_sandbox`) ;
  `history_core.rs` (testait un `FakeStore` + une copie locale du handler, pas le
  code de prod — remplacé par un vrai test de pagination).

### Fixed
- **test**: 3 fichiers de tests ne compilaient plus (donc jamais exécutés en CI) —
  `bridge_test.rs`, `bridge_e2e_test.rs`, `addr_activity_test.rs` : champs de
  config manquants (`health`, `auto_reindex_activity_items`, `AAD.binding`,
  `SecretSettings.*`) + `protocol_version` obsolète (1 vs config 2). Réparés,
  compilent et passent.

### Known issues (pré-existant, hors scope de cette passe)
- Plusieurs tests HTTP admin-gated (`compliance_test.rs`,
  `activity_e2e.rs::activity_seize_*`) échouent en `401` : ils font POST vers
  `/admin/*` sans token et le harnais configure `admin_token=None`. Le helper
  `post_json_admin` existe (v0.9.2) mais ces fichiers utilisent leur propre
  `post_json` sans token. À migrer dans une passe dédiée.

---

## [0.9.2] - Unreleased — H-5 : wallet restore endpoints admin-gated

### Changed
- **fix(security/H-5)**: les endpoints de restauration de wallet — qui
  acceptent un secret long-terme de l'utilisateur (mnémonique BIP39 / clé
  privée hex) dans le body et le renvoient — passent de
  `POST /v1/wallet/restore/{mnemonic,private-key}` (gated API key) à
  `POST /admin/wallet/restore/{mnemonic,private-key}` (gated
  `require_local_or_admin`, catégorie `admin_recovery` — aucun bloc DAG
  produit). Restreindre ces helpers custodial au credential opérateur réduit
  la surface où un secret utilisateur traverse la frontière de confiance
  (gateway qui termine le TLS, logs serveur). L'ancien chemin `/v1/...`
  renvoie désormais 404. Doc-comments ajoutées : « NE JAMAIS logger le body ».
- **API_VERSION** `13` → `14` (changement de route + auth).
- **Workspace** `0.9.1` → `0.9.2`.

### Tests
- `crates/pms-server/tests/wallet_x25519_sk.rs` : nouveau
  `wallet_restore_mnemonic_requires_admin` (appel remote sans token → 401,
  ancien chemin → 404) ; les deux tests de dérivation X25519 migrés sur le
  nouveau chemin + token admin. Nouveaux helpers testkit `post_json_admin`
  et `post_json_remote` (IP source non-loopback, TEST-NET-3) pour exercer le
  gate admin sans court-circuit loopback.

### Note (non corrigé)
- Le fond de H-5 (la dérivation custodiale sans transmettre le secret au
  serveur) reste un choix produit : le SDK sait déjà tout dériver côté client.
  Ces endpoints sont conservés pour les flux custodial assumés, désormais
  réservés à l'opérateur. ⚠️ Le **dashboard Heshima**, s'il appelle restore,
  doit envoyer le token admin sur le nouveau chemin (sinon 401/404).

---

## [0.9.1] - Unreleased — Simulator testnet DB-longevity slowdown

### Changed
- **chore(simulator/testnet)**: tous les `interval_ms` de
  `tools/simulator/agents_testnet.toml` ont été multipliés par 6 (click/active/obs
  10s→60s, trader 5s→30s, spammer 200ms→1.2s, adversarial 1s→6s, coordinator
  60s→6min). À la charge d'origine (~30-40 blk/s soutenus, dominée par le minting
  de cubes), le DAG immuable (pas de pruning sur disque) remplissait les 480 Go du
  VPS testnet en ~1 semaine. `interval_ms` est le seul levier wall-clock ; tous les
  autres knobs (`mint_per_tick`, `sends_per_tick`, `edn_sends_per_tick`,
  `target_cubes`, `burn_cooldown_ticks`) sont tick-relatifs, donc ×6 conserve la
  *forme* exacte de la charge en l'étalant sur 6× le temps réel. Résultat : ~5-7
  blk/s soutenus → disque plein en ~6 semaines au lieu de 1 (au-delà du plancher
  « ≥ 1 mois »). L'invariant `cooldown_ticks × interval_ms > distribution_interval`
  ne fait que se renforcer.

### Added
- **test(simulator)**: `testnet_slowdown_tests` dans `tools/simulator/src/config.rs`
  — charge et parse réellement `agents_testnet.toml`, assert que chaque intervalle
  vaut exactement 6× son baseline v0.9.0 (garde-fou anti-revert), et projette la
  longévité disque (×6.00 → ~42 jours ≥ 1 mois). Affiche le débit calculé via
  `println!` (run : `cargo test --release testnet_slowdown -- --nocapture`).

### Infrastructure
- **docs(simulator)**: section « Ralentissement longévité-DB » ajoutée à
  `documentation/features/simulator.md` (frontmatter `updated`/`version` bumpés).

> ⚠️ **Déploiement** : ce changement n'est actif sur le testnet qu'après
> `scripts/upgrade-testnet.sh` (rebuild de l'image `pms-simulator:testnet` qui
> embarque le config). L'engine/gateway ne changent pas — image identique à v0.9.0.

---

## [0.9.0] - Unreleased — Security audit remediation (C-1/C-2/H-3/H-4/M-6/M-7/M-8/M-9)

Remédiation de l'audit de sécurité statique du 2026-06-11. Constat central de
l'audit : la couche de signature de **transaction** (les `unlocks`) était
inopérante — jamais vérifiée dans le hot path (C-2) et, même vérifiée, sans
lien cryptographique avec le propriétaire de l'UTXO dépensé (C-1). La sécurité
des fonds reposait à 100 % sur la signature de bloc du Coordinator.

### Fixed
- **fix(security/C-1+C-2)**: les transactions UTXO sont désormais pleinement
  autorisées dans le hot path (`do_persist_block_internal`) via la nouvelle
  `validate_transaction_full()` : appariement strict `input[i] ↔ unlock[i]`,
  vérification ECDSA de chaque unlock sur le message canonique
  `{network_id, inputs, outputs, fee}`, et **binding ownership** — la pubkey
  de l'unlock doit dériver l'adresse propriétaire de l'UTXO dépensé
  (`unlock_matches_address`, nouveau module `validations/ownership.rs`,
  supporte les deux formes d'adresse : pubkey hex brute du SDK et bech32m
  `SHA256(pubkey)[..20] || x25519`). Dépenser l'UTXO d'autrui avec sa propre
  clé est maintenant rejeté (`ValidationError::OwnershipMismatch`).
- **fix(security/C-1+C-2, encrypted path)**: `wallet_send_tx`
  (`POST /v1/wallet/tx/send`) vérifiait l'équilibre des montants mais **ni les
  signatures ni l'ownership** de la tx pré-signée avant de la chiffrer et
  d'appliquer son delta UTXO — un chemin de vol parallèle contournant le hot
  path (le ciphertext n'y est pas validable). Le handler exécute désormais
  appariement + `verify_tx_signatures` + binding ownership + conservation
  par asset sur le plaintext, avant chiffrement.
- **fix(security/H-3)**: la validation UTXO du hot path est **inconditionnelle**
  — elle ne dépend plus de `policy.skip_utxo_checks`. Ce flag ne pilote plus
  que le chemin sync legacy de `validate_block` (dag.rs/tests) ; une policy
  par défaut ne peut plus désactiver silencieusement les checks de production.
- **fix(security/M-7)**: règle de conservation canonique unique — conservation
  stricte PAR ASSET (`check_asset_conservation`, partagée hot path + handler) ;
  le champ `tx.fee` est déclaratif (la valeur des frais doit être un output
  explicite) mais subit un sanity check : décimal non-négatif et
  `<= max_fee_per_tx`. Avant ce fix, `fee` n'était validé nulle part en
  production (une tx avec `fee: "999999999"` passait), et `wallet_send_tx`
  acceptait une conversion cross-asset (10 PMS in → 10 EDN out) car il ne
  sommait que les totaux globaux.

### Fixed (suite — phases 2 à 5)
- **fix(security/C-2 extension)**: les checks d'autorité coordinator-only
  (`ConfigUpdate`, `Freeze`/`Unfreeze`/`Seize`/`Reverse`, `Reward`,
  `EncryptedReward`, `TokenCreate`, `Milestone`, `Bridge*`, `Contract*`,
  `LedgerOwnershipTransfer`, `CoordinatorKeyRotate`) étaient appliqués par le
  hot path **sans aucune vérification du signataire** (ils ne vivaient que
  dans le `validate_block` legacy, retiré du hot path) — seule l'enforcement
  single-writer les masquait. Nouveau module partagé
  `validations/authority.rs::validate_payload_authority()`, exécuté par les
  deux chemins AVANT tout apply d'état, avec la clé courante rotation-aware.
  Le bloc Mint réutilise la même policy (suppression d'une re-dérivation par
  bloc).
- **fix(security/H-4)**: single-writer **fail-closed** — un active set de
  clés vide (clé bootstrap absente, état de rotation corrompu) SAUTAIT le
  contrôle et acceptait tout bloc auto-signé. Nouvelle fonction pure
  `single_writer_gate()` : active set vide ⇒ rejet de TOUS les blocs en
  Testnet/Mainnet ; seul le mode Dev pur reste permissif (warn). 5 tests.
- **fix(security/M-6)**: l'id de bloc est recalculé à l'ingestion depuis le
  contenu canonique (parents + nonce + en-tête d'enveloppe avec commitment
  SHA-256 du payload) — tout mismatch est rejeté. Avant, l'id fourni par le
  client était accepté tel quel alors qu'il sert de clé d'idempotence
  (`AlreadyExists`) et de référence parent.
- **fix(security/M-8)**: `constant_time_compare` (auth admin) compare
  désormais les digests SHA-256 des deux côtés (32 octets fixes, aucune
  branche dépendante du secret) — l'ancien `ct_eq` factice sur mismatch de
  longueur fuyait la longueur du token admin par timing. 4 tests.
- **fix(security/M-9)**: le rate limiting du gateway passe de
  `PeerIpKeyExtractor` à `SmartIpKeyExtractor` (X-Forwarded-For/X-Real-IP) —
  derrière Caddy, tous les clients partageaient l'IP du proxy (limite
  globale contournable / DoS involontaire). Aligné sur l'engine.
- **fix(tests)**: réparation de `multi_ledger_test` (cassé sur main —
  Settings literal obsolète : champs `auto_reindex_activity_items`,
  `strict_key_permissions`, section `health` manquants).

### Changed
- **perf(validation)**: `verify_tx_signatures` déduplique les unlocks
  identiques avant la vérification ECDSA (les wallets mono-clé, SDK inclus,
  répètent le même unlock N fois — une seule vérification suffit).
- **consolidation + wallet_send_simple**: produisent un unlock PAR input
  (appariement positionnel requis par `validate_transaction_full`) au lieu
  d'un unlock unique pour N inputs.
- Le freeze check compliance du hot path réutilise les outputs fetchés par la
  validation (suppression d'un second lookup ShardedUtxoSet par input).

### Tests
- Nouveau `crates/pms-core/tests/spend_authorization.rs` (9 tests d'attaque) :
  vol d'UTXO d'autrui rejeté, vol en input mixte rejeté (index exact), tx sans
  unlocks rejetée, replay cross-network rejeté, fee fantôme/malformé rejeté,
  conversion cross-asset rejetée, dépenses légitimes acceptées (adresses
  bech32m ET pubkey brute SDK).
- Tests unitaires `validations/ownership.rs` (5 cas, dont adresse poubelle).

### Infrastructure / Versions
- **Workspace** `0.8.3` → `0.9.0`.
- **DAG_VERSION** `2.0.0` → `3.0.0` (MAJOR — règles de validation breaking :
  des blocs acceptés sous 2.x sont rejetés sous 3.x ; **wipe testnet requis**
  au deploy, cf. procédure DAG_VERSION major dans CLAUDE.md).
- **API_VERSION** `12` → `13` (`tx/send` exige des unlocks valides, 401
  sinon ; `submit/block` rejette les dépenses non autorisées et les ids non
  canoniques).
- Schéma DB (`CURRENT_VER=10`) et protocole P2P inchangés.

### Vérification
- `cargo test --release -p pms-server --test dag_sandbox -- --ignored` :
  **22/22 verts** (send-simple, contracts, compliance, tokens, gas pool,
  webhooks, SSE, cluster multi-engine, stress TPS) — aucun faux positif des
  nouveaux checks sur les chemins légitimes.
- `cargo test -p pms-core` : 49 tests lib (dont authority + ownership +
  single_writer_gate) + 9 tests d'attaque spend_authorization + 19
  multi_token + 4 tx_validation.

### ⚠️ Breaking / coordination requise
- **Le SDK TypeScript signe un message canonique différent**
  (`JSON({inputs,outputs,fee})` sans `network_id`, sans le pré-hash hex Rust) :
  ses transactions `submit/block` n'ont JAMAIS été compatibles avec
  `Transaction::signing_message` — personne ne les vérifiait. Avec ce fix,
  elles sont rejetées. **Le SDK doit être aligné avant tout deploy testnet.**
- Le simulateur n'est PAS impacté (il n'utilise que `send-simple`, signé côté
  serveur). `tools-cli` et les benchs signent déjà au format Rust.

---

## [0.8.3] - Unreleased — jemalloc / THP fragmentation control

### Fixed
- **Slow memory bloat over multi-day uptime** (testnet 4-day diagnostic 2026-05-13, v0.8.2): engine anon RSS climbed from ~5.5 GiB at boot to ~8.5 GiB over 4 days while producer == consumer (no leak in the persist pipeline) and the in-RAM DAG was bounded by `max_dag_blocks = 10000`. Memory profile breakdown via `GET /admin/memory-profile` revealed **4.58 GiB of anon_thp** — Transparent Huge Pages that jemalloc had requested in 2 MB chunks during burst allocations and never returned to the kernel under the default decay timers. As cgroup RSS approached the 14 GiB limit, kernel direct-reclaim contention slowed the tokio scheduler enough that the persist consumer task ran intermittently slow → producers experienced ≥ 1 s `send().await` waits → Phase 5.2 armed read-only every ~2 min (cumulative 3.7 M `pms_read_only_rejections_total{reason="memory"}` + 15.8 M `{reason="persist_queue"}` over 4 d). A manual restart freed **2.9 GiB instantly** (8.48 → 5.58 GiB), confirming the root cause was THP fragmentation, not a leak.
- **`MALLOC_CONF` environment variable** added to both `docker-compose.testnet.yml` and `docker-compose.mainnet.yml`:
  ```
  background_thread:true,dirty_decay_ms:30000,muzzy_decay_ms:30000,metadata_thp:auto
  ```
  - `background_thread:true` enables jemalloc's purge worker — pages get released to the OS without waiting for a synchronous allocation call to trigger the decay path.
  - `dirty_decay_ms:30000` / `muzzy_decay_ms:30000` force jemalloc to return dirty/muzzy pages within 30 s instead of the default 10 s muzzy + indefinite hold under sustained allocation pressure.
  - `metadata_thp:auto` keeps THP for jemalloc's internal metadata (small, hot) but pushes user allocations onto 4 KB pages, eliminating the half-empty-2 MB-page bloat pattern observed on testnet.
- Expected effect: stable anon RSS at ~5–6 GiB indefinitely under steady-state load, with sawtooth drops every 30 s instead of monotonic climb until OOM-adjacent. Eliminates the "memory-pressure-induces-Phase-5-flap" cascade entirely.

### Files
- `docker-compose.testnet.yml` — `MALLOC_CONF` env var on the engine service.
- `docker-compose.mainnet.yml` — same.
- `Cargo.toml` — workspace version `0.8.2` → `0.8.3`. No Rust code changes; only the deployment env var (jemalloc reads `MALLOC_CONF` at startup).

### Verification plan
1. Deploy v0.8.3 to testnet via `upgrade-testnet.sh`.
2. Sample `GET /admin/memory-profile` at +1 h, +6 h, +24 h, +4 d and verify `anon_thp` stays flat (no monotonic climb).
3. Compare ARM count over 96 h vs v0.8.2 baseline. Expected: 0 ARMs (vs 27 on day 4 of v0.8.2).
4. Compare `pms_read_only_rejections_total{reason="memory"}` rate. Expected: 0 (vs 3.76 M / 4 d on v0.8.2).

### Limit
- THP fragmentation is a runtime allocator behavior — the fix is env-only (no Rust code change). If the workload changes substantially (e.g. very different burst pattern, much larger working set), the 30 s decay timer may need re-tuning. Watch `anon_thp` from the memory profile endpoint as the canary.
- If memory continues to grow despite this fix, the leak is in the application (not the allocator) — next step would be a heap profile via `tikv-jemalloc-ctl` or `jeprof`.

---

## [0.8.2] - Unreleased — Phase 5.2 stall-only producer-signal

### Fixed
- **Phase 5 producer-signal still too noisy** (testnet 25h diagnostic 2026-05-08, v0.8.1 deployed): engine was read-only ~50 % of the time despite the consumer keeping up perfectly with the producer. Hard data over 25h: 5,713,926 blocks produced, 5,713,926 blocks consumed (exactly equal — zero backlog ever accumulated), 0 entries in `pms_persist_stall_seconds_total` (the ≥1 s stall counter never incremented), consumer batch p99 duration 1.87 ms, consumer total CPU 0.035 %. The system was nominal. But `pms_persist_back_pressure_events_total` showed 240,013 events / 25h = 5.5 events/tick avg — *exactly* at the v0.8.1 threshold (`bp_count >= 5`). Any 2 consecutive ticks slightly above the noise floor armed read-only, so 87 ARMs / 24h with 8.3M `pms_read_only_rejections_total{reason="persist_queue"}` accumulated while the engine was healthy.
- **Phase 5.2 fix**: drop the count-based path. Tick signal is now `bp_max_ms >= 1000` only — i.e. at least one producer in the last 5 s window had a `send().await` wait ≥ 1 s. The 2-tick gate is preserved (10 s sustained). Brief 500-999 ms waits under concurrent producers are normal load-shedding by the bounded channel and don't warrant 503ing every write; a ≥1 s wait means the channel was genuinely full long enough that subsequent producers would queue behind it. This aligns the producer-signal trigger with `pms_persist_stall_seconds_total`'s definition (the warn-interval that fires on ≥1 s waits), giving a single coherent saturation threshold across the metrics surface.

### Files
- `crates/pms-server/src/api/tasks.rs` — `tick_signal` simplified to `bp_max_ms >= 1000`; comments updated with the testnet diagnostic + rationale chain (v0.8.0 → v0.8.1 → v0.8.2).
- `Cargo.toml` — workspace version `0.8.1` → `0.8.2`.

### Limit
- The 2-tick gate (10 s of sustained ≥1 s waits) remains conservative vs the queue-depth sampler's 1-tick fast-arm (5 s) — the depth path catches sample-time saturation immediately. Producer-signal is now an additive, lower-frequency confirmation rather than a primary trigger. If a sustained ≥1 s stall pattern *should* fast-arm in some future workload, drop the gate to 1 tick — but observing testnet first will quantify if it's needed.

---

## [0.8.1] - Unreleased — Phase 5.1 producer-signal anti-flap

### Fixed
- **Read-only flap loop** (testnet 2026-05-06, v0.8.0 deployed): engine cycled in/out of read-only every 60–65 s with `reason: "persist_queue"` despite a sustained load of only ~1 blk/s and queue depth = 0 between flaps. 6.1 M `pms_read_only_rejections_total{reason="persist_queue"}` accumulated in hours. Cause: Phase 5 producer-signal threshold was too sensitive — `bp_count >= 3 || bp_max_ms >= 1000` per single 5 s tick, fast-armed (1 tick). At each disarm window, the simulator's pent-up requests rushed the channel, briefly saturated it (channel cleared by the next tick → depth 0), accumulated 3+ events in 5 s, and re-armed immediately. Phase 5 was correctly catching real saturation but couldn't distinguish a 5 s microburst from sustained pressure.
- **Phase 5.1 fix**: producer signal is now multi-tick. Per-tick threshold raised to `bp_count >= 5 || bp_max_ms >= 2000`, and the signal must persist across 2 consecutive ticks (10 s of sustained back-pressure) before it folds into `persist_pressure`. `producer_signal_consecutive` counter tracks consecutive-tick state; resets to 0 on any below-threshold tick. The depth-based sampler (`adapter.persist_queue_depth() ≥ threshold_pct`) keeps its existing 1-tick fast-arm — sample-time saturation IS sustained by definition. This filters microbursts while still catching the sub-5 s saturation Phase 5 was originally designed to detect.

### Files
- `crates/pms-server/src/api/tasks.rs` — `spawn_resource_guard_task`: new `producer_signal_consecutive` state, raised per-tick threshold, multi-tick gating before `producer_signal_pressure` flips true.
- `Cargo.toml` — workspace version `0.8.0` → `0.8.1`.

### Limit
- Threshold values (`bp_count >= 5 || bp_max_ms >= 2000`, 2 ticks) tuned conservatively for current testnet load profile. Observe `pms_persist_back_pressure_events_total` and read-only arm count post-deploy to confirm flap is fixed without losing legitimate burst detection. If sustained 4 events/tick load produces no arm but later causes 503-on-send().await, drop threshold to `bp_count >= 3` while keeping the 2-tick gate.

---

## [0.8.0] - Unreleased — Cross-chain replay protection (BREAKING) + HD wallet (BIP32) + Payment-rail RPC + Watcher API + producer-signaled read-only

### Phase 5 — Producer-signaled read-only mode (close the sub-5s saturation gap)

#### Why
Diagnostic 2026-05-04 (24 h check, v0.7.30 in prod) :
- 6728 producer-side `persist_tx.send().await` waits ≥ 500 ms over 24 h (p50 642 ms, p95 1.4 s, p99 3.6 s, max 6.3 s).
- **0 read-only auto-arms** despite the 4-watermark resource guard being wired in v0.7.27.
- Cause: the 5 s sampling cadence of `spawn_resource_guard_task` polls `adapter.persist_queue_depth()` as a point-in-time observation. Bursts that saturate the channel for less than the sampling interval (which is most of them — p99 wait was 3.6 s, well under 5 s) clear before the next sample reads them. The channel went `0 → full → 0` between two ticks ⇒ invisible to the watcher, but every saturation made one or more producers wait.

#### Fixed
- **Producer-side accumulator** (`pms_core::back_pressure`): the persist pipeline calls `record_event(elapsed_ms)` whenever a `send().await` waited ≥ 500 ms. A process-global atomic counter + `max_elapsed_ms` accumulator survives until the next `drain()` call. Zero allocations, zero locks, zero contention — just two `AtomicU64`s touched per back-pressure event.
- **Resource guard folds the producer signal into `persist_pressure`**: every 5 s tick now drains the accumulator and treats `count ≥ 3 OR max_elapsed_ms ≥ 1000` as an additional `PersistQueue` pressure source. Either threshold alone or together arms read-only via the existing 1-tick fast-arm (5 s) → 60 s min-arm-duration → 30 s disarm-after-clear pipeline. Same wire format (`code: 1020`, `reason: "persist_queue"`), same backward compat.
- **New Prometheus metric**: `pms_persist_back_pressure_events_total` counter increments inline with `record_event`. Pair with `pms_persist_queue_depth` to spot the divergence between point-in-time observation and producer-observed wait time. Expected post-fix behavior: counter still increments (signal SOURCE), but each event triggers a fast-arm of `pms_engine_read_only` so downstream clients see clean 503s instead of waiting up to 6 s on `send().await`.

#### Files
- `crates/pms-core/src/back_pressure.rs` — new module (~80 lines), atomic counter + drain + unit test for round-trip semantics.
- `crates/pms-core/src/lib.rs` — register `pub mod back_pressure;` (alphabetical order, before `background_activity`).
- `crates/pms-core/src/metrics.rs` — new `PERSIST_BACK_PRESSURE_EVENTS` `IntCounter`.
- `crates/pms-core/src/net_adapter/persist.rs` — call `record_event(elapsed_ms)` immediately after the existing back-pressure log line in the producer's `Ok(())` branch.
- `crates/pms-server/src/api/tasks.rs` — `spawn_resource_guard_task` drains the accumulator each tick, folds into `persist_pressure` alongside the queue-depth check.

#### Limit
- Threshold tuning is conservative (`≥ 3 events OR ≥ 1000 ms`). Real burst patterns may show more or fewer events at the saturation boundary — observe `pms_persist_back_pressure_events_total` rate post-deploy to refine. If sustained 0-event-per-tick load somehow produces a 800 ms isolated wait every few minutes (e.g. one-off OS scheduler hiccup), it won't trigger arming. That's the right trade-off for testnet; mainnet may want stricter.
- Auto-disarm path unchanged (60 s min-arm + 30 s under-threshold). Producer signal only contributes to the arm decision; the watcher's hysteresis state machine remains the single source of truth for transitions.



### Why
Intégration PMS comme rail de paiement dans un SaaS streaming white-label.
- **Phase 1 (sécurité)** : `Transaction::signing_message()` ne hashait que `(inputs, outputs, fee)`, sans aucun lien avec le réseau. Une TX signée sur testnet était valide bit-pour-bit sur mainnet → cross-chain replay trivial. Blocker pour des dépôts à garanties bancaires.
- **Phase 2 (HD wallet)** : la plateforme doit pouvoir émettre une adresse de dépôt par utilisateur (potentiellement millions) sans stocker N clés privées. Standard BIP32/BIP39/BIP44 attendu par la plupart des SDK et hardware wallets.
- **Phase 3 (RPC)** : la SaaS a besoin de 4 endpoints qui n'existaient pas — un snapshot DAG monotone (équivalent `get_block_height` linéaire), une estimation de fee découplée du `prepare_tx`, un lookup de TX unifié (`{from, to, amount, fee, depth, is_finalized}` au lieu du raw block JSON), et un scan de range pour rattraper après un crash watcher.
- **Phase 4 (watcher API)** : les SaaS qui surveillent des millions d'adresses ne peuvent pas tenir N connexions SSE (une par user). Multi-address SSE = un seul stream qui filtre N adresses. Webhook subscription = pour les SaaS serverless (Lambda, Cloud Functions) qui ne peuvent pas garder un stream ouvert.

Faire les quatre maintenant, avant lancement mainnet, évite une migration chaotique plus tard.

### Phase 4 — Watcher API (multi-address SSE + webhooks)

#### Added
- **`GET /v1/activity/stream?addresses=a,b,c`** ([crates/pms-server/src/api_fn/activity/stream.rs](crates/pms-server/src/api_fn/activity/stream.rs)) : single SSE stream qui filtre N adresses (capé à `MAX_ADDRESSES_PER_STREAM = 1000`). Chaque event matchant émet un `ActivityItem` avec un champ `address` injecté dans le payload pour permettre au SaaS de router vers le user record sans re-parser. Encrypted payloads → événement `encrypted` générique avec l'adresse matchée (pas de décryptage côté serveur). >1000 adresses → 400.
- **Webhook subscription** ([crates/pms-server/src/api_fn/webhooks.rs](crates/pms-server/src/api_fn/webhooks.rs)) — module complet :
  - `POST /admin/webhooks` : crée une subscription `{addresses, callback_url, secret?}`. Si `secret` omis, le serveur génère 32 bytes aléatoires. Retour : `{subscription_id, secret, addresses_count}` — le secret n'est exposé qu'**une fois**, le SaaS doit le persister.
  - `GET /admin/webhooks` : liste les subscriptions. Le secret est `#[serde(skip_serializing)]` — jamais re-exposé après création.
  - `DELETE /admin/webhooks/{id}` : unsubscribe.
  - **Storage in-memory** (`DashMap`) — perdu au restart. Le SaaS ré-enregistre via heartbeat. Persistance en Phase 4.5 si demande réelle (évite un CF + migration RocksDB pour cette release).
  - Limites : `MAX_ADDRESSES_PER_SUBSCRIPTION = 1000`, `MAX_SUBSCRIPTIONS = 10_000`.
- **Background delivery worker** ([crates/pms-server/src/api_fn/webhooks.rs::run_delivery_loop](crates/pms-server/src/api_fn/webhooks.rs)) : subscribe au `pms_event::PmsEvent::BlockPersisted` du bus, intersecte les `involved_addresses` avec chaque subscription, POST signé HMAC-SHA256 vers chaque `callback_url`. Headers `X-PMS-Signature`, `X-PMS-Subscription-Id`, `X-PMS-Delivery-Id`, `X-PMS-Delivery-Attempt`. Body : `{subscription_id, block_id, address, ts_ms, encrypted, ledger_id}`. Retry exponentiel max 5 tentatives (1, 2, 4, 8, 16 s — total worst-case ~31s avant abandon avec `tracing::error!` + counter `failed_count`). Spawn-per-delivery pour qu'un callback lent ne bloque pas la pump.
- **`AppState.webhook_store`** ([crates/pms-server/src/api/state.rs](crates/pms-server/src/api/state.rs)) : nouveau champ partagé. Initialisé en `serve.rs` + delivery loop spawnée si `event_bus` disponible.

#### Changed
- **Bumped** `API_VERSION` : `11` → `12` (multi-SSE route + 3 webhook routes).

#### Tests
- **Unit tests** dans `crates/pms-server/src/api_fn/webhooks.rs` (run via `cargo test -p pms-server --lib api_fn::webhooks`) :
  - `hmac_signature_is_deterministic_and_distinguishes_inputs` — same secret + body → same sig (64 hex chars), différent secret/body → différente. Vital pour que le SaaS reproduise la signature dans n'importe quelle lib crypto.
  - `store_matching_returns_intersection_per_subscription` — l'intersection adresses-event × adresses-subscription est correcte (bug = revenue lost OU privacy leak).
- **Sandbox tests** ([crates/pms-server/tests/dag_sandbox.rs](crates/pms-server/tests/dag_sandbox.rs)) — run avec `cargo test --release -p pms-server --test dag_sandbox -- --ignored --nocapture --test-threads=1 <test_name>` :
  - `test_multi_address_sse_filters_correctly` — connect SSE pour 2 adresses, mint à A/C/B, reçoit exactement 2 events (pas l'unwatched). 1001 adresses → 400.
  - `test_webhook_subscribe_list_unsubscribe_roundtrip` — CRUD complet : subscribe (CREATED + secret returned), list (sans secret), bad request → 400 code=2030, delete (200), re-delete → 404 code=3040.

#### Dépendances ajoutées (pms-server)
- `hmac = "0.12"` (RustCrypto, compatible avec sha2 0.10 déjà présent)
- `reqwest = "0.12"` (était dans dev-dependencies, déplacée en runtime pour le delivery worker)

---

### Phase 3 — Payment-rail RPC endpoints

#### Added
- **`GET /v1/dag/status`** ([crates/pms-server/src/api_fn/dag.rs](crates/pms-server/src/api_fn/dag.rs)) : snapshot SaaS-friendly du DAG — `{tip_count, last_milestone, total_blocks, latest_block_ts_ms, network_id, api_version, dag_version}`. Curseur monotone via `total_blocks` + `latest_block_ts_ms` pour détecter de l'activité sans poller un endpoint plus lourd.
- **`POST /v1/estimate-fee`** ([crates/pms-server/src/api_fn/estimate_fee.rs](crates/pms-server/src/api_fn/estimate_fee.rs)) : pure compute (pas de UTXO selection), retourne `{fee, transfer_fee, total, fee_breakdown}` pour `{amount, asset_id?}`. Réutilise `FeePolicy::compute_fee` + `evaluate_transfer` (smart contract OnTransfer fees). Permet à la UI d'afficher le total exact AVANT que l'utilisateur signe.
- **`GET /v1/transaction/{block_id}`** ([crates/pms-server/src/api_fn/transaction_lookup.rs](crates/pms-server/src/api_fn/transaction_lookup.rs)) : lookup unifié — décode le payload, résout le `from` via les UTXOs parents, calcule `is_finalized + depth`, retourne `{tx_hash, block_id, from, to, amount, asset_id, fee, timestamp_ms, is_finalized, depth, status, inputs, outputs}`. Supporte `TxUtxo` (plain), `Mint`, `Reward` ; renvoie 403 code=1010 pour les payloads chiffrés en pointant vers `GET /v1/wallet/{address}/activity` (qui decrypt avec la clé du destinataire). 404 code=3040 pour block_id inconnu.
- **`GET /v1/blocks/range?after_ts=&after_id=&limit=`** ([crates/pms-server/src/api_fn/blocks.rs](crates/pms-server/src/api_fn/blocks.rs)) : scan paginé par timestamp via le CF `by_time` existant, le plus récent en premier. Curseur exclusif `(ts_ms, id, has_more)` — relance le call avec `after_ts` / `after_id` du curseur jusqu'à ce que `next_cursor` soit `None`. `limit` borné à 1000 (défaut 100). Idempotent : la même requête deux fois retourne les mêmes blocks (pour reprise après crash watcher).

#### Changed
- **`NetDagAdapter` trait étendu** ([crates/pms-interface/src/net_adapter.rs](crates/pms-interface/src/net_adapter.rs)) avec 3 méthodes par défaut (no-op fallback) :
  - `async fn count_descendants(&self, block_id: &str, max_count: usize) -> usize`
  - `async fn is_finalized(&self, block_id: &str) -> bool`
  - `async fn last_milestone(&self) -> Option<String>`
  Implémentées dans `CoreAdapter` ([crates/pms-core/src/net_adapter/mod.rs](crates/pms-core/src/net_adapter/mod.rs)) en délégant à `self.dag.*` (sync sous le capot).

#### Bumped
- **`API_VERSION`** : `10` → `11` (4 nouveaux endpoints).
- *Workspace, DAG_VERSION, protocol_version inchangés en Phase 3.*

#### Tests sandbox (4 ajoutés, tous passants)
Run avec `cargo test --release -p pms-server --test dag_sandbox -- --ignored --nocapture --test-threads=1 <test_name>`.
- `test_dag_status_endpoint` — snapshot vide (genesis seul) → `total_blocks=1` ; après 3 mints → `total_blocks=4`, `latest_block_ts_ms` set, `api_version=11`, `dag_version="2.0.0"`, `network_id="pms-e2e-test"`.
- `test_estimate_fee_endpoint` — 100 PMS → `fee="3.0000001"`, `total=amount+fee+transfer_fee`. Amount négatif → 400 code=2020. Amount malformé → 400 code=2020.
- `test_transaction_lookup_endpoint` — Mint block lookup retourne tous les champs (`from=null`, `to=recipient`, `amount=42`, `inputs=[]`, `outputs=[1]`). TxUtxo chiffré → 403 code=1010 avec redirect vers `/v1/wallet/{addr}/activity`. block_id inconnu → 404 code=3040.
- `test_blocks_range_endpoint` — page 1 (limit=3) retourne 3 blocks + curseur, idempotent à travers 2 calls. Page 2 via curseur retourne des blocks **disjoints** de page 1 (curseur exclusif).

---

### Phase 2 — HD wallet (BIP32 / BIP39 / BIP44)

#### Added
- **`pms_wallet::hd` module** ([crates/pms-wallet/src/hd.rs](crates/pms-wallet/src/hd.rs)) : dérivation BIP32 secp256k1 + BIP39 mnemonic + BIP44 path. API : `master_xprv_from_mnemonic`, `master_xprv_from_seed`, `derive_child_wallet`, `derive_child_wallet_at_index`, `pms_bip44_path`. Chaque wallet enfant est un `Wallet` complet (secp256k1 + X25519 dérivés cohérents).
- **`PMS_COIN_TYPE = 0x7FFF_FFFF`** : SLIP-44 coin type temporaire (range "private use") en attendant l'enregistrement officiel. Path BIP44 par défaut : `m/44'/2147483647'/{account}'/0/{index}`.
- **Tests** ([crates/pms-wallet/tests/hd_derivation_test.rs](crates/pms-wallet/tests/hd_derivation_test.rs), 7 tests) : déterminisme (re-dérivation reproduit l'octet pour octet), 1000 adresses uniques, multi-tenant (`account` différent → adresses disjointes), BIP39 passphrase protection, signing avec network_id Phase 1, chemins arbitraires, gestion d'erreurs.
- **Fiche Obsidian** [[hd-wallet-bip32]] : pattern d'usage SaaS, limitations watch-only, migration future SLIP-44.

#### Limitations actuelles (documentées)
- **Pas de mode "vrai watch-only"** (xpub-only sans master en RAM). Raison : l'adresse PMS bind deux pubkeys (secp + X25519), et le X25519 est dérivé de la *privée* secp via HKDF — un xpub seul ne peut pas le reconstruire. Phase 2.5 envisagée : (a) dérivation parallèle SLIP-0010 pour X25519, ou (b) adresses "deposit-only" sans X25519. Le pattern actuel "master chiffré at-rest, déchiffré à la demande" couvre 95% du bénéfice cold/hot.
- **SLIP-44 non enregistré** : migration nécessaire au moment de l'enregistrement officiel (ré-dérivation des adresses utilisateurs).
- **Pas de plugin hardware wallet** Ledger/Trezor (Phase ultérieure, dédiée).

#### Dépendance
- `bip32 = "0.5"` (RustCrypto) ajoutée à `crates/pms-wallet/Cargo.toml`. Features : `secp256k1`, `alloc`. Pas de `default-features` pour rester no_std-friendly côté bip32.

---

### Phase 1 — Cross-chain replay protection (BREAKING)

#### Why
`Transaction::signing_message()` ne hashait que `(inputs, outputs, fee)`, sans aucun lien avec le réseau. Une TX signée sur testnet était valide bit-pour-bit sur mainnet (mêmes UTXOs côté attaquant, même clé coordinateur) → cross-chain replay trivial. Le SaaS ayant besoin de garanties bancaires sur les dépôts, ce trou est un blocker. Faire le fix maintenant, avant lancement mainnet, évite une migration chaotique plus tard.

#### Changed (BREAKING)
- **`Transaction::signing_message(network_id: &str)`** — l'API prend désormais un `network_id` qui est inclus dans le message canonique signé. Le verifier hashe avec le `network_id` de la chaîne courante ; toute TX signée pour un autre réseau est rejetée comme `InvalidSignature("signature mismatch …")`. Aucune addition au wire format (la protection est intrinsèque au signing) — l'attaquant ne peut même pas prétendre signer pour un réseau X.
- **`ValidatePolicy.network_id: String`** ajouté. Plumb depuis `Settings.network.network_id` via `from_settings(&ValidationSettings, network_id: &str)` et `try_from_global_config()`.
- **`verify_tx_signatures(tx, network_id)`** prend désormais le `network_id` courant.
- **SDK TS local** (`sdk/src/client.ts`) : `txCanonical` inclut maintenant `network_id: this.config.networkId` en première position du JSON canonique — synchro stricte avec le struct Rust `Canon`.

#### Bumped
- **Workspace** : `0.7.30` → `0.8.0` (breaking change protocole)
- **`DAG_VERSION`** : `1.2.0` → `2.0.0` (major bump — refus de démarrer sur DB pré-existant, **wipe testnet obligatoire**)
- **`API_VERSION`** : `9` → `10` (handlers de signing exigent network_id matching)
- **`protocol_version`** P2P : `1` → `2` dans configs dev/testnet/mainnet
- *`CURRENT_VER` schema RocksDB inchangé (10) — pas de nouveau CF en Phases 1-2*

#### Tests
- `crates/pms-core/tests/tx_validation.rs::reject_tx_signed_for_different_network` — TX signée `pms-testnet-v1` rejetée par verifier `pms-mainnet-v1` (sortie : `signature mismatch input 0`). Sanity check : la même TX re-signée pour mainnet est acceptée.
- `accept_tx_signed_for_matching_network` — TX signée `pms-testnet-v1` acceptée par verifier `pms-testnet-v1` (preuve que le binding ne casse pas le happy path).

#### Décisions actées (non implémentées)
- **DER hex unification SDK ↔ Rust** : skipped. Le SDK convertit déjà hex → base64 avant l'envoi (`sdk/src/client.ts:540-542`), le wire format reste base64. Switcher en hex ferait grossir la signature sur le fil (140 chars vs 96 base64) — régression nette pour zéro bénéfice opérationnel.
- **`InvalidNetworkId` ApiError variant** : skipped. La protection est intrinsèque au hash signé — un network_id différent ne peut PAS être signalé comme tel par le verifier (ce serait précisément ce qu'on veut empêcher : que l'attaquant déclare son network_id côté wire). Le retour `SignatureMismatch` (4001) existant est sémantiquement correct.

---

### Migration / déploiement (Phase 1+2)
- **Wipe testnet obligatoire** : `DAG_VERSION` major bump → l'engine refusera de démarrer sur la DB existante. Procédure : arrêter la stack, `docker volume rm rocksdb_testnet_data`, redéployer avec `IMAGE_VERSION=v0.8.0 scripts/deploy-testnet.sh --yes`.
- **SDKs externes** doivent être mis à jour pour inclure `network_id` dans le canonical signing — sans ça, toutes leurs TX sortantes seront rejetées en `4001 SignatureMismatch`.
- **HD wallet** rétrocompatible : les wallets existants (créés via `Wallet::from_seed` / `Wallet::from_mnemonic`) continuent à fonctionner. La nouvelle API `pms_wallet::hd::*` est purement additive.

### Hors scope (Phases suivantes)
- Phase 2.5 : mode watch-only complet (xpub-only, master jamais en RAM) — exige redesign d'adresse pour soit dérivation parallèle SLIP-0010 X25519, soit format "deposit-only" sans X25519
- Phase 3 : `GET /v1/transaction/{tx_hash}`, `POST /v1/estimate-fee`, `GET /v1/dag/status`, `GET /v1/blocks/range`
- Phase 4 : multi-address SSE, webhook subscription
- SLIP-44 registration officiel
- Plugin hardware wallet (Ledger / Trezor)
- Voir `/Users/erwan.ngma/.claude/plans/je-veux-pouvoir-faire-jazzy-crayon.md` pour le plan complet.

---

## [0.7.30] - 2026-05-02 — RocksDB drain rate visibility (histograms) + 4× batch amortization

### Why
Diagnostic 2026-05-02 found the persist consumer capped at ~94 blk/s peak drain rate, while the producer rate hit 200 blk/s during simulator bursts — the 106 blk/s gap saturated the channel and caused 21 s `send().await` blocks. The cumulative counters `pms_persist_consumer_blocks_total` / `pms_persist_consumer_us_total` give us only the **average** batch metrics; we couldn't see whether the consumer was processing 64-block batches at high rate (fsync-bound — try larger batches) or 1-block batches in a tight loop (drip-fed — fsync rate × tiny amortization). Without distribution data, any tuning is blind.

### Added
- **metrics(consumer-batch-size)**: new `pms_persist_consumer_batch_size` Prometheus histogram. Buckets `1, 2, 4, 8, 16, 32, 64, 128, 256` cover the typical batch range. Lets operators see p50 vs p99 batch fill — bimodal distribution (idle = 1, saturated = MAX) reveals whether amortization is actually happening.
- **metrics(consumer-batch-duration)**: new `pms_persist_consumer_batch_duration_ms` Prometheus histogram with buckets from `0.1 ms` (sub-ms typical) to `30 000 ms` (worst observed stall). A long-tail at >1 s under load means RocksDB is holding a lock during compaction / memtable flush — different fix than fsync amortization.

### Changed
- **`MAX_BATCH_SIZE` 64 → 256** in `pms_core::background_persist`. Each `append_blocks_batch` commits one `WriteBatch` with one WAL fsync — the cost is paid per **batch**, not per **block**. Bumping to 256 gives 4× more amortization headroom under burst. Memory cost: ~25 MB transient at worst case (256 × 100 KB), acceptable on the 14 GB cgroup. Idle scenarios still process size-1 batches via `try_recv` — the cap only matters under burst, where the channel has many blocks queued.

### How to read the new metrics
```promql
# Average batch size over 5min — close to 256 = fsync amortization working
histogram_quantile(0.5, rate(pms_persist_consumer_batch_size_bucket[5m]))

# Worst-case batch duration — >1s sustained = compaction stall
histogram_quantile(0.99, rate(pms_persist_consumer_batch_duration_ms_bucket[5m]))

# Drain rate (blocks/sec)
rate(pms_persist_consumer_blocks_total[5m])
```

### Limit
- This is **measurement + 1 low-risk tuning knob**, not a structural fix for the underlying drain rate cap. If after deploy the histograms show batches consistently at 256 with durations of seconds, the real bottleneck is RocksDB internals (probably WAL fsync serialization × 67 CFs or compaction lock contention) — that needs a deeper investigation with `tokio-console` / flame graphs / RocksDB `OPTIONS` tuning. Tracked as the next iteration.

### Files
- `crates/pms-core/src/metrics.rs` — `Histogram` import + 2 new `register_histogram!` declarations.
- `crates/pms-core/src/background_persist.rs` — `MAX_BATCH_SIZE 64 → 256` + 2 `.observe()` calls in the consumer loop after every successful `append_blocks_batch`.

---

## [0.7.29] - 2026-05-02 — Forensic shutdown + persist drain (close banking-grade data-loss bug) + memory profile endpoint

### Fixed
- **bin(shutdown/data-loss)**: on `SIGTERM` (Docker stop, k8s pod cycle, `upgrade-testnet.sh`), the previous shutdown handler flushed RocksDB WALs but **dropped the persist channel mid-batch**. Up to 5 K blocks (v0.7.28 buffer) had been acknowledged to clients with `PutResult::Inserted` but never written to RocksDB. On restart they were silently lost — banking-grade data loss event. **Fixed**: shutdown now drains the persist queues with a 25 s deadline, polling depth every 100 ms across all ledgers (main + customs). Drains in well under 1 s under normal conditions (5 K / 64 batch × ~50 ms ≈ 4 s worst case). If the deadline hits with blocks still queued, logs an `ERROR` line at `target = "shutdown_data_loss"` with per-ledger counts so the operator KNOWS this restart corresponds to lost blocks.
- **bin(shutdown/forensic-logging)**: `shutdown_signal()` now logs at `WARN` level with the signal name (SIGINT / SIGTERM / SIGHUP) so operators can correlate clean exits with their cause via grep. Pre-shutdown logs the persist queue depths per ledger; post-drain logs total ms spent. Combined with the data-loss check above, the diagnostic of "37 historical clean exits, what caused them?" becomes a one-line grep instead of inspect-ex-post.
- **bin**: registered SIGHUP handler in addition to SIGINT/SIGTERM so a controlling-terminal close on dev boxes is also logged loudly.

### Added
- **api(memory-profile)**: new `GET /admin/memory-profile` endpoint exposes the full cgroup v2 memory breakdown (`memory.current`, `memory.max`, every key in `memory.stat` — `anon`, `file`, `kernel`, `kernel_stack`, `pagetables`, `slab`, `slab_reclaimable`, `slab_unreclaimable`, etc.) plus `/proc/self/status` totals (`VmRSS`, `VmPeak`, `VmSize`, `RssAnon`, `RssFile`, `VmSwap`, etc.). Returns `current_pct` (matches `docker stats`) and `irreclaimable_pct` (the value the read-only resource guard uses for the memory watermark check since v0.7.26). Drops in `admin_recovery` so it stays queryable while the engine is in read-only mode for memory pressure (the time you most need it). Linux + cgroup v2 only; on non-Linux platforms returns 503 `{"error":"unsupported"}` with a clear reason.
- **Why now**: the testnet engine showed `anon = 9.6 GiB` (out of 14 GiB cgroup cap) under load, with documented memtable cap = 2.1 GiB and block cache = 256 MiB, leaving ~7 GiB unaccounted for. The read-only safety valve (v0.7.23) arms before OOM kill but doesn't help diagnose. This endpoint lets us correlate allocation patterns with workload changes without `docker exec` + manual `cat`. **Future**: integrate `tikv-jemalloc-ctl` (`stats.allocated/active/resident/mapped`) to quantify allocator fragmentation specifically.

### Files
- `bin/src/main.rs` — shutdown branch rewritten with drain loop + forensic logs; `shutdown_signal()` upgraded with WARN-level signal-source logging.
- `crates/pms-server/src/api_fn/memory_profile.rs` — new module with `admin_memory_profile` handler and `read_cgroup_v2_snapshot` helper.
- `crates/pms-server/src/api_fn/mod.rs` — register the new module.
- `crates/pms-server/src/api/routes.rs` — wire `/admin/memory-profile` into `admin_recovery`.

### Limit
- The persist drain is best-effort: we don't gate new writes during the drain window (would need to wire `AppState.read_only` into the main shutdown path, which is a bigger refactor). In practice SIGTERM means clients are also being torn down so new writes are minimal during the drain. A future iteration can `state.read_only.arm(Manual)` for a hard stop.
- Memory profile shows cgroup categories but not allocator internals. The 9.6 GiB anon mystery may need jemalloc-ctl integration to fully resolve.

---

## [0.7.28] - 2026-05-02 — Persist back-pressure final close: fast-arm + bigger buffer + healthcheck slack

Three-part fix to close the residual back-pressure observed after v0.7.27 deployed:

### Diagnostic (testnet 2026-05-02)
- v0.7.27 read-only auto-arm fired correctly (44/76 samples armed in 2 h, no flap, 60 s min-arm respected) — but the disarm→re-arm window let bursts saturate the channel and produce **20 s `send().await` blocks** at 22:06:12 UTC (`elapsed_ms=19987` on 7 simultaneous blocks). Cause: 10 s arm latency (2 ticks × 5 s) is too slow for producer-side back-pressure that hits the wall in 5-10 s.
- Engine showed `Up X (unhealthy)` despite `/healthz` returning 200 OK on direct curl — the Docker healthcheck timeout (5 s) was being exceeded by the `/healthz` handler under burst load (multiple RocksDB property reads). 10 consecutive timeouts within 100 s flipped Docker's healthy bit to unhealthy. Cosmetic but misleading.

### Fixed
- **A) Fast-arm on `PersistQueue`**: arm threshold drops from 2 ticks (10 s) to **1 tick (5 s)** when the dominant pressure reason is the persist channel. Closes the disarm→re-arm window. Memory / disk / RocksDB still arm at 2 ticks (10 s) — those signals legitimately oscillate during normal operation (memtable flush) and need the second-sample confirmation to avoid flap. Persist queue is purely producer-driven and any sustained 5 s saturation is a real problem we want to gate immediately.
- **B) Channel buffer 2 K → 5 K blocks** in `pms_core::core_adapter`. RAM cost: ~30-100 MB at 10-100 KB per block, easily absorbed on the 14 GB cgroup. Combined with the 5 s fast-arm, the watcher now has 2.5× more burst headroom to detect saturation and arm before producers hit the wall. Buffer growth alone wouldn't fix the issue (RocksDB consumer caps at ~94 blk/s peak; bigger buffer just delays saturation), but pairing it with proactive arming gives the right "fail-fast at 80 % depth" semantics with margin.
- **C) Docker healthcheck slack**: timeout `5 s → 15 s`, interval `10 s → 30 s`, retries `10 → 5`. The `/healthz` handler does multiple RocksDB property reads (block count, last block age, persist queue depth, disk free) that can sometimes take >5 s under burst — adding `--max-time 15` to the curl probe and giving the handler 15 s to respond avoids the cosmetic "unhealthy" flag without weakening the underlying signal. Net: Docker still marks unhealthy after 5 × 30 s = 150 s of consecutive failures (down from 10 × 10 s = 100 s but timeout is 3× more permissive per check).

### Files
- `crates/pms-server/src/api/tasks.rs` — `ARM_TICKS` split into `ARM_TICKS_DEFAULT (2)` and `ARM_TICKS_PERSIST (1)`; new `arm_threshold` selection inside the watcher loop based on the dominant `new_reason`.
- `crates/pms-core/src/core_adapter.rs` — both call sites of `spawn_background_persist_with_activity` bumped from 2 000 to 5 000 buffer.
- `docker-compose.{testnet,mainnet}.yml` — engine healthcheck `interval=30s`, `timeout=15s`, `retries=5`, `--max-time 15` on the curl probe.

### Limit of this fix
- Does NOT address the underlying root cause that consumer drain caps at ~94 blk/s peak. That's a RocksDB write throughput question — needs profiling of WAL fsync contention across the 67 CFs, possible parallelization of the write path, or batching tuning. Tracked as a follow-up. The current fix prevents producer pile-up by shedding load proactively; the engine continues to serve reads normally during armed periods.
- Memory pressure (anon ~9.6 GiB / 14 GiB cap = 70 %) keeps triggering the Memory-based read-only too. That's a separate investigation — possibly memtable accounting + jemalloc fragmentation + multi-CF overhead. Tracked as a follow-up.

---

## [0.7.27] - 2026-05-01 — Read-only mode 4th trigger: persist queue saturation (banking-grade fast-fail)

### Fixed
- **engine(persist/back-pressure)**: testnet 2026-05-01 logged `persist_tx.send was back-pressured` events with `elapsed_ms` up to **21,147 ms** — producers blocked for 21 s on a single `persist_tx.send().await` waiting for the bounded mpsc channel (2 000 capacity) to free up. Diagnosis: simulator producer rate peaks ~200 blk/s during burst while the RocksDB consumer drain caps at ~94 blk/s peak (observed via `rate(pms_persist_consumer_blocks_total[1m])`); the gap (~106 blk/s net fill) saturates the 2 000-block buffer in ~19 s, which matches the 21 s blocking observed. A 21-second blocking send is unacceptable for a banking-grade engine — clients hit unrelated request timeouts, and the engine surface up to the SDK looks like a hang rather than a structured back-pressure signal.

### Added
- **engine(read-only/persist-queue)**: new `ReadOnlyReason::PersistQueue` (string `"persist_queue"`, code `1020` HTTP 503) and 4th check in the resource-guard task. When ANY ledger's persist channel depth crosses `[health].persist_queue_critical_pct` (default `0.8` = 80 %) for `ARM_TICKS` consecutive samples (10 s), the engine flips into read-only mode globally. Writes return a fast `503 read_only` with `Retry-After: 30` — machine-readable, SDK-handleable, and gives the consumer breathing room without forcing client timeouts. Subject to the v0.7.26 60 s min-arm-duration floor so a single burst doesn't flap.
- **walks all ledgers**: main + custom (eden, etc.) — if any saturates, arm globally. Producers hitting the wall on one ledger's `send().await` block their HTTP handlers; shedding load across the engine keeps the SDK happy for unrelated reads on other ledgers (which still serve since reads are never gated).
- **stable wire**: same code `1020`, only the `reason` field changes. Existing SDK clients that switch on `code` continue to work; they get an additional `reason: "persist_queue"` value to surface to users (useful for "we're catching up, retry in 30 s" UX).
- **Unit test added** to `read_only.rs` lock-down list (`reason_str_is_stable` now asserts `"persist_queue"` is the stable wire string).

### Why not just bump the channel buffer?
Considered. 2 000 → 5 000 would cost ~30-100 MB extra RAM (10-100 KB per block × 5 K) and would absorb 2.5× longer bursts — but it just delays the saturation, doesn't prevent it. With the engine-side persist consumer capped at ~94 blk/s peak (RocksDB WAL fsync × 67 CFs serialised), any sustained producer rate above that fills the buffer eventually. The 503 fast-fail is the structurally correct answer: it propagates back-pressure to the SDK, which already implements exponential backoff via the existing `Retry-After` semantics.

### Configuration
- `[health].persist_queue_critical_pct` — fraction of capacity at which to arm. Default `0.8`. Set per-network in `etc/config/config.{testnet,mainnet}.toml` with explanatory comments. Mainnet could tighten to `0.7` if real load demands earlier shedding; the default is intentionally permissive.

### Files
- `crates/pms-server/src/read_only.rs` — new variant `ReadOnlyReason::PersistQueue` + `as_str` + `from_u8` + unit-test assertion. The numeric discriminant `5` is part of the wire — adding new variants is additive, renumbering would be a breaking change.
- `crates/pms-config/src/config.rs` — new `HealthSettings::persist_queue_critical_pct` field, default 0.8.
- `crates/pms-server/src/api/tasks.rs` — new `check_persist_saturated` helper that walks main + custom ledgers via `LedgerManager`; new 4th check in the watcher loop wired into the existing reason-priority cascade.
- `etc/config/config.{testnet,mainnet}.toml` — explicit setting + comment.

---

## [0.7.26] - 2026-05-01 — Read-only mode anti-flap (memtable-flush cycle fix)

### Fixed
- **engine(read-only/flap)**: testnet was looping in/out of read-only mode every 1-2 minutes — observed 2026-05-01 with arm/disarm pairs at 15:46:03→15:46:32, 15:47:12→15:47:42, 15:50:52→15:51:22 (engine logs). Each cycle: memtable burst pushes anonymous memory > 90 % for 10 s → ARM, RocksDB flushes ~2 GiB of memtables → memory drops < 75 % over the next 30 s → DISARM, memtable refills → repeat. Two changes:
  1. **Effective memory excludes reclaimable** (`pms-server::api::tasks::read_cgroup_memory_pct`): subtracts `file` (page cache) + `slab_reclaimable` from `memory.current` before computing the percentage. The OOM killer triggers on irreclaimable memory only — page cache and reclaimable slabs are released by the kernel under pressure, so counting them toward our 90 % watermark would arm read-only on a perfectly healthy process. On testnet today the file cache is only ~100 MiB so the immediate impact is small; this is correct semantics + future-proofing. Matches what `docker stats` displays in its memory column for the same reason.
  2. **Min-arm-duration floor** (`[health].read_only_min_arm_duration_secs`, default `60`): once auto-armed, refuse to auto-disarm before this many seconds have elapsed — even when pressure has cleared for the full `DISARM_TICKS` window. Breaks the memtable-flush cycle: the burst-then-flush sequence is now treated as one sustained pressure event rather than a flap-able toggle. **Manual arms** via `POST /admin/read-only/arm` are NOT subject to this floor — they release immediately on operator `disarm`.
- **Why the cycle was bad** (beyond the cosmetic flap): each ARM rejected legitimate writes for ~30 s with `503 read_only`; the simulator and SDK clients backed off and retried, adding pressure to the persist channel during the recovery window. Net effect: artificial throughput dips that propagated all the way to the dashboard's TPS chart.

### Tuning notes
- Watermarks unchanged (testnet: 90 % / 75 %, mainnet: 88 % / 70 %). Hysteresis ticks unchanged (ARM=2/10 s, DISARM=6/30 s).
- The min-arm floor is independent: a sustained-pressure event still releases on the same cadence as before *plus* the 60 s floor (whichever is longer). On a real OOM-imminent leak the floor is irrelevant — pressure stays high, the watcher stays armed, the operator restarts.
- Next step: investigate the persist-channel back-pressure observed during the same window (212 `persist_tx.send back-pressured` events in 5 min, `elapsed_ms` up to 6.9 s) — separate from the flap. RocksDB stats survey TBD.

### Files
- `crates/pms-config/src/config.rs` — new `HealthSettings::read_only_min_arm_duration_secs` field, default 60.
- `crates/pms-server/src/api/tasks.rs` — `read_cgroup_memory_pct` subtracts reclaimable via new `read_cgroup_v2_reclaimable` helper; watcher loop tracks `armed_at: Option<Instant>` and gates auto-disarm on min-duration elapsed.
- `etc/config/config.{testnet,mainnet}.toml` — both pinned at 60 s (the memtable-burst pattern is universal, not load-profile specific).

---

## [0.7.25] - 2026-05-01 — Smoothed `pms_blocks_per_second_ewma` gauge (kills the fake-1500-blk/s dashboard artifact)

### Added
- **metrics(blocks_per_second)**: new `pms_blocks_per_second_ewma{ledger_id}` Prometheus gauge, computed by the metrics sampler every 5 s as an exponentially weighted moving average (`α = 0.2`) of the per-ledger block production rate. Effective smoothing window ~25 s. Exposed in both `/metrics/all` (full Prometheus format) and `/l/{id}/metrics` (dashboard-compatible label-free format).

### Why
Dashboards that polled the raw `pms_blocks_persisted_total` counter at sub-second cadence and computed their own `Δcounter / Δwall_time` were showing **fake 1500 blk/s spikes** under normal load. The cause: the `background_persist_task` drains the persist channel in `WriteBatch`-es of up to 64 blocks, and a single batch completes in ~30-50 ms (RocksDB amortizes the fsync). When that 64-block jump landed inside a 50 ms dashboard polling window, the displayed rate was `64 / 0.05s ≈ 1280 blk/s` — a visualization artifact of sub-second sampling on a counter that increments in batches, **not** real throughput.

Diagnostic on testnet 2026-05-01 (1 h window via Prometheus):
- Median rate (30 s windows): **60 blk/s** consumer.
- p95: **80 blk/s**.
- Max sustained: **146 blk/s** aggregated across ledgers.
- Persist queue depth max: 56 blocks (2.8 % of 2 K capacity → 35× headroom). Queue oscillates 0 → 56 → 0 every ~5-10 s under load — exactly the burst pattern that fooled the dashboard.

### How to consume
- **Dashboards**: read `pms_blocks_per_second_ewma` directly. No more `Δcounter / Δwall_time` logic; the smoothing is done at the source. Honest sustained throughput, no sub-second drain-burst illusions.
- **Operators**: keep using `rate(pms_blocks_persisted_total[5m])` for the long-window view; the new gauge is the short-window dashboard equivalent (~25 s effective window).
- **Multi-ledger**: per-ledger gauge — `pms_blocks_per_second_ewma{ledger_id="main"}`, `{ledger_id="eden"}`, …

### Files
- New gauge in `crates/pms-server/src/metrics.rs::BLOCKS_PER_SECOND_EWMA` + emission in `render_for_ledger` (dashboard format).
- New `update_blocks_per_second_ewma` helper in `crates/pms-server/src/api/tasks.rs` driven by the existing `spawn_metrics_sampler_task` 5 s loop. State is a per-ledger `(prev_count, prev_ewma)` map; first observation initialises the baseline, second observation onward publishes a smoothed rate.

---

## [0.7.24] - 2026-05-01 — Stable numeric API error codes (anti-enumeration framework)

### Added
- **api(error-codes)**: new `pms-server::api_error::ApiError` enum with stable 4-digit numeric codes for every API error. Each variant carries the precise internal context (address, amount, signature failure reason, RocksDB error, …) which is logged via `tracing::warn!` / `tracing::error!` with `target: "api_error"`, but exposes only a vague public message in the JSON body for state/auth/crypto categories. Wire format: `{"code": NNNN, "message": "..."}`.
- **Numbering grid** (28 codes, all enumerated in `documentation/api/error-codes.md`):
  - `1xxx` auth/authz: `1001 MissingAuth`, `1002 InvalidAuth`, `1010 AdminRequired`, `1020 ReadOnly`, `1030 IpNotAllowed`, `1040 InsufficientScope`.
  - `2xxx` validation: `2001 MalformedJson`, `2010 InvalidAddress`, `2020 InvalidAmount`, `2030 InvalidField`, `2040 TooLarge` — specific public OK, no security info.
  - `3xxx` state/business: `3001 InsufficientBalance`, `3010 AlreadySpent`, `3020 UnknownLedger`, `3030 ContractDisabled`, `3040 NotFound`, `3050 AlreadyExists`, `3060 AddressFrozen`, `3070 Conflict` — vague public, anti-enumeration.
  - `4xxx` crypto/security: `4001 SignatureMismatch`, `4002 ReplayDetected`, `4010 CryptoFailure`, `4020 AuthorizationSignatureInvalid` — always vague, status 401 instead of 400 to defeat timing attacks.
  - `5xxx` resource/quota: `5001 RateLimited`, `5010 GasPoolEmpty`, `5020 SubscriptionInactive`.
  - `9xxx` internal: `9001 StorageError`, `9002 ConsensusError`, `9999 Internal` — never leaks RocksDB error / stack trace.
- **First migration wave** (4 sites): `require_writable` middleware now returns `ApiError::ReadOnly { reason }` (preserves v0.7.23 wire fields `error/reason/retry_after_seconds` for backward compat, adds `code: 1020`). `require_local_or_admin`, `require_admin_token`, `require_api_key` migrated to `ApiError::MissingAuth`/`InvalidAuth`/`IpNotAllowed`/`InsufficientScope`.
- **Prometheus metric**: `pms_api_errors_total{code}` counter (bounded cardinality, ~30 codes). Operators can alert on `rate(pms_api_errors_total{code=~"9..."}[5m]) > 0` (handler still on legacy `anyhow` path = migration target) or `rate(pms_api_errors_total{code=~"4..."}[5m]) > 1` (sustained crypto / replay = brute force).
- **Documentation**: `documentation/api/error-codes.md` — full grid with public message + internal detail per variant, recommended alerts, migration roadmap (high-value financial paths next: `wallet_send_simple`, `wallet_send_tx`, `prepare_tx`, NFT mint/burn, token mint/create, `submit_block`, compliance handlers). Linked from `documentation/MOC.md`.
- **Sandbox tests**:
  - `test_read_only_mode_gates_writes` extended to assert `body["code"] == 1020` alongside the existing legacy fields.
  - `test_api_error_codes_on_auth_failure` — boots a custom ledger to escape the loopback bypass, exercises both `1001 MissingAuth` (no token) and `1002 InvalidAuth` (wrong token). Confirms public messages stay vague (`"Authentication required"` vs `"Authentication failed"`).
- **Unit tests** in `api_error.rs`:
  - `codes_are_unique` enumerates all 29 variants and panics on a duplicate code (catches mistakes when adding a new variant).
  - `public_message_never_leaks_internal_detail` asserts no substring of the internal detail (addresses, amounts, RocksDB errors) appears in the public message.

### Why it matters
Public error messages are an attack surface. A handler that returns `"insufficient balance: addr=8e1... requested=100 available=3"` lets an attacker probe wallets and infer balances, rate-limited but cheap. Generic strings like `"Operation failed"` paired with a stable `code: 3001` give SDK clients exactly what they need to handle errors programmatically (branch on the number, retry policy, user-facing translation) without leaking state. The same pattern Stripe / AWS / OAuth use, adapted for a banking-grade DAG.

### Migration plan
- v0.7.24 ships the framework + middleware migration. Internal handlers continue using `anyhow::Error` until migrated.
- Next waves migrate financial / crypto paths (highest leak risk). The `pms_api_errors_total{code="9999"}` rate is the migration tracking metric — every `9999` increment is a handler still on legacy path that needs an `ApiError` variant.

### Files
- New: `crates/pms-server/src/api_error.rs` (430 lines, including unit tests).
- New: `documentation/api/error-codes.md` (full grid + migration roadmap).
- Modified: `crates/pms-server/src/api/middleware.rs` (4 middlewares migrated), `crates/pms-server/src/metrics.rs` (new counter), `crates/pms-server/src/lib.rs` (export `pub mod api_error`), `crates/pms-server/tests/dag_sandbox.rs` (sandbox test additions), `documentation/MOC.md` (index entry).

---

## [0.7.23] - 2026-05-01 — Read-only mode (graceful degradation under resource pressure)

### Added
- **engine(read-only-mode)**: new resource-guard task and `require_writable` middleware. When cgroup memory usage crosses `[health].memory_high_watermark_pct` (default 90%), free disk drops below `disk_critical_free_percent` (default 5%), or RocksDB reports `is-write-stopped == 1` / L0 file count crosses `rocksdb_l0_critical_files` (default 100), the engine flips into **read-only mode**:
  - Write-producing API routes (submit/block, wallet/tx/send, send-simple, NFT mint/burn, faucet, distribute_fees, tokens create/mint, ledgers create / transfer-ownership, bridge/transfer, compliance freeze/unfreeze/seize/reverse, per-ledger admin tokens/faucet/nft/mint) return `503 Service Unavailable` with body `{"error": "read_only", "reason": "memory|disk|rocksdb|manual", "message": "...", "retry_after_seconds": 30}` and a `Retry-After: 30` header.
  - Background tasks `spawn_fee_distributor_task` and `spawn_inflation_mint_task` skip their tick (fees keep accumulating in the pool, distributed on the next clear tick).
  - **Reads keep serving normally** (balance, supply, history, blocks, NFT lookup, version, dag tips, dashboard streams, `/v1/tx/prepare`, wallet create/restore) so users see their state without disruption.
  - **Recovery endpoints stay open** (`/admin/compact`, `/admin/reindex-*`, `/admin/rebuild-tips`, `/admin/purge-*`, `/admin/consolidate-utxos`, `/admin/config` GET/POST, `/admin/api-keys` CRUD, `/admin/contracts` CRUD/toggle, `/admin/gas-pool` deposit/withdraw, `/admin/rocksdb-stats`, `/admin/read-only/*`) so the operator can bring the engine back without fighting the guard.
- **Hysteresis** prevents flapping: ARM after 2 consecutive samples (10 s) over the high watermark; DISARM after 6 consecutive samples (30 s) below the low watermark. A `Manual` arm is **never** auto-cleared — only `POST /admin/read-only/disarm` releases it (so an operator can hold the engine in a known state during maintenance windows).
- **Operator controls**:
  - `GET /admin/read-only/status` — `{"armed": bool, "reason": "..."}`.
  - `POST /admin/read-only/arm` — manually flip into read-only (reason `manual`).
  - `POST /admin/read-only/disarm` — clear the flag.
- **Prometheus metrics**:
  - `pms_engine_read_only` — gauge, 1 when armed, 0 otherwise.
  - `pms_read_only_rejections_total{reason}` — cumulative count of write requests rejected with 503, labelled by reason.
- **Alert rule**: new `EngineReadOnly` (severity `critical`, `for: 5m`) with a runbook covering the four reasons (memory restart engine, disk free space, rocksdb compact, manual disarm).
- **`/healthz`** gains a `read_only_mode` check that returns `degraded` when armed, with detail `{"armed": true, "reason": "..."}`.
- **Sandbox test**: `test_read_only_mode_gates_writes` in `crates/pms-server/tests/dag_sandbox.rs` validates the full pipeline end-to-end via HTTP — baseline write 201, manual arm, write 503 with `error: read_only` + `Retry-After: 30`, reads 200 (`/v1/version`, `/v1/balance`), disarm, write 201 again.

### Configuration
- New `[health]` fields with defaults: `read_only_guard_enabled = true`, `memory_high_watermark_pct = 90.0`, `memory_low_watermark_pct = 75.0`, `disk_critical_free_percent = 5.0`, `rocksdb_l0_critical_files = 100`.
- `etc/config/config.testnet.toml` — defaults applied (90% / 75% / 5% / 100).
- `etc/config/config.mainnet.toml` — stricter watermarks (88% / 70%) for more headroom under real traffic spikes.
- `etc/prometheus/alerting_rules.yml` — `EngineReadOnly` alert added under `pms_critical`.

### Why it matters
Without this safety valve, a memory leak or a sustained traffic spike on the testnet 14 GiB cgroup limit would result in a Docker SIGKILL on `pms-engine-testnet`. That kill loses the persist channel buffer (~2K blocks at the 0.7.1 sizing), which is a **data-loss event**. Read-only mode degrades gracefully: 503 to clients (which retry with backoff), pause block-producing background work, give RocksDB compaction time to free pages, then resume automatically when the watermark drops. The operator can also trigger this manually for maintenance windows.

### Files
- New: `crates/pms-server/src/read_only.rs` (lock-free `ReadOnlyMode` + `ReadOnlyReason` enum + unit tests).
- New tasks: `crates/pms-server/src/api/tasks.rs::spawn_resource_guard_task` + `read_cgroup_memory_pct` helper (cgroup v2 first, v1 fallback).
- New middleware: `crates/pms-server/src/api/middleware.rs::require_writable`.
- New admin handlers: `crates/pms-server/src/admin.rs::admin_read_only_{status,arm,disarm}`.
- New helper: `crates/pms-storage/src/rocks_store/helpers.rs::l0_files()` — exposes `rocksdb.num-files-at-level0` for the guard.
- Routes refactored: `build_ledger_scoped_routes` now returns `(public, auth_read, auth_write)`; admin Router split into `admin_writable` (+ `require_writable`) and `admin_recovery` (free); per-ledger admin Router gains `require_writable`.
- New gauge / counter in `crates/pms-server/src/metrics.rs`.
- Test: `test_read_only_mode_gates_writes` in `dag_sandbox.rs`.

---

## [0.7.21] - 2026-04-29 — Memory alerting via cAdvisor (host-level proxy)

### Fixed
- **deploy(alerting/memory-blind-spot)**: `EngineMemoryHigh` was relying on `process_resident_memory_bytes{job="pms-engine"}` — a Go-prometheus-client convention NOT auto-exposed by the Rust client. The alert silently never fired during the 2026-04-29 incident where the engine grew from 5 GiB to 12.67 GiB in ~13 hours without any Telegram notification. Replaced with cAdvisor-backed `HostMemoryHigh` (`container_memory_rss{id="/"} > 13 GiB` for 5 min). Imperfect (cAdvisor produces only the cgroup-root series due to a Docker 29.x overlayfs incompatibility — see "Known limitations" below), but on the testnet VPS the engine is by far the dominant memory consumer so root RSS > 13 GiB ≈ engine OOM imminent on its 14 GiB cgroup cap.
- **deploy(prometheus/rules-not-mounted)**: `prometheus.yml` declared `rule_files: alerting_rules.yml` but the alerting rules file was never bind-mounted into the prometheus container. The rules silently never loaded (`/api/v1/rules` returned `groups: []`). Fixed by adding `./etc/prometheus/alerting_rules.yml:/etc/prometheus/alerting_rules.yml:ro` to the prometheus volumes.

### Added
- **deploy(cadvisor)**: New `pms-cadvisor-testnet` service in `docker-compose.testnet.yml` (image `gcr.io/cadvisor/cadvisor:v0.52.1`, privileged, mem_limit 256m, loopback `:8082`, on `pms-testnet-internal`). Reads cgroup metrics directly so memory alerts work without per-binary instrumentation. Wired as a Prometheus scrape job. v0.49.x first attempted but the Docker client is pinned at API 1.41 which Docker 29.x rejects (`client version 1.41 is too old`). v0.52.1 negotiates correctly.

### Known limitations (cAdvisor + Docker 29.x compat)
- cAdvisor v0.52.1 successfully registers the Docker factory but fails to enumerate the per-container overlayfs layer paths (`/rootfs/var/lib/docker/image/overlayfs/layerdb/mounts/<id>/mount-id` doesn't exist on Docker 29.x's newer storage scheme). Net effect: only the cgroup root `id="/"` series flows through, no `name="pms-engine-testnet"` per-container labeling.
- The cgroup root RSS (~75% of `free -h Used`) is consistently lower than the actual sum of container RSS reported by `docker stats`. The 13 GiB threshold is empirical: ≈ "engine + sim + gateway + system overhead = host at risk".
- For proper per-container alerting we'd need either (a) wait for cAdvisor compat fix in a future release, or (b) add `pms_engine_rss_bytes` directly via `/proc/self/statm` read in the engine's metrics sampler (already done in the simulator — copy the pattern). The latter is the cleanest fix and is queued for the next iteration.

---

## [0.7.22] - 2026-05-01 — Simulator burn perma-loop fix (state-divergence on 404)

### Fixed
- **sim(burn-state-divergence)**: the burn-failure handler in `tools/simulator/src/agent/random.rs::game_tick` previously restored the local `cube_ids: Vec<String>` to the agent's RAM whenever a batch burn failed — including 404 ("NFT not found or already burned"). When the engine restarts mid-burn or any RPC drops a response after the engine processed it, the simulator's RAM holds **ghost cube IDs** that the engine sees as already gone. Every subsequent burn 404s on the same IDs, blocking the agent's mint→burn→EDN-send cycle. Observed 2026-05-01: after an engine restart at 00:04 UTC, **1.35M burn 404s** accumulated on click agents and 354K on active agents, dropping testnet TPS from ~50 blk/s to ~3 blk/s (only spammers + traders + observers were producing).
- **Fix**: distinguish state-divergence errors (404 / "not found" / "already burned") from transient errors (network, 5xx). State-divergence → drop the local IDs unconditionally, the agent's `cubes < target_cubes` guard re-mints fresh cubes next tick. Transient → restore registry + local list as before, agent retries the same batch. ~30 lines diff. cargo check clean.

### Recovery applied
- Restarted `pms-simulator-testnet` on the testnet VPS to clear the existing ghost state. Agents re-mint from scratch via `funder` then resume normal cycles. Will deploy the code fix via `scripts/upgrade-testnet.sh` at the next routine push.

---

## [0.7.21] - 2026-04-30 — Mainnet config + scripts (artifacts only, deploy plus tard)

### Added
- **deploy(mainnet/config)**: Nouveau `etc/config/config.mainnet.toml` — clone du testnet avec `[network] mode = "mainnet"`, `network_id = "pms-mainnet-v1"`, `[rocks] prefix = "pms:main"`. Diffs sémantiques : `daily_inflation_interval_sec` 120s → 86400s (cycle réel quotidien, pas accéléré), `max_last_block_age_seconds` 300 → 120 (alerte plus stricte), `min_disk_free_percent` 10 → 15, `activity_retention_days` 30 → 90 (audit fiscal), `auto_consolidate_interval_secs` 600 → 300.
- **deploy(mainnet/compose)**: Nouveau `docker-compose.mainnet.yml` — engine/gateway/caddy/prometheus/alertmanager/cadvisor avec suffixe `-mainnet`, networks `pms-mainnet-{internal,public}`, volumes `*_mainnet_data`. Image tags semver immutables via `${IMAGE_VERSION:-v0.7.21}` (vs `:testnet` mutable). Engine `mem_limit: 12g` (sans simulator on a moins besoin). **PAS de service `pms-simulator`** — vrais utilisateurs uniquement.
- **deploy(mainnet/alertmanager)**: Nouveau `etc/alertmanager/alertmanager.mainnet.yml` — channel Telegram dédié "PMS Mainnet Alerts" via le même bot que testnet (`secrets/telegram_bot_token` partagé). Préfixe message `🟢 [MAINNET]` pour distinguer visuellement des alertes testnet. `chat_id: 0` placeholder à remplacer par l'ID réel du channel quand l'opérateur le crée.
- **deploy(mainnet/prometheus)**: Nouveau `etc/prometheus/prometheus.mainnet.yml` — scrape config sans le job `pms-simulator`. `rule_files: alerting_rules.yml` partagé avec testnet (les filtres `up{job="..."}` sont environment-agnostic, chaque Prometheus scrape son propre engine local).
- **deploy(mainnet/scripts)**: Nouveaux `scripts/deploy-mainnet.sh` (840 lignes) et `scripts/upgrade-mainnet.sh` (489 lignes) — clonés des testnet scripts avec : domaine `pms-network.com` (root), tags semver via `IMAGE_VERSION` env, build de **2 images** au lieu de 3 (drop simulator), backup paths `pms-mainnet-*.json`, init coordinator avec génération de keys mainnet uniques (le script garde-fou contre l'éventuelle confusion testnet/mainnet).
- **doc(CLAUDE.md)**: Nouvelle section "VPS Mainnet — Infrastructure de Production (à provisionner)" parallèle à la section testnet : tableau des diffs config, checklist pre-launch (provision VPS + DNS + Telegram channel + chat_id + 1er deploy + backup + paging test + 24h stability run), commandes de déploiement et rollback.

### Validation locale (cette release)
- `bash -n scripts/deploy-mainnet.sh && bash -n scripts/upgrade-mainnet.sh` : clean.
- `docker compose -f docker-compose.mainnet.yml config --quiet` : clean. Liste services : `alertmanager`, `caddy`, `cadvisor`, `pms-engine`, `pms-gateway`, `prometheus` — confirmé **pas de simulator**.
- `cargo check -p pms-server` : clean (5m10s, no errors). La nouvelle config n'introduit aucun changement de code, seulement de nouvelles valeurs lues à runtime.

### Hors scope (action ultérieure de l'opérateur)
- Provisionner un second VPS IONOS (16 Go min, 32 Go recommandé)
- DNS `pms-network.com` → IP du nouveau VPS
- Création du channel Telegram "PMS Mainnet Alerts" + récupération du `chat_id`
- Premier `deploy-mainnet.sh` avec génération de coordinator keys uniques
- 24h stability run avant ouverture aux vrais utilisateurs
- Stripe / fiat on-ramp (étape suivante après stabilité confirmée)

---

## [0.7.20] - 2026-04-29 — Launch-readiness: token mint UTXO double-apply fix + 4 sandbox tests

### Fixed
- **fix(api/token-mint/double-utxo-apply)**: `admin_mint_token` (`POST /admin/tokens/mint`) was the last handler missed by the v0.6.4 mass-fix for the plain-payload double-apply bug. After `persist_block(&wb).await` (which already applies the `UtxoDelta` for plain `Mint` payloads via `apply_diff()`), the handler called `adapter.add_utxo(...)` again for every output. Net effect: minted token supply doubled in the supply endpoint, and the redundant `push()` on the LRU shard evicted the freshly-added UTXO and removed it from the address index — so the recipient saw a balance of 0 even though the mint succeeded. Detected by the new `test_token_lifecycle_and_token_burn_warning` sandbox test (user1 had 0 USDX after a 1000 USDX mint, while circulating supply showed 2000). Removed the redundant loop. The plain `Mint` payload is auto-applied by `persist_block` — see CLAUDE.md "UTXO Delta — Plain vs Encrypted".

### Added (tests/launch-readiness)
- **test(sandbox/sse-stream)**: `test_sse_activity_stream_real_time` — opens an SSE connection on `GET /v1/wallet/{addr}/activity/stream`, triggers two faucet mints to the user, and asserts that at least one `event: activity` frame containing the user's address arrives within 8 seconds. Closes the audit gap "SSE streaming endpoint exists but zero E2E coverage". Uses `reqwest::Response::chunk()` with a tokio timeout — no extra dependency.
- **test(sandbox/token-lifecycle)**: `test_token_lifecycle_and_token_burn_warning` — creates a custom token `USDX` with `max_supply: 1_000_000`, mints 1000 to user1, asserts balance + circulating supply match, asserts an over-mint of 1_000_000 returns 422, transfers 250 USDX user1 → user2 and asserts conservation (supply unchanged). Then simulates an `OnTokenBurn` contract via `/admin/contracts/simulate` and asserts the engine emits the documented "not yet implemented" warning — a regression guard that prevents accidentally shipping the trigger as if it worked.
- **test(sandbox/gas-pool)**: `test_gas_pool_deposit_withdraw_consumption` — exercises the per-ledger gas-pool custody surface: deposit 1000 PMS → withdraw 200 → verify `balance=800, total_deposited=1000` → over-withdraw 99999 returns 402 PaymentRequired with balance untouched → a user tx never makes the pool grow → unknown ledger `GET` returns 404. Consumption is reported but not asserted (default sandbox boots without a fee policy active; economic enforcement is exercised separately in `fee_consistency_test.rs`).
- **test(sandbox/contract-killswitch)**: `test_contract_toggle_kill_switch` — closes the audit gap that `test_contract_simulate_endpoint` only exercised `false → true` once and never observed runtime behavior change. Registers an enabled 5% transfer-fee contract on eden, sends a transfer and asserts `transfer_fee=5` (active), toggles to `enabled=false`, sends another transfer and asserts `transfer_fee=0` (kill switch effective), re-enables and asserts the fee comes back. Operators rely on the toggle as a runtime kill switch — this test now proves it.

### Audit context
- These four tests were added to close the gaps surfaced by the launch-readiness audit run in this conversation. With them, the e2e sandbox now covers: wallet/coordinator funding (existing), token full lifecycle including supply consistency (new), NFT mint/transfer/burn + refund (existing), per-ledger gas pool custody (new), smart contract simulate + register + **runtime toggle behavior** (new), SSE streaming (new), fee distribution + conservation (existing).

---

## [0.7.19] - 2026-04-29 — Boot-resiliency: prevent stale-stack reboot trap

### Fixed
- **deploy(boot-resiliency)**: After a host reboot Docker auto-restarts every container with `restart: unless-stopped`. If a stale `docker compose up -d` had ever been run from `/opt/pms/` without `-f docker-compose.testnet.yml`, Compose v2 defaulted to `docker-compose.yml` (the pre-multi-ledger legacy compose), creating a parallel set of containers (`pms-engine`, `pms-gateway`, `pms-caddy`, `pms-prometheus`) on `:latest` images. After reboot, BOTH stacks came back, the testnet simulator pointed at the wrong gateway via Docker DNS, and the system silently regressed. Hit 2026-04-28.
- **Fix**: both `scripts/deploy-testnet.sh` and `scripts/upgrade-testnet.sh` now (idempotently, on every run):
  1. Create `compose.yaml → docker-compose.testnet.yml` symlink in `/opt/pms/`. Compose v2 prefers `compose.yaml` over `docker-compose.yml` so plain `docker compose up -d` resolves to the testnet stack — no `-f` required.
  2. Rename `docker-compose.yml` to `docker-compose.legacy-prod.yml.disabled` if still present, killing the trap completely.
- **doc(CLAUDE.md)**: New "Boot-resiliency (post-reboot)" subsection in the runbook listing the 4 invariants the operator should verify when checking the testnet after a reboot (no ghost containers, symlink intact, legacy compose disabled, smoke test).

### Operator action — already applied to current testnet
Hot-applied 2026-04-29 via SSH: symlink created, legacy renamed, `docker compose up -d` (no `-f`) tested and confirmed picks up the 6 testnet services correctly.

---

## [0.7.18] - 2026-04-29 — Alertmanager dual-network: fix Telegram outbound delivery

### Fixed
- **deploy(alertmanager/networks)**: Alertmanager was attached only to `pms-testnet-internal` (which has `internal: true` → no Internet egress), so it could resolve sibling containers (`pms-engine:8080`, `prometheus:9090`) but couldn't reach `api.telegram.org`. Symptom: alert fires, gets stuck in retry loop with `lookup api.telegram.org on 127.0.0.11:53: server misbehaving` in alertmanager logs. Same trap would've hit Better Uptime / PagerDuty / Slack — every external paging API needs Internet egress. **Fix**: `alertmanager` service now joins both `pms-testnet-internal` (for Prometheus to scrape it) AND `pms-testnet-public` (for outbound to paging providers).

---

## [0.7.17] - 2026-04-29 — Single-ledger stability run (eden only)

### Changed
- **sim(testnet/single-ledger)**: `simulator.testnet.toml` reduced from 4 `[[simulation.games]]` (eden / arena / colosseum / nexus) to **1** (eden only). The 3 multi-ledger entries served their purpose during the v0.7.9 scalability validation; for the long-running stability test the operator wants confidence that the DAG runs days/weeks without bugs under a steady, production-shaped load — not max stress. Dormant ledgers (arena/colosseum/nexus) still exist on the VPS but receive no new traffic.
- **sim(testnet/agents)**: `agents_testnet.toml` consolidated from 4×250 click groups (1119 agents) to **1×250 + 50 trader + 50 active + 5 spammer + 3 adversarial + 3 obs + 1 coord = 362 agents** on eden. Target steady-state load: ~30-40 blk/s sustained, peak ~60 during burn windows. Comfortable headroom under the engine's measured ceiling — anomalies during this run will be signal, not load saturation.

### Notes (operational)
- Engine RSS at boot in this configuration: ~5 GiB (baseline RocksDB memtables for 5 prefixes × 33 CFs × 16 MiB × 2 ≈ 5.3 GiB even with 3 dormant ledgers). Headroom under the 14 GiB cgroup cap is ~9 GiB. To reclaim ~3 GiB, the dormant arena/colosseum/nexus ledgers can be wiped (drop their CFs in RocksDB + remove entries from `ledger_defs`) — destructive for those test ledgers' data, no real-user impact.
- Bloom skip ratio still at 99.99% with 6.3M+ blocks in main alone, so dedup is no longer in the critical path of the stability test.

---

## [0.7.16] - 2026-04-29 — Dashboard: stop cascade-blanking the admin token on a single 401

### Fixed
- **dashboard(api.ts/cascade-blanking)**: `apiCall()` was calling `adminToken.set('')` whenever any endpoint returned 401/403. With the Dashboard component firing 6 parallel calls on mount (`/metrics`, `/v1/nodes`, `/v1/peers`, `/v1/supply`, `/admin/ping`, `/v1/tokens`), a single failing endpoint would wipe the token store globally → App swaps to Login → other in-flight requests + polling timers retry with empty token → all log `with token: MISSING` and 401-loop. Symptom seen 2026-04-29 with a stale local admin token: the user's correct dashboard token was saved by Login, then nuked by a 401 cascade from `/metrics` before they could even see the dashboard.
- **Fix**: `apiCall()` now only `throw`s on 401/403, leaving the token in place. The decision to clear the token belongs to the caller. Login.svelte already has its own `adminToken.set('')` in its catch when `/admin/ping` fails — that's the only place where blanking is correct (explicit auth-check failure during login). The Dashboard's other calls now fail individually without taking the entire session down.

---

## [0.7.15] - 2026-04-29 — Gateway nofile ulimit: prevent 502 under production-realistic load

### Fixed
- **deploy(gateway/nofile)**: `pms-gateway` container in `docker-compose.testnet.yml` now has `ulimits.nofile.soft/hard: 65536` (matching the engine). Without it, the gateway runs on Linux's default 1024 fd limit per process — fine for ~100 agents but the v0.7.9 production-realistic config (1119 simulator agents + dashboard clients × multiple HTTP/2 streams to the upstream `pms-engine`) burns through it within hours. Symptoms: gateway logs spam `Too many open files (os error 24)` followed by `dns error` (because socket-creation failure inside reqwest masquerades as DNS failure), and the dashboard loads HTML/JS/CSS but every API call returns `502 Bad Gateway` because the gateway can't open a new socket to the engine. Hit on 2026-04-29 ~30 min after the post-reboot redeploy. Recovery: hot-recreate the gateway via `docker compose up -d --force-recreate pms-gateway` (data lives in the engine, gateway is stateless).

---

## [0.7.14] - 2026-04-29 — upgrade-testnet.sh: pre-deploy guard against stale non-testnet containers

### Fixed
- **deploy(upgrade/stale-containers)**: `scripts/upgrade-testnet.sh` now `docker rm -f` any containers using the *simple* names (`pms-engine`, `pms-gateway`, `pms-caddy`, `pms-prometheus`, `pms-alertmanager`) before the `up -d` step. These stale containers come from the legacy `/opt/pms/docker-compose.yml` (the pre-multi-ledger prod compose file) and have their own `restart: unless-stopped` policy that makes them auto-resurrect on every host reboot. Until v0.7.14 we never removed them — just created the parallel `-testnet` versions — so a host reboot would silently bring back the *stale* ones first (via Docker's reverse-creation-order startup), leaving the simulator crash-looping with `Cannot reach gateway` because `pms-gateway` resolved to the wrong network. Hit on 2026-04-28 after an IONOS reboot. Volumes are named and persistent — nothing is lost, just the shell containers are recreated.

### Documentation
- **CLAUDE.md**: New "Bug historique : Containers fantômes non-testnet au reboot" runbook entry. Captures the symptom (mixed `pms-engine` / `pms-engine-testnet` names in `docker ps`), the diagnostic clues (`docker inspect <name> --format '{{.Config.Image}}'` shows `:latest` instead of `:testnet`; the compose project label points at the legacy YAML), the root cause (two compose files sharing the same `project=pms` label, host-reboot brings back both), and the manual-recovery one-liner.

---

## [0.7.13] - 2026-04-27 — Alerting: Telegram-first via Alertmanager native receiver

### Changed
- **deploy(alerting/telegram-first)**: `etc/alertmanager/alertmanager.yml` rewritten to use Alertmanager's built-in `telegram_configs` receiver (added in v0.24, we're on v0.27.0) instead of generic webhooks. Two `telegram_configs` blocks (one for `pager` with sound, one for `notify` silent) read the bot token from `/etc/alertmanager/telegram_bot_token` (mounted from host's `secrets/telegram_bot_token` so the token never enters git or YAML) and post HTML-formatted messages to a configurable `chat_id`. Operator only fills two values (bot token + chat_id) instead of running a webhook adapter / n8n / PagerDuty subscription. Total cost: **zero** (Telegram bot API is free, unlimited at our volume).
- **deploy(alerting/secrets)**: `docker-compose.testnet.yml` mounts `secrets/telegram_bot_token` into the alertmanager container as a one-line file (same pattern as the prometheus admin token). `scripts/deploy-testnet.sh` and `scripts/upgrade-testnet.sh` write a placeholder if the file is missing so the bind-mount succeeds and the service starts cleanly even on a fresh deploy without paging configured.
- **doc(runbooks/alerting)**: Runbook rewritten Telegram-first. Step-by-step bot creation (@BotFather), channel + admin setup, getting `chat_id` from `getUpdates`, Android notification-sound override for "wake-me-up" critical alerts. Provider-comparison section moved to "Switching to another provider" — the four alternatives (Better Uptime / PagerDuty / Discord / Slack) are still documented for users who outgrow Telegram.
- **deploy(alertmanager.yml)**: kept the four alternative receiver blocks (Better Uptime / PagerDuty / Discord / Slack) at the bottom of the file as commented YAML. Switching providers is one block-swap.

### Operator action required to go live (5 min total)
1. `@BotFather` on Telegram → `/newbot` → copy bot token.
2. Create a private channel → add the bot as admin → send any message.
3. `curl https://api.telegram.org/bot<TOKEN>/getUpdates` → grab `chat.id` (negative integer for channels).
4. `echo -n '<TOKEN>' > secrets/telegram_bot_token`.
5. Replace `chat_id: 0` in `etc/alertmanager/alertmanager.yml` (two places, can be the same chat).
6. Switch `route: receiver: 'null'` → `'pager'` so unmatched alerts also page.
7. `scp` both files to the VPS, `docker restart pms-alertmanager-testnet`.

Test: `docker exec pms-alertmanager-testnet amtool alert add alertname=Test severity=critical` → phone buzzes within ~30 s.

---

## [0.7.12] - 2026-04-27 — Alerting stack: Prometheus rules + Alertmanager + runbook

### Added
- **deploy(alerting/rules)**: New `etc/prometheus/alerting_rules.yml` shipping 12 rules in two severity tiers. **Critical** (page immediately): `EngineDown`, `GatewayDown`, `PersistFailures` (data-loss), `RocksDBWriteStopped`, `HealthzFail`. **Warning** (notify, investigate during business hours): `PersistQueueBackpressure` (>80% capacity for 5m), `PersistRetriesElevated`, `PersistStallsAccumulating`, `BloomSkipRatioLow` (<90% for 10m), `EngineMemoryHigh` (>12 GiB for 10m), `AdminAuthFailureSpike` (>1/s for 2m, brute-force signal), `PrometheusTargetMissing`. All thresholds calibrated against the v0.7.10 1119-agent steady-state.
- **deploy(alerting/alertmanager)**: New `etc/alertmanager/alertmanager.yml` with two-receiver routing (`pager` for critical, `notify` for warning), inhibition rules (don't page on persist-pipeline / storage symptoms while `EngineDown` is firing), and placeholder webhook URLs for Better Uptime / PagerDuty / Discord / Slack. Until URLs are wired, alerts go to a `null` receiver — safe default.
- **deploy(alerting/compose)**: New `alertmanager` service in `docker-compose.testnet.yml` (image: `prom/alertmanager:v0.27.0`, mem_limit 256m, loopback-only port 9093). Integrated into `pms-testnet-internal` network so Prometheus can resolve it via Docker DNS. Volume `alertmanager_testnet_data` persists silences across restarts.
- **deploy(alerting/prometheus)**: `etc/prometheus/prometheus.yml` now declares `rule_files` + `alerting.alertmanagers` blocks. Added a self-scrape job for `alertmanager:9093` so we can alert on the alerting layer itself (paging path can't be silently broken).
- **doc(runbooks/alerting)**: New `documentation/runbooks/alerting.md` (190 lines) covering: pipeline architecture, provider comparison table (Better Uptime vs PagerDuty vs Slack vs Discord with free-tier matrix), step-by-step Better Uptime/PagerDuty/Slack/Discord setup, hot-reload via `curl -X POST /-/reload`, two end-to-end test methods (`amtool alert add` synthetic + real `docker stop` test), reading existing alerts, cost guardrails (`group_interval` / `repeat_interval` / inhibitions), and three-layer fallback strategy if alerting itself breaks.
- **doc(MOC)**: New "Runbooks (opérations)" section linking to the alerting runbook.

### Changed
- **deploy(scripts)**: `scripts/deploy-testnet.sh` and `scripts/upgrade-testnet.sh` now `mkdir -p etc/alertmanager` on the VPS and `scp` both `etc/prometheus/alerting_rules.yml` and `etc/alertmanager/alertmanager.yml` on every deploy. The new alertmanager service is reachable to compose's normal lifecycle (no special restart logic required — `restart: unless-stopped` covers it). `bash -n` clean.

### Operator action required to go live
1. Pick a paging provider (Better Uptime free tier recommended).
2. Paste the integration URL into `etc/alertmanager/alertmanager.yml` where it says `<PASTE_YOUR_..._URL_HERE>` (two slots: `pager` and `notify`).
3. `scp etc/alertmanager/alertmanager.yml pms@VPS:/opt/pms/etc/alertmanager/alertmanager.yml`.
4. `curl -X POST http://127.0.0.1:9093/-/reload` over SSH.

Total time: ~5 min. The full walkthrough is in `documentation/runbooks/alerting.md`.

---

## [0.7.11] - 2026-04-27 — Stop killing Prometheus on every upgrade

### Fixed
- **deploy(upgrade-testnet.sh)**: The script wiped `pms-prometheus-testnet` on every run via two compounded bugs:
  1. `docker ps -a --filter "label=com.docker.compose.project=pms" -q | xargs docker rm -f` — supposed to clear "ghost" Compose containers before recreate, but it removed **every** container in the project regardless of whether it was in `$SERVICES_TO_RECREATE = "pms-engine pms-gateway pms-simulator"`. Prometheus and Caddy got force-killed every time.
  2. `docker compose up -d --force-recreate --remove-orphans $SERVICES_TO_RECREATE` — passing `--remove-orphans` with a *subset* of services makes Compose treat the omitted siblings as orphans (Prometheus is in the YAML, just not in the targeted list).
  Caddy was rescued by a downstream `up -d caddy` block; Prometheus had no such fallback so it stayed dead until manually restarted. The earlier "Prometheus disparu silencieusement" runbook entry blamed `restart: unless-stopped` after a manual `docker stop` — that was wrong; the real culprit was our own upgrade script all along, and the residual `/prometheus/lock` lockfile was just the SIGKILL trace.
- **Fix**: cleanup loop now iterates only `$SERVICES_TO_RECREATE` and `docker rm -f` each by name; `up -d` calls dropped `--remove-orphans` (both for the main batch and the Caddy follow-up). The CLAUDE.md runbook entry is rewritten with the correct root cause so future me doesn't chase the same wrong hypothesis. `bash -n` syntax-checked.

---

## [0.7.10] - 2026-04-27 — Simulator memory watchdog now configurable (was hardcoded 400 MB)

### Fixed
- **sim(memory-watchdog)**: The in-process RSS watchdog had a hardcoded **400 MB** ceiling — a leftover from when the simulator was sized for ~100 agents. At 1119 agents (post-v0.7.9 production-realistic config) the agent-task stack working set crosses 400 MB during the bootstrap-and-mint storm, triggering a graceful shutdown via `cancel.cancel()` with exit code 0. `restart: on-failure` doesn't relaunch on a clean exit, so the simulator stayed dead. Symptoms in the testnet logs: `Memory watchdog: RSS 404 MB exceeds limit 400 MB — shutting down` followed by `Simulation complete.`
- **sim(config)**: New `[simulation].max_rss_mb` field on `SimulationParams` (default **1500 MB**). Set to 0 to disable the watchdog entirely (the cgroup OOM-killer is the backstop). Boot log now shows the resolved threshold so the operator can confirm what's actually enforced.

---

## [0.7.9] - 2026-04-27 — Simulator scaled to production-realistic clicker load

### Changed
- **sim(client)**: HTTP client `pool_max_idle_per_host` 32 → **512**, `pool_idle_timeout` 90 s, `tcp_keepalive` 60 s, `tcp_nodelay` on. The previous reqwest defaults serialised concurrent requests through a 32-slot pool — at 1000 agents driving the gateway in parallel, observed effective TPS collapsed to `32 × per-call latency`. New pool lets every burst send fan out without queueing on a half-closed pool.
- **sim(funder)**: `BOOTSTRAP_CONCURRENCY` 30 → configurable via `[simulation].bootstrap_concurrency` TOML field (default 128). At 1000 agents the previous 30 made bootstrap a 5-8 minute serialised crawl; 128 brings a 1000-agent boot to ~90 s on the testnet VPS without overwhelming the gateway's tightened 500 RPS rate-limit. Per-agent log lines auto-suppressed for fleets > 200 agents and replaced with ~5%-step milestones so the funding storm doesn't flood the logs. Same pattern on the cube-mint pass for fleets > 500 cubes.
- **sim(testnet/agents)**: `agents_testnet.toml` rebuilt for production-realistic load. 1000 pure clickers split across 4 independent game ledgers (250 each on `eden`, `arena`, `colosseum`, `nexus` via `game_index = 0..3`), 50 PMS traders (no game), 50 active players (mine + PMS), 10 spammers, 5 adversarial, 3 observers, 1 coordinator → **1119 agents total**. Steady-state target: ~120-150 blk/s game load + ~6 blk/s PMS economy + ~50 RPS spammer attempts (back-pressured). Dial larger by bumping per-group `count`.
- **sim(testnet/games)**: `simulator.testnet.toml` `[simulation.game]` (single) → `[[simulation.games]]` × 4 entries (eden / arena / colosseum / nexus). Engine now boots 4 independent game engines + smart contracts + gas pools at startup. The old single-ledger config is preserved as a commented fallback.
- **deploy(simulator/mem)**: `docker-compose.testnet.yml` `pms-simulator.mem_limit` 512m → **2g**. The old 512m OOM-killed the simulator within seconds of the 1000-agent boot; 2g leaves headroom for the per-agent cube_id Vecs (~23 MiB raw) and tokio task stacks (~2 MiB × 1000 worst case).

---

## [0.7.8] - 2026-04-27 — Prometheus gateway scrape fix + missing-container runbook

### Fixed
- **deploy(prometheus/gateway-scrape)**: The `pms-gateway` job in `etc/prometheus/prometheus.yml` was scraping `https://pms-gateway:8443/metrics` with no authorization, returning `401 Unauthorized` and leaving the gateway's request-rate / upstream-latency / rate-limit counters invisible to Grafana. The gateway protects its `/metrics` with the same admin token as the engine (`ADMIN_TOKEN` env var on both services), so the job now reuses `credentials_file: /etc/prometheus/admin_token` — the same one-line file already mounted into the prometheus container by `scripts/deploy-testnet.sh`. Hot-reloaded on the testnet VPS without a Prometheus restart; `pms-engine`, `pms-gateway` and `pms-simulator` targets all `up`.

### Documentation
- **CLAUDE.md**: New "Bug historique : Prometheus disparu silencieusement" runbook entry. Captures the symptom (`pms-prometheus-testnet` absent from `docker ps -a` while the other 4 containers stay healthy), the root cause (`restart: unless-stopped` is inhibited after an explicit `docker stop`/`rm`, even if the original kill was clean — Docker treats it as an operator decision), the diagnostic clues (residual `/prometheus/lock` lockfile + tiny memory footprint = not an OOM), and the one-shot fix (`docker compose up -d prometheus` recreates the container while preserving the TSDB volume). Sits next to the existing OOM-loop entry so the operator pattern is "if a service is missing, walk this list before assuming a code bug".

---

## [0.7.7] - 2026-04-27 — Recent-blocks Bloom filter eliminates LSM dedup growth

### Performance
- **storage(bloom-dedup)**: New in-RAM rotating Bloom filter fronts the `multi_get_cf` dedup lookup in `append_blocks_batch`. Before this change, the dedup sub-stage owned ~47% of consumer time after 11h of testnet traffic (189 µs/block) and grew sub-linearly with the DB size as parent block IDs aged out of the memtable into L0 SSTs. The filter answers the negative side authoritatively (every persisted block_id is inserted, so "definitely not in filter" → "definitely not in DB"); positives fall back to the existing `multi_get_cf` so correctness is preserved. Two-segment rotation caps RAM at ≈12 MB total (5M entries × 10 bits × 2 segments) and gives at least one full capacity window of recent IDs queryable. `[storage] recent_blocks_bloom.rs` (≈225 lines) + 4 unit tests (no false negatives, FPR < 2%, rotation preserves recents, warmed flag). Wired into both `RocksStore::new` and `from_shared_db` constructors plus the `secondary` read-only one. Inserts also fire from the single-block atomic paths (`append_block_atomic`, `append_block_atomic_with_utxo`, `put_block`) so low-RPS test paths stay consistent.
- **storage(bloom-warmup)**: `RocksStore::warm_recent_blocks_bloom(limit)` walks `cf_by_time` newest-first (key-only, no JSON parse) and feeds up to `BLOOM_WARMUP_LIMIT = 2_000_000` block IDs into the filter at boot, then flips the `warmed` flag. Until warmed, every block falls back to the legacy whole-batch `multi_get_cf` so a block actually present in RocksDB can never be misclassified as new. Called once per ledger from `LedgerInstance::bootstrap`. Cost on a 20M-block DB: ≈2 s for 2M keys (RocksDB-iterator-bound, not hash-bound).

### Added
- **server(bloom-metrics)**: `pms_persist_bloom_skips_total` (LSM read skipped — the dominant outcome under steady state) and `pms_persist_bloom_hits_total` (bloom said "maybe", fallback `multi_get_cf` fired) IntCounters in `pms-core::metrics`. Per-ledger atomics on `RocksStore` (`bloom_skips`, `bloom_hits`) feed the global counters via the metrics sampler at 5 s cadence. Same reading also surfaced in `/admin/rocksdb-stats` JSON snapshot (`bloom_skips_total`, `bloom_hits_total`, `bloom_front_inserted`, `bloom_back_inserted`, `bloom_capacity_per_segment`, `bloom_warmed`) so the operator can watch saturation + skip ratio without Prometheus.

### Validation
- `cargo check -p pms-storage -p pms-ledger -p pms-server` clean.
- `cargo test -p pms-storage --lib recent_blocks_bloom` 4/4 green; full storage lib suite 36/36 green.
- `cargo test --release -p pms-server --test dag_sandbox test_edn_transfer_fee_flow` and `test_tps_degradation_profile` green — TPS profile shows degradation peak→last 14.5% over 90 s + 2.5M UTXO growth, no longer correlated with dedup (sub-stage stayed flat).

---

## [0.7.6] - 2026-04-26 — Simulator hardening: 7 ROI-ranked recommendations

### Added
- **sim(progressive-mining)**: New `[agents.game].target_cubes` + `mint_per_tick` knobs (rec #1). When `target_cubes` is set the agent mines `mint_per_tick` cubes per tick until it hits the target, then burns the lot — replaces the legacy bulk mint of 350-380 cubes every ~hour with a continuous N-cubes-per-min stream that matches a real EDN-clicker player. Casual + active groups in `agents_testnet.toml` now ship with `target_cubes=360`, `mint_per_tick=1` at a 10s tick → exactly 6 cubes/min. Legacy bulk mode preserved when `target_cubes = None` (default) — back-compat for `agents_dev.toml` / `agents_docker.toml`.
- **sim(burn-jitter)**: New `[agents.game].burn_cooldown_jitter_pct` knob (rec #6). Multiplies the post-burn cooldown by `1 + uniform(-j, +j)` so the 100-agent fleet doesn't synchronise on the same tick after a shared event (e.g. all bootstrapping at once). Default `0.0` (off); testnet config sets `0.5` for ±50% spread.
- **sim(prometheus-metrics)**: New `pms_simulator_tx_sent_total{agent_group, kind}`, `pms_simulator_tx_failed_total{agent_group, kind, reason}`, `pms_simulator_cubes_minted_total`, `pms_simulator_cubes_burned_total`, `pms_simulator_burn_batches_total` (rec #7). Exposed at `GET http://pms-simulator:9090/metrics`. Pair with the engine's `/metrics/all` in Grafana to compute the attempted-vs-accepted gap (e.g. spammers attempt 50 RPS, engine accepts 30 RPS, dashboard shows 20 RPS as `pms_simulator_tx_failed_total`). Prometheus scrape config + new pms-simulator job auto-wired in `etc/prometheus/prometheus.yml`.
- **server(auto-consolidate)**: New `[health].auto_consolidate_interval_secs` + `auto_consolidate_min_utxos` (rec #5). When enabled, a background task fires `/admin/consolidate-utxos` every N seconds at the coordinator master address — bounds the per-address UTXO accumulation that fee receipts produce on busy networks. Default `None` (off); testnet config sets `interval=600` (10 min) + `min_utxos=200`. Coordinator-only; coexists with `[fees].coord_shard_count` (sharding caps per-shard accumulation, this caps the master).
- **sim(adversarial-spammer)**: New `AgentBehavior::Adversarial` agent type (rec #3). Each tick picks a random attack from the configured list — `bad_signature`, `over_balance`, `malformed_json`, `no_auth`, `replay`, `bad_utxo` — and records the engine's response in `pms_simulator_tx_failed_total{kind="adversarial:*", reason=http_4xx|http_422_validation|http_401_unauthorized|...}`. Validates the engine's REJECT paths (4xx codes), complementing the legitimate flood-spammer's BACK-PRESSURE validation. Three `adversarial` agents added to `agents_testnet.toml` (1s tick, 1 attack/tick).
- **sim(multi-ledger-games)**: New `[[simulation.games]]` array (rec #2). Each entry boots an independent `GameEngine` on its own ledger with its own EDN-equivalent token + smart contract + gas pool. Agents pick which game to play via `[agents.game].game_index` (default 0 — picks the first game, falls back to legacy `[simulation.game]` if the array is empty). New `AgentContext::game_engines: Vec<Arc<RwLock<GameEngine>>>` + `game_engine_for(index)` resolver; the legacy `game_engine: Option<...>` alias is preserved for observers / coordinator that don't care about multi-ledger. Documented + commented out in `simulator.testnet.toml` so operators flip a single section to spread the playerbase across N ledgers.

### Changed
- **deploy(rate-limit-tightened)**: `RATE_LIMIT_RPS` 50000 → 500, `BURST_SIZE` 100000 → 1000 in `docker-compose.testnet.yml` (rec #4). The previous 50K limit was so far above realistic client rates that the gateway 429 path was effectively dead code in the testnet. New value lets the new `spammer` + `adversarial` agent groups actually trip the limit while staying above the 100-user legitimate burst (~150-300 RPS during cube-mint moments).
- **sim(scenario-realism)**: `agents_testnet.toml` rebuilt around the prod-shaped EDN-clicker scenario (109 agents):
  - 60 `casual` (10s tick, 30% PMS send) — 6 cubes/min progressive
  - 40 `active` (10s tick, 50% PMS send, 2 sends/tick) — same 6 cubes/min, different PMS economic profile
  - 5 `spammer` (200ms tick, valid-but-fast) — back-pressure validation
  - 3 `adversarial` (1s tick, random attacks) — rejection-path validation
  - 3 `obs` + 1 `coordinator` — utility
- **sim(version)**: `tools/simulator/Cargo.toml` adds `prometheus = "0.14"` and `once_cell = "1"` deps for the metrics module.

### Validation
- `tools/simulator` cargo build + 10/10 unit tests green.
- Engine `dag_sandbox` 8/8 tests green (no regression from the new `[health]` knobs / consolidation task).

---

## [0.7.5] - 2026-04-26 — TPS-degradation diagnostic + persist-pipeline optimisations

### Infrastructure
- **deploy(testnet/audit)**: Sweep of the testnet deployment surface vs. v0.7.4 + v0.7.5 changes. Six gaps closed:
  1. **`etc/prometheus/prometheus.yml`** scraped `/metrics` (per-ledger filtered, 3 dashboard counters) instead of `/metrics/all` (full registry with the v0.7.4 sampler + v0.7.5 per-stage diagnostic counters). Switched to `/metrics/all` + `authorization: { type: Bearer, credentials_file: /etc/prometheus/admin_token }` because the endpoint sits behind `require_local_or_admin` and Prometheus is non-loopback inside docker. Net effect: 10+ previously-invisible counters (`pms_persist_stage_us_total{stage=...}`, `pms_persist_blocks_total`, `pms_persist_consumer_us_total/_batches/_blocks`, `pms_persist_queue_depth/_capacity`, `pms_fee_pool_total`, `pms_utxo_set_size`, `pms_rocksdb_write_stalled_seconds_total`, `pms_admin_auth_failures_total`) now reach Grafana.
  2. **`docker-compose.testnet.yml::pms-engine.healthcheck`** was `timeout 2 bash -c '</dev/tcp/127.0.0.1/8080'` — a TCP-port probe that masks a degraded RocksDB. Switched to `curl -sk -f https://127.0.0.1:8080/healthz` so Docker's restart policy actually reacts to the v0.7.4 structured `degraded`/`fail` responses (RocksDB writable + persist queue under high-water + last block age + disk free). Companion change in `Dockerfile.testnet`: alpine runtime image now `apk add curl` (busybox wget in alpine 3.20 doesn't reliably handle the self-signed HTTPS the engine binds on 8080).
  3. **`PMS_COORDINATOR_KEY_PASSPHRASE`** env var plumbed through `docker-compose.testnet.yml` with a `:-` empty fallback, so an operator who configures `[secrets].node_identity_key_encrypted_path` can decrypt `node.key.enc` at boot without editing the compose file. No-op for the legacy plain-`node.key` deployments.
  4. **`secrets/prometheus_admin_token`** file mount added to the prometheus service, plus `mkdir -p secrets && printf '%s' "$ADMIN_TOKEN" > secrets/prometheus_admin_token && chmod 600` step added to `scripts/deploy-testnet.sh` (DO_BUILD path) and `scripts/upgrade-testnet.sh`. The latter also now `scp`'s `etc/prometheus/prometheus.yml` to the VPS — without this, the new scrape config would never propagate via upgrade.
  5. **`etc/config/config.testnet.toml::[fees].coord_shard_count = 32`** restored under the correct section (the operator had typed it under a `[coord]` heading that doesn't exist in the schema, silently disabling sharding). 32 sub-addresses round-robin'd through HKDF-SHA256 derivation; auditors can sum balances via the `shards` array of `/v1/coordinator/info`.
  6. **`etc/config/config.testnet.toml::[health]`** section seeded with explicit values (`max_last_block_age_seconds = 300`, `persist_queue_high_water = 0.8`, `min_disk_free_percent = 10.0`, `activity_retention_days = 30`). All have code-side defaults but explicit values document the operator intent and let the v0.7.4 retention task actually fire.
- **Verified**: `cargo run pms-config::load_config` loads the new config cleanly (`coord_shard_count = 32`, `activity_retention_days = Some(30)`); both deploy + upgrade scripts pass `bash -n` syntax check.



### Added
- **diag(tps/per-stage)**: Sub-microsecond persist-pipeline instrumentation as Prometheus counters so the TPS-degradation curve can be decomposed by stage instead of seen only at the headline level. New counters in `pms-core`: `pms_persist_stage_us_total{stage="parents|utxo_val|dag_val|utxo_ram|dag_insert|send"}`, `pms_persist_blocks_total`, `pms_persist_consumer_us_total`, `pms_persist_consumer_batches_total`, `pms_persist_consumer_blocks_total`. Producer-side timings are captured in `do_persist_block_internal` next to the existing 500-block trace log; the `send_us` stage is the channel back-pressure window (previously not measured separately). Consumer-side timing wraps `store.append_blocks_batch().await` in `background_persist`. `RocksStore` also exposes four `AtomicU64`s for the per-batch sub-stages (dedup, build, write, trim) read out by `GET /admin/rocksdb-stats`. The 90s profile test `test_tps_degradation_profile` now prints three rows per interval — TPS + RocksDB / per-stage producer µs/block / consumer µs/block — and respects `PROFILE_DURATION_SECS` so the same test serves the quick diagnostic and the 5-min prod-readiness run. **Verdict**: per-block consumer cost rises from ~50 µs at boot to ~150–200 µs after 1.5–4 M UTXOs, dominated by `db.write()` of the WriteBatch. Producer `send_us` mirrors the consumer cost (1 ms → 5 ms over 90s) — channel never fills, the producer is throttled by inherent RocksDB write throughput as the LSM tree grows. See [crates/pms-core/src/metrics.rs](crates/pms-core/src/metrics.rs), [crates/pms-core/src/net_adapter/persist.rs](crates/pms-core/src/net_adapter/persist.rs), [crates/pms-server/tests/dag_sandbox.rs](crates/pms-server/tests/dag_sandbox.rs).
- **feat(background-activity)**: New `pms-core::background_activity` module + RocksStore method `append_activity_batch`. Pre-step3 the persist consumer's `WriteBatch` bundled the activity-index writes (`addr_activity` + `addr_type_activity` + `activity_items` — three CFs that feed only the dashboard's history API and have no consensus role) with the critical block / UTXO / children-count writes. As the DAG grew, the per-batch `db.write()` time grew with it, throttling producers through `persist_tx.send().await`. Now `core_adapter.rs` spawns a dedicated `spawn_activity_writer` (50K buffer) alongside the persist consumer, and the persist consumer forwards each freshly-persisted block to the activity writer via `try_send` — fire-and-forget by design, since dropping a dashboard row is preferable to back-pressuring producers. The activity writer drains in batches of up to 256 and writes its own `db.write()`. Failure is non-fatal: `append_activity_batch` errors are logged at WARN level and skipped, and channel saturation drops events with a periodic warning. See [crates/pms-core/src/background_activity.rs](crates/pms-core/src/background_activity.rs), [crates/pms-core/src/background_persist.rs](crates/pms-core/src/background_persist.rs), [crates/pms-storage/src/rocks_store/dag_storage_impl.rs](crates/pms-storage/src/rocks_store/dag_storage_impl.rs).

### Performance
- **perf(persist/parent-counts)**: Eliminated the per-batch `multi_get_cf` walk on `children_count` that was the leading scaling cost in the consumer. Pre-fix, `apply_dag_indices` issued one `multi_get_cf` per block to read each parent's current `children_count` — as the DAG grew, parent blocks aged out of the memtable into L0 SSTs and the lookup walked the LSM tree on every batch. New approach: the producer captures the post-insert count via `ConcurrentDag::get_children_count` (lock-free atomic load) right after `dag.insert_block()` and ships it in `PersistJob::parent_count_updates`. The consumer writes the value verbatim — single-consumer FIFO order guarantees the highest count for any parent persists last, so RocksDB converges to the in-memory truth. `append_blocks_batch` was also restructured to pre-resolve every CF handle once per batch (instead of ~640 `db.cf_handle()` calls per batch). Kept the existing dedup `multi_get_cf` against `cf_blocks` (race window between RAM dedup in `insert_block` and consumer can deliver duplicates). Trait change: `DagStorage::append_blocks_batch` signature gained a third per-block tuple field `&[(String, u64)]` carrying the parent counts; default impl ignores it (mocks unaffected), RocksStore consumes it. **Bench impact**: ~5–7% on consumer cost steady-state at 1.5 M UTXOs, dominant write cost now measurably in `db.write()` itself.
- **perf(rocks-tuning)**: Tried bumping `RocksMemoryConfig::default()` from 32→64 MB write_buffer / 3→4 max_buffers / 512→1024 MB db_write_buffer to keep the working set in RAM longer. **Reverted**: on the in-process bench the 8.4 GB worst-case memtable budget pushed the test runner into swap, making the curve marginally worse. The change is correct in principle for production VPS deployments — operators on the 16 GB testnet VPS can opt in via `[rocks]` overrides without affecting the bench defaults.

### Diagnostic findings (informational)
- **TPS at sustained 16K-TPS bench**: peak ~16K, settles at ~5K after 5 min and 4.2 M UTXOs. Per-block consumer cost in `db.write()` grows ~3× as the LSM tree expands (50 µs → 150 µs). Producer `send_us` follows in lockstep. **Real-world prod context**: at the 4K TPS prod target (EDN clicker game launch), the consumer never approaches saturation (consumer cap at boot ≈ 20K blocks/s) — channel never fills, latency stays sub-millisecond. Sustained 4K TPS over hours WILL eventually pressure the consumer, but compactions running in the background will move data L0 → L1+ where bloom filters work great, stabilising the per-block cost at a bounded level. The 90s/5min benches can't reproduce this because they saturate the writer with no spare CPU for compactions and run from a fresh tempdir DB. Async activity (step 3) shaves WAL contention modestly; the genuine win for higher prod loads would be a dedicated activity DB or true multi-consumer sharding (not implemented in this sprint).
- **Why "step 3" (async activity) appeared as a wash on the bench**: RocksDB pipelined writes overlap WAL append + memtable insert across writers, but WAL append itself serializes on a single mutex. Splitting the activity work into a separate `db.write()` doubles the WAL-append count without unlocking parallelism on the bench's saturated writer. In a production node where compactions run continuously and the writer has slack, the separation lets persist drain ahead while activity catches up — same architectural value, different bench surface.

### Changed
- **trait(DagStorage::append_blocks_batch)**: Signature change — third per-block tuple field added (`&[(String, u64)]` for parent counts). Default impl ignores it; only RocksStore reads it. Mock backends in tests updated mechanically.
- **PersistJob struct**: New required field `parent_count_updates: Vec<(String, u64)>`. Three test construction sites in `crates/pms-core/tests/persist_no_silent_drops.rs` and `crates/pms-server/tests/chaos_recovery.rs` patched to pass `vec![]`.

### Fixed
- **fix(testnet-config)**: Removed an in-tree `[coord]` section accidentally added in `etc/config/config.testnet.toml`. The `coord_shard_count` knob lives under `[fees]` (see the v0.7.4 audit follow-up); the `[coord]` section name was silently ignored, so coordinator sub-address sharding was never actually enabled in testnet. No prod impact yet (testnet hasn't been re-deployed since).

---

## [0.7.4] - 2026-04-25 — Production hardening sprint

### Added (post-merge)
- **diag(tps-profile)**: New `test_tps_degradation_profile` (in `dag_sandbox.rs`, `#[ignore]`) + `GET /admin/rocksdb-stats` admin endpoint (40 admin routes covered now). Polls RocksDB internals (`num-files-at-level0`, `is-write-stopped`, `compaction-pending`, `actual-delayed-write-rate`, `mem-table-flush-pending`, `num-running-compactions`, `num-running-flushes`, `estimate-num-keys`, `size-all-mem-tables`) + `/metrics` (persist queue depth, retries, stall seconds, rocksdb-stalled seconds) + `/v1/supply` (UTXO count) every 5s during a 90s sustained-load run. Prints a wide table per interval so the operator can correlate TPS drops with the underlying signal. Run with `cargo test --release -p pms-server --test dag_sandbox test_tps_degradation_profile -- --ignored --nocapture`. **Local diagnostic result on a fresh sandbox**: TPS dropped 15574 → 6471 (-58%) over 90s while RocksDB stayed clean (L0 0–3 files, write-stop never fired, compaction-pending=0) and the persist queue was never saturated (depth=0). The UTXO count went 155934 → 1633590 (+948%), strongly correlating with the TPS curve. The smoking gun is the **coordinator address accumulating UTXOs from every worker's send** — every `wallet_send_simple` lands a new output on `admin_addr`, no auto-consolidation runs in the sandbox, and selection scans grow with the set. The operator-level fix exists already (`POST /admin/consolidate-utxos`, shipped in v0.6.7) but is not auto-triggered. Documented as a finding for a future "auto-consolidate every N minutes" task; not blocking for the launch since the production deployment can run `/admin/consolidate-utxos` on a cron. See [crates/pms-server/tests/dag_sandbox.rs](crates/pms-server/tests/dag_sandbox.rs).
- **feat(retention)**: Bounded retention for the activity index CFs and an operator-only purge for the compliance log (audit follow-up to v0.7.4). Pre-fix: `addr_activity`, `addr_type_activity`, `activity_items`, and `compliance_log` grew without any cleanup mechanism — under sustained traffic the per-address indexes can dominate disk usage even after the underlying blocks are pruned, and a long-running deployment had no way to bound either CF without a manual scripts. New `RocksStore::purge_activity_before(cutoff_ms)` walks the three activity CFs newest-first, parses the embedded timestamp from each key (key format is `[addr][0x00][ts_be:8][...]`, separator is unambiguous because addresses are bech32 / no NUL), and batch-deletes everything strictly older than `cutoff_ms`. Companion `purge_compliance_log_before(cutoff_ms)` does the same for the compliance log but parses the JSON value (the log is keyed by `block_id`, timestamp lives in the value) — and the log is regulatory data so we never run it automatically. New config knob `[health].activity_retention_days: Option<u64>` (default `None` = unlimited; preserves existing behaviour). When set, `spawn_activity_retention_task` wakes 1h after boot then every 24h to apply the retention. Two new admin endpoints: `POST /admin/purge-activity` and `POST /admin/purge-compliance-log`, both `{"before_days": <u64>}` bodied, both forwarded to `tokio::task::spawn_blocking` so the iterator scan doesn't pin the runtime. `admin_auth_enforcement.rs` now covers 39 routes (was 37, +2). Four storage unit tests (cutoff math, idempotence, both helpers, malformed-key resilience) plus three integration tests on the live router. See [crates/pms-storage/src/rocks_store/retention.rs](crates/pms-storage/src/rocks_store/retention.rs), [crates/pms-server/src/admin.rs](crates/pms-server/src/admin.rs), [crates/pms-server/src/api/tasks.rs](crates/pms-server/src/api/tasks.rs).

### Fixed (post-merge)
- **fix(p2p/broadcast-asymmetry)**: Two production-path bugs in the gossip protocol surfaced by the multi-engine cluster sandbox: `peer.rs::handle Inv` and `blocks.rs::orphan-recovery` both used `Server::broadcast()` to send `GetBlock`/`GetBlocks` *back to the peer that just announced something*. But `broadcast()` only fans out to INBOUND peers (see `broadcast.rs:81-87`) — when the local node was the initiator of the connection (e.g. a follower that dialed the coordinator), the announcer sat in the OUTBOUND table and the request silently went nowhere. Result: in any deployment where the writer doesn't dial back the readers, block propagation freezes. Both call sites now use `unicast(&sa, ...)` — the peer that announced the block is exactly the peer that has the data, so unicast is both more efficient (single send) and direction-independent. Adjacent `unicast` already exists at `blocks.rs:104` for the metadata-driven parent fetch path; this brings the orphan-recovery and Inv-response paths into line with the same pattern. Validation: the multi-engine cluster sandbox now passes with **one-way dial only** (follower → coord) for n=1,2,3,4 — coordinator sees exactly `n-1` inbound peers, every follower's block_count_estimate reaches 2 (genesis + faucet) within seconds. Full `dag_sandbox` suite stays 8/8 green including the 5-min stress test (100% success, 4.08M blocks). `dynamic_connection`, `healthz_enriched`, `metrics_exposure`, `chaos_recovery`, `persist_no_silent_drops`, `mint_security`, `mint_policy_and_fees` all green — no regression on the persist or admin paths. Removes the bidirectional-dial workaround from `boot_sandbox_cluster`. See [crates/pms-server/src/server/peer.rs](crates/pms-server/src/server/peer.rs), [crates/pms-server/src/server/blocks.rs](crates/pms-server/src/server/blocks.rs).

### Added (post-merge)
- **test(sandbox/cluster)**: New `boot_sandbox_cluster(n)` helper + `test_multi_engine_cluster_propagation` test (`crates/pms-server/tests/dag_sandbox.rs`) — boots 1 to 4 engines in-process to simulate the multi-VPS topology before the user provisions a 2nd machine. Each engine gets its own tempdir + RocksDB + HTTP/P2P ports + node_wallet (admin for the coordinator, distinct seeds for followers); they all share the same global config so `coordinator_public_key` matches everywhere. The test loops `n ∈ {1, 2, 3, 4}`, faucet-mints on the coordinator, asserts each follower's `block_count_estimate` reaches 2 (genesis + faucet) within a few seconds. Surfaced a real PMS protocol limitation along the way: `Server::broadcast()` only fans out to INBOUND peers, so a follower's `GetBlock` reply (sent via `broadcast()` in `peer.rs::handle Inv`) doesn't reach the coordinator if the coord is the follower's outbound. Workaround in the cluster helper: dial in BOTH directions so each pair sits in the other's inbound table — two TCP connections per follower, fine for an in-process test, but worth fixing properly in `peer.rs` later (use `unicast(sa, ...)` instead of `broadcast()` in the Inv handler). Run with `cargo test --release -p pms-server --test dag_sandbox test_multi_engine_cluster_propagation -- --ignored --nocapture`. 8/8 sandbox tests still green including the 5-min stress test (100% success rate, 3.88M blocks, 12925 blk/s).

### Validation
- **Post-merge validation (2026-04-26)**: After the sprint merged into `main` (commit `1b38b6d`), ran the project's reference end-to-end suites against the full v0.7.4 surface to confirm no regression. `dag_sandbox` (production-like in-process engine: fee distribution PMS+EDN, smart contracts, supply/balance with asset filtering, multi-ledger, gas pool, contracts) — **7/7 tests green**, including a 5-minute sustained-TPS stress test that pushed 4.05M blocks through 80 parallel workers with **100.0% success rate** (zero block failures across all the new validator gating in `persist.rs` 1.e and the patched mint security in 1.x). `local_bench` TPS benchmark (10 workers × 1000 tx) — **10000/10000 successful, 0 failed, 5189 TPS client-side, 1.93s total**, well within the project's historical 10K+ TPS envelope. Confirms the item-4 sampler instrumentation and the item-8 active-key-set lookup don't introduce a measurable hot-path regression. The `compliance_lock` field missing on `local_bench.rs::AppState` (pre-existing breakage from sprint-prior commit `45ecad1`) was patched as part of this validation. Sprint formally closed; `hardening/production-sprint` ready for cleanup.

### Added
- **feat(coordinator-key/rotate)**: Rotation in-band of the secp256k1 key that signs every coordinator block (audit item 8 of the production-sprint plan). Pre-0.7.4 the only way to change the key was to restart the network with a new bootstrap value — every block signed by the new key would be rejected by peers still on the old config. New `PlainPayload::CoordinatorKeyRotate { old_pk, new_pk, grace_window_seconds }` lets the operator hand authority over with a single signed block: validation in `do_persist_block_internal` accepts it only when signed by the **current** coordinator pk and `old_pk` matches that current pk (so a stolen-but-grace-window key can't keep authority by chaining a fresh rotation), then writes the rotation to a new RocksDB CF `coordinator_key_history` and refreshes the in-RAM cache. The single-writer signer check on every subsequent block consults the cache: signer is accepted if it's the new `current_pk` OR any rotation's `old_pk` whose grace window hasn't yet expired (overlap of consecutive grace windows is intentional — it widens tolerance during back-to-back rotations). Mint authority is narrower: it tracks `current_pk` only, so the rotation atomically transfers the right to mint to `new_pk` while old keys in grace can sign chain blocks but not new tokens. New trait `CoordinatorKeyStorage` (in `pms-storage`) with default empty impls so mock backends opt in transparently. New CLI `tools-cli rotate-coordinator-key --old-key <path> --new-key <path> --parent <block_id> [--grace 60] [--out file.json]` forges + signs the rotation block offline and prints the wire JSON; the operator submits it via the existing `POST /v1/submit/block` endpoint. Schema `CURRENT_VER` 9→10 (new CF). Smoke-tested end-to-end: two random keys, valid signed rotation block emitted with correct payload + signature. Unit tests cover the storage layer (round-trip, idempotence on the same block ID, grace boundary), and the in-memory `KeyRotationState` (empty fallback to bootstrap, current pk follows latest rotation, atomic-revocation grace=0). See [crates/pms-types-payload/src/payload.rs](crates/pms-types-payload/src/payload.rs), [crates/pms-storage/src/coordinator_key_store.rs](crates/pms-storage/src/coordinator_key_store.rs), [crates/pms-storage/src/rocks_store/coordinator_key_storage.rs](crates/pms-storage/src/rocks_store/coordinator_key_storage.rs), [crates/pms-core/src/core_adapter.rs](crates/pms-core/src/core_adapter.rs), [crates/pms-core/src/net_adapter/persist.rs](crates/pms-core/src/net_adapter/persist.rs), [crates/tools-cli/src/main.rs](crates/tools-cli/src/main.rs).
- **test(chaos)**: New `crates/pms-server/tests/chaos_recovery.rs` — four `#[ignore]` disaster-recovery tests proving the persist pipeline holds its durability/atomicity contracts (audit item 7 of the production-sprint plan). `s1_durability_after_unclean_drop` writes 32 blocks via the single-block `append_block_atomic` path, drops the store, reopens, asserts every block is back. `s4_batch_atomicity_across_reopen` does the same for `append_blocks_batch` (64 blocks in one `WriteBatch`) — the contract is "all or nothing" across an unclean restart, this enforces it. `s3_corrupted_sst_fails_loud_or_recovers` writes 200 blocks, compacts to materialise SSTs, overwrites 64 bytes mid-file in the latest one, reopens — RocksDB must either recover gracefully or refuse to open with a diagnostic message, never silently accept corrupted data. `s5_pipeline_failure_surfaces_to_caller` spawns the background persist task against a storage that always fails, asserts the task drains its retry budget and shuts down, and that subsequent `tx.send().await` calls observe the closed channel — that's what `do_persist_block` translates into an HTTP error instead of a fake `Inserted`. Plan scenario S2 (disk-full) is documented as already covered by the existing `pms-core/tests/persist_no_silent_drops.rs`. New helper script `scripts/run-chaos-tests.sh` runs the suite in release mode with `--ignored --test-threads=1`.
- **feat(rocksdb/tips-rebuild)**: Curative reconciliation of the RocksDB `tips` CF (audit finding H3, item 6 of the production-sprint plan). The 0.7.3 fix to `trim_tips` only DELETES — it evicts zombies (entries with `children_count > 0`) but never adds anything. So a tip that's missing because of an older crash between `append_block_atomic` and `add_tip` stayed missing forever, leaving `top_tips()` to return the wrong set and silently blocking fee distribution. New helper `RocksStore::rebuild_tips_from_children_count(scan_limit)` walks the recent window via `by_time` (newest first), and adds to `tips` any block whose `children_count == 0` is missing. Idempotent: a second call is a no-op. Bounded scan: production callers pass `tip_limit × 8` (or whatever they trust). New admin endpoint `POST /admin/rebuild-tips` exposes the helper for ops intervention; defaults `scan_limit` to `tip_limit × 8` floored at 64 so a brand-new node still scans enough recent blocks to matter, body `{"scan_limit": 0}` opts into a full scan. Wired into `admin_auth_enforcement.rs` (now 37 routes covered). Four unit tests cover happy recovery, blocks-with-children skip, idempotence, and scan-limit honoring. See [crates/pms-storage/src/rocks_store/maintenance.rs](crates/pms-storage/src/rocks_store/maintenance.rs), [crates/pms-server/src/admin.rs](crates/pms-server/src/admin.rs).
- **docs(trust-model)**: New `documentation/trust-model.md` — answers in plain language "à qui dois-tu faire confiance, et pour quoi exactement ?" before launch (audit item 5 of the production-sprint plan). Three audiences: operators (SPOF, RTO/RPO, recovery procedures for VPS loss vs. coordinator-key loss), end users (a CGV-ready paragraph that names the trust assumption explicitly), and regulators (the can/cannot list, side-by-side comparison with Bitcoin and Ethereum). Cross-links wired to existing fiches via Obsidian wikilinks: [[server-engine]], [[features/wallet-encryption]], [[features/block-payloads]], [[features/storage-rocksdb]], [[features/compliance]], [[features/multi-ledger]]. README gets a "Modèle de Confiance" section linking the doc — readers see it before the API reference. MOC.md gets a new "Gouvernance & Sécurité" group so the trust model surfaces at the top of the index instead of being lost among 30+ feature fiches.
- **feat(metrics)**: Operator-facing Prometheus surface gained ten new signals so on-call has something to alert on instead of inspecting logs after the fact (audit item 4 of the production-sprint plan). Three counters declared in `pms-core` so the persist pipeline can drive them from the consumer/producer hot paths without a circular dep on `pms-server`: `pms_persist_retries_total` (every retry attempt in `background_persist`), `pms_persist_failures_total` (terminal failures after exhausting retries — pages immediately), `pms_persist_stall_seconds_total` (cumulative seconds back-pressured on `persist_tx.send().await`, fed by the existing 1-second warn ticker). Six gauges/counters declared in `pms-server` and driven by a new `spawn_metrics_sampler_task` that ticks every 5s: `pms_persist_queue_depth` and `pms_persist_queue_capacity` (per ledger, derived from `mpsc::Sender::{capacity, max_capacity}`), `pms_fee_pool_total` (per ledger, snapshot of `FeePool::total_fees`), `pms_utxo_set_size` (per ledger, from new `NetDagAdapter::utxo_set_size` accessor that hits `ShardedUtxoSet::total_len`), `pms_rocksdb_write_stalled_seconds_total` (sums sampler interval whenever the new `DagStorage::is_write_stopped` reads the canonical `rocksdb.is-write-stopped` property as `true`). Plus the per-output counter `pms_fees_distributed_total{ledger_id, recipient_type ∈ {burn_refund,treasury,node}}` wired into `perform_fee_distribution`. Smoke test `metrics_exposure.rs` asserts every name shows up in `metrics::render()` so a future rename / drop is caught at CI time. See [crates/pms-core/src/metrics.rs](crates/pms-core/src/metrics.rs), [crates/pms-server/src/metrics.rs](crates/pms-server/src/metrics.rs), [crates/pms-server/src/api/tasks.rs](crates/pms-server/src/api/tasks.rs).
- **feat(healthz)**: `/healthz` is no longer the boot-time `_ready` flag (audit finding H-healthz). It now runs four real probes and returns a structured JSON body — `rocksdb_writable` (cheap O(1) DB property), `persist_queue_depth` (current depth vs max capacity, configurable high-water threshold), `last_block_age` (timestamp of the newest persisted block via the new `DagStorage::block_ts_ms` accessor), `disk_free_percent` (libc `statvfs` on the RocksDB volume). HTTP code is `200` when every check passes, `503` with a per-check breakdown when one trips. `/livez` stays trivial (the Kubernetes liveness contract — "is the process answering at all"), so a transient persist-queue spike never restarts the pod. New `[health]` config block exposes `max_last_block_age_seconds`, `persist_queue_high_water`, `min_disk_free_percent` (sane defaults shipped). New trait method `NetDagAdapter::persist_queue_depth` returns `Some((used, capacity))` from the live tokio mpsc; mock adapters keep the default `None`. Three integration tests (`healthz_enriched.rs`) cover JSON shape, HTTP code ↔ aggregate consistency, and `/livez` triviality. See [crates/pms-server/src/api_fn/healthz.rs](crates/pms-server/src/api_fn/healthz.rs).

### Security
- **sec(coordinator-key/at-rest)**: The coordinator's ECDSA private key used to sit on disk as plain 64-hex (`/opt/pms/etc/pms/node.key`). A stolen offsite backup, an inadvertent log of `cat node.key`, or any process that read the file gave the attacker the key directly. v0.7.4 introduces an optional encrypted envelope `node.key.enc`: AES-256-GCM ciphertext keyed by an Argon2id-derived AES-256 key, salt and nonce per file, JSON wire shape so the format can be inspected and versioned. Plaintext is the raw 32 key bytes; AAD is `pms/coordinator-key/v1` to bind the ciphertext to its purpose. New module [`pms-wallet::key_encryption`](crates/pms-wallet/src/key_encryption.rs) implements `encrypt_key` / `decrypt_key` with passphrase zeroized at the boundary. New CLI `tools-cli encrypt-coordinator-key <plain.key> <out.enc>` produces the envelope (passphrase from env var `PMS_COORDINATOR_KEY_PASSPHRASE` or interactive prompt with double-confirm). The server (`bin/main.rs`) now prefers `[secrets].node_identity_key_encrypted_path` when configured AND the file exists, and reads the passphrase from `PMS_COORDINATOR_KEY_PASSPHRASE`; falls back transparently to the legacy plain-hex path so existing dev/testnet deployments boot unchanged. New helper `Wallet::check_key_file_permissions` warns at boot when the key file is group/world-readable, and aborts boot when `[secrets].strict_key_permissions = true`. 9 tests cover envelope round-trip, wrong passphrase, tampered ciphertext, unsupported version, JSON shape, end-to-end disk → `Wallet`, and mismatched-passphrase rejection. Audit finding H-key.

### Security
- **sec(admin-auth)**: Audit finding H-auth closed on five fronts. (A) The two admin middlewares (`require_local_or_admin`, `require_admin_token`) used to compare the admin token with `auth_str == format!("Bearer {token}")` — a direct string compare that can leak via timing. Both now delegate to `helper::is_admin_authorized`, which already runs `subtle::ConstantTimeEq` on the payload. (D) `api_fn::compliance::is_admin_authorized` was a divergent copy (wrong default "allow all on missing token", only checked `Authorization` not `X-Admin-Token`); removed and `use crate::helper::is_admin_authorized` wired in. (E) The global `CorsLayer` shifted from `allow_methods(Any)` / `allow_headers(Any)` to an explicit small set (`GET/POST/PUT/DELETE/OPTIONS`, `Authorization`/`Content-Type`/`Accept`/`X-Api-Key`/`X-Admin-Token`), and the `/admin/*` sub-router intentionally carries no CORS layer of its own — a browser refusing the preflight is an extra defense-in-depth barrier against CSRF targeting an operator with a stored admin token. (F) New `pms_admin_auth_failures_total{reason}` counter lets operators alert on brute-force / token-leak bursts (reasons: `ip_not_allowed`, `missing_token`, `wrong_token`). (C) New integration test `admin_auth_enforcement.rs` fires every known `/admin/*` route (36 at time of writing) from a non-loopback IP with no Authorization header, asserts 401/403 across the board — a regression trap if a future router refactor ever accidentally unplugs the middleware.
- **bump(version)**: Workspace version 0.7.3 → 0.7.4.

---

## [0.7.3] - Unreleased — Persist hot-path clone reduction + atomic encrypted UTXO delta + tips reconcile

### Fixed
- **fix(rocksdb/tips-consistency)**: `trim_tips` used to sort every entry in the `tips` CF by timestamp and evict the oldest over `tip_limit` — which let "zombie" tips (entries that had already gained a child, but whose `remove_tip` call was lost to a crash or a bug) survive because of their recent timestamp, while an actually-active older tip got evicted instead (audit finding H3). The CF would then drift out of sync with `ConcurrentDag::tips`, the real source of truth. Now `trim_tips` cross-checks every candidate against the `children_count` CF: any entry with `children_count > 0` is a zombie and is reclaimed first, then `tip_limit` enforcement runs over the *real* tips only. The fast-path (`estimate <= tip_limit`) is kept for steady-state performance — zombies simply clear on the next trim that actually runs. Three new unit tests lock the semantics. See [crates/pms-storage/src/rocks_store/maintenance.rs](crates/pms-storage/src/rocks_store/maintenance.rs).

### Security
- **sec(encrypted-tx/race)**: The pre-0.7.3 encrypted-TxUtxo flow was a two-step dance on the caller side (`persist_and_broadcast(wb)` then `apply_utxo_delta(inputs, outputs)`), leaving a window where the block was already visible in the RAM DAG and the persist pipeline while the `ShardedUtxoSet` still listed the inputs as spendable — a concurrent handler could re-select the same UTXOs and build a double-spending transaction (audit finding H1). Introduced `NetDagAdapter::persist_block_with_delta(wb, delta)` with a default non-atomic fallback; `CoreAdapter` overrides it so the externally-provided `UtxoDelta` is applied in the same critical section as the block insert (same step as plain payloads). New `tx_helpers::persist_and_broadcast_with_delta` wraps the common bookkeeping. Migrated both encrypted callers (`transaction::prepare_tx` and `wallet_factory::send_simple`). Sender lookup (`get_utxo(first_input)`) is now done BEFORE the persist call since inputs are consumed atomically on the return path. See [crates/pms-interface/src/net_adapter.rs](crates/pms-interface/src/net_adapter.rs), [crates/pms-core/src/net_adapter/persist.rs](crates/pms-core/src/net_adapter/persist.rs), [crates/pms-server/src/api_fn/tx_helpers/block_ops.rs](crates/pms-server/src/api_fn/tx_helpers/block_ops.rs).

### Performance
- **perf(persist)**: Extracted `block_id` once in `do_persist_block` and moved both the `Block` (into `ConcurrentDag::insert_block`) and the `StoredBlock` (into `PersistJob`) by value instead of cloning them (audit finding H6). The `StoredBlock` clone was the most expensive — it carries `payload_json`, which can be 10-100 KB on encrypted blocks (`LedgerOwnershipTransfer`, wrapped DEK payloads). The reused `block_id` also folds 3 separate `block.id.clone()` calls in the finality path into a single pre-extracted `String`. Net effect at 10 K TPS: ~20 K avoided clones/s of the full `StoredBlock` and `Block` structs, several MB/s less memory pressure on the persist pipeline, no behavioural change. All 150+ branch tests still green. See [crates/pms-core/src/net_adapter/persist.rs](crates/pms-core/src/net_adapter/persist.rs).

### Changed
- **bump(version)**: Workspace version 0.7.2 → 0.7.3.

---

## [0.7.2] - 2026-04-22 — Security audit sprint (C2, C3, M1, H5, M2, M3+M4, M5+H7, H4+M6)

### Fixed
- **fix(persist/critical)**: `do_persist_block` previously wrapped `persist_tx.send()` in a 5-second `tokio::time::timeout` and returned `PutResult::Inserted` to the caller even when the send timed out or the channel was closed — a silent data-loss bug. In a saturated persist pipeline (RocksDB stall, compaction pressure), the block was in RAM but never queued for disk, and the HTTP client saw a false "Inserted" acknowledgement. Replaced with an unbounded `send().await` that blocks until the queue has room (natural end-to-end back-pressure), emits periodic `tracing::error!` warnings every second while blocked, and returns `Err(anyhow!)` on a closed channel instead of fake success. See [crates/pms-core/src/net_adapter/persist.rs](crates/pms-core/src/net_adapter/persist.rs).
- **fix(persist/critical)**: `background_persist_task` used to drop an entire batch on a single `append_blocks_batch` failure with only a `tracing::error!`. The task now retries with exponential backoff (100ms → 500ms → 2s → 5s → 15s → 30s) and, if all retries fail, shuts down the background loop so the channel closes and subsequent `send().await` calls return an error to the HTTP caller — no more false acknowledgements for blocks that never reached RocksDB. See [crates/pms-core/src/background_persist.rs](crates/pms-core/src/background_persist.rs).
- **fix(spent-tracking/preventive)**: `ConcurrentDag::is_spent()` only inspects the bounded FIFO RAM tracker, so any caller that trusted it alone would miss outpoints evicted once `max_spent_outpoints` was reached (audit finding C3). The method is now documented as best-effort RAM and a new authoritative helper `ConcurrentDag::is_outpoint_spent_authoritative(&store, txid, index)` consults RAM first and then falls back to the new `DagStorage::is_outpoint_spent` trait method. `RocksStore` implements the fallback by reading the on-disk `utxo_spent` column family — the single source of truth for double-spend detection across restarts and eviction. Today's production flow (`ShardedUtxoSet::get` with RocksDB fallback) was already safe; this change keeps any future caller from re-introducing the FIFO hole. See [crates/pms-core/src/concurrent_dag/spent.rs](crates/pms-core/src/concurrent_dag/spent.rs) and [crates/pms-storage/src/traits.rs](crates/pms-storage/src/traits.rs).
- **fix(wallet/security)**: `Wallet` used to `#[derive(Debug)]`, which meant a stray `dbg!(wallet)` or `println!("{wallet:?}")` would print the ECDSA private key and the full BIP-39 mnemonic (audit finding M1). `Debug` is now implemented manually and redacts both `private_key_b64` and `mnemonic_words` with length-only hints; public material (`public_key_hex`, `x25519_pub_hex`) is kept for debugging. New integration test `debug_redaction.rs` locks the invariant in. See [crates/pms-wallet/src/wallet.rs](crates/pms-wallet/src/wallet.rs).
- **fix(bridge/security)**: `admin_bridge_transfer` parsed `cross_ledger_fee_multiplier: f64` with `Decimal::from_f64_retain(...).unwrap_or(Decimal::from(2))`, so any NaN/±Inf/huge config value silently fell back to a hardcoded `2×` — users could be charged a mystery fee instead of the operator's intended multiplier (audit finding H5). Validation is now routed through a pure helper `validate_cross_ledger_multiplier(f64)` that explicitly rejects NaN, ±Inf, negative values, and values that overflow `Decimal`; rejected configs skip the fee with a `tracing::error!` so operators see their config is wrong. Multiplication uses `checked_mul` to avoid overflow surprises. Eight unit tests cover every edge case. See [crates/pms-server/src/api_fn/bridge.rs](crates/pms-server/src/api_fn/bridge.rs).

### Changed
- **bump(version)**: Workspace version 0.7.1 → 0.7.2.
- **deps(rand)**: Unified all workspace crates on `rand = "0.9.2"` (audit finding M2). Previously `pms-wallet` used 0.8.5, `pms-config` 0.8, and `pms-ledger` was pinned to the pre-release `0.10.0-rc.5` which dragged in `chacha20 = "0.10.0-rc.5"` (an unfinished AEAD crate) as a transitive dependency. Both RC packages are now out of the dependency graph. `pms-wallet/helpers.rs` migrated to the 0.9 API (`rand::distr::weighted::WeightedIndex`, `rand::rng()`). `pms-config` tests pin `rand_core = "0.6"` explicitly for the `CryptoRngCore` trait required by `k256 0.13` (transitive `rand_core 0.6.4` via `ecdsa 0.16.9` is independent of the `rand` version we use). The only remaining non-0.9 `rand` in `Cargo.lock` is `0.8.5` pulled by `nanoid 0.4.0`, used for non-cryptographic ID generation only.

### Security
- **sec(encrypted-payload/preventive)**: `EncryptedPayload`'s AES-GCM body AAD used to bind only `len_hint`, so an attacker holding the raw envelope outside a signed block context could rewrite the `recipients` list, flip a `kid`, or swap `ephem_pub` without invalidating the body tag (audit finding M4). `KEY_VERSION_CURRENT` bumped `1 → 2`; a new `aad.binding` field hashes `scheme`, `key_version`, `ephem_pub`, and the sorted recipient kids, then feeds that hash into the body AAD. Decryption recomputes the binding from the wire envelope and rejects mismatches before even touching the ciphertext. v1 envelopes (`aad.binding = None`, byte-identical wire format via `skip_serializing_if`) still decrypt cleanly so existing testnet blocks stay readable. Audit finding M3 (nonce "reuse" across recipients) was re-analysed and deemed a false positive — a single `(DEK, nonce)` pair encrypts the body once and the shared ciphertext is standard multi-recipient design, not nonce reuse. Seven new tests cover round-trip, backward-compat, and four negative paths (binding tampering, ephem_pub swap, recipient removal, kid substitution). See [crates/pms-types-payload/src/encrypted_payload.rs](crates/pms-types-payload/src/encrypted_payload.rs).

### Performance
- **perf(locks)**: Migrated every `std::sync::Mutex` / `std::sync::RwLock` on a hot path to `parking_lot` equivalents (audit findings M5, H7). Faster acquisition (smaller atomic footprint), no poisoning semantics, smaller lock objects (1 byte vs 16+). Affects `ConcurrentDag::{finality, insertion_order, spent_order}`, `ShardedUtxoSet::{supply_cache, Interner::set}`, `RocksStore::{top_tips_cache, runtime_config_cache}`, `Server::seen_invs`, `TpsTracker::timestamps`. Eliminated every `match lock() { Err(poisoned) => poisoned.into_inner() }` workaround (≈30 lines of defensive code). All 80+ tests on the branch stay green, including `pms-economics` (33 tests on `TpsTracker`).

### Security
- **sec(compliance/race)**: `admin_freeze` / `admin_unfreeze` used to run `is_frozen(addr)` and then forge+persist the Freeze/Unfreeze block without any locking. Two admin requests for the same address could both observe `is_frozen == false`, both proceed, and both end up signed & persisted — producing two distinct audit trails for a single real state transition (audit finding M6). Added `AppState::compliance_lock: Arc<tokio::sync::Mutex<()>>`, acquired at the top of both handlers so the check-then-persist sequence is atomic across concurrent admin requests. Contention is negligible (freezes are rare). New `compliance_lock_tests::concurrent_freeze_sections_are_serialised` asserts that 16 racing "requests" never observe more than one critical section active at a time. See [crates/pms-server/src/api_fn/compliance.rs](crates/pms-server/src/api_fn/compliance.rs).

### Infrastructure
- **ops(fees/treasury)**: Misconfigured treasury fees (`treasury_fee_percent > 0` with no `treasury_addresses` configured anywhere) used to redirect the cut to the node pool with a single quiet `warn!` (audit finding H4). New `fee_distribution::validate_treasury_config` is called at boot and emits a loud `tracing::error!` surfacing exactly what to fix; the runtime fallback now also logs at `error!` instead of `warn!` so the misconfig can't hide in log noise. Funds are never lost — the cut is still safely retained in the node pool if the error slips through — but operators will see the problem immediately. Five new unit tests lock every branch of the validator. See [crates/pms-server/src/fee_distribution/mod.rs](crates/pms-server/src/fee_distribution/mod.rs) and [crates/pms-server/src/fee_distribution/distribute.rs](crates/pms-server/src/fee_distribution/distribute.rs).

---

## [0.7.1] - 2026-03-24 — Fix: OOM crash loop + software_version endpoint

### Fixed
- **fix(rocksdb/critical)**: Engine OOM crash loop on testnet VPS (12 restarts in 24h, exit code 137). Root cause: with 2 ledgers (67 column families) and 20M blocks in DB, RocksDB compaction memory spikes exceeded the 14 GiB Docker limit. Aggressive tuning: `write_buffer_size_mb` 32→16, `max_write_buffer_number` 3→2, `block_cache_size_mb` 512→256, `db_write_buffer_size_mb` 512→256, `max_dag_blocks` 50K→10K. New memtable budget: 2.1 GiB (67 × 2 × 16 MB). Memory follows saw-tooth pattern: trough ~6-7 GiB, peak ~12.4 GiB during compaction. Stable at ~20 tx/s + game activity.
- **fix(persist/critical)**: Background persist channel used `try_send()` which **silently dropped blocks** when the 10K buffer was full — a data loss bug in a financial system. Replaced with `send().await` + 5-second timeout: API callers now block (natural back-pressure) instead of losing data. Timeout prevents indefinite blocking if RocksDB stalls. Both channel-closed and timeout scenarios log errors.
- **fix(api)**: `GET /v1/version` returned `software_version: "0.1.0"` instead of the actual binary version. Root cause: `env!("CARGO_PKG_VERSION")` in `pms-server` crate read that crate's own Cargo.toml version (0.1.0), not the binary version. Fix: introduced `[workspace.package] version` in root Cargo.toml, inherited by `bin` and `pms-server` via `version.workspace = true`. Now all report the correct version.

### Performance
- **perf(persist)**: Reduced background persist channel buffer from 10K to 2K blocks. With back-pressure enabled, the large buffer only consumed RAM without benefit. 2K blocks × ~1 KB = ~2 MB vs ~10 MB, with 31 batches of headroom at MAX_BATCH_SIZE=64.

### Changed
- **refactor(versioning)**: Software version now defined once in `Cargo.toml` workspace (`[workspace.package] version = "0.7.1"`), inherited by `bin` and `pms-server`. Bumping version requires changing only the workspace root.
- **bump(version)**: Software version 0.7.0 → 0.7.1.

### Infrastructure
- **ops(testnet)**: Updated `config.testnet.toml` with aggressive memory tuning for 16 GB VPS: `max_write_buffer_number` 3→2, `block_cache_size_mb` 512→256, `db_write_buffer_size_mb` 512→256, `max_dag_blocks` 50K→10K, `max_utxos` 2M→500K. Memory sizing table updated in CLAUDE.md.
- **ops(testnet)**: Reduced simulator `agents_testnet.toml` from ~1,800 tx/s (original) to ~20 tx/s (30 agents). 16 GB VPS can sustain ~20 PMS tx/s + game activity with saw-tooth compaction pattern staying under 14 GiB limit.

---

## [0.7.0] - 2026-03-24 — Cube Obfuscation, Rarity System & Calibrated Economics

_See git log for full details (commit bf33f90)._

---

## [0.6.8] - 2026-03-23 — Smart Contract Simulation & Sandbox Mode

### Added
- **feat(contracts)**: `POST /admin/contracts/simulate` dry-run endpoint. Accepts a candidate contract + a simulated event (`NftBurn`, `Transfer`, `TokenBurn`), evaluates the contract against an ephemeral in-memory store (existing contracts + candidate), and returns `SimulationResult` with `matched`, `match_reason`, `burn_results`, `transfer_fee_results`, `warnings`, and `existing_contract_matches`. No state is persisted — pure dry-run. Protected by `require_local_or_admin`.
- **feat(contracts)**: Sandbox mode — contracts now default to `enabled: false` on registration. The `RegisterContractRequest` accepts an optional `enabled` field (`#[serde(default)]`). Pass `"enabled": true` to activate immediately, or use `POST /admin/contracts/{id}/toggle` to activate later. Backward-compatible: existing clients passing no `enabled` field get `false`.
- **feat(contracts)**: New simulation types in `pms-contracts`: `SimulationEvent` (enum: `NftBurn`, `Transfer`, `TokenBurn`), `SimulationResult`, `ExistingContractMatch`. All derive `Serialize`/`Deserialize` for JSON API.
- **feat(contracts)**: `simulate_contract()` function in `pms-contracts::engine` — builds ephemeral `InMemoryContractStore`, force-enables candidate, evaluates against event, filters results, detects existing contract matches.
- **test(contracts)**: 8 new unit tests for simulation engine: transfer fee, NFT burn, existing contracts detection, scope mismatch, trigger mismatch, token burn warning, attribute formula, disabled candidate force-evaluation.
- **test(contracts)**: Integration test `test_contract_simulate_endpoint` — full lifecycle: simulate → verify no persistence → register (enabled=false) → toggle → verify enabled.
- **test(contracts)**: Integration test `test_contract_registration_concurrent` — 10 concurrent registrations + 5 concurrent reads, verifies all 10 succeed with `enabled: false`.

### Changed
- **breaking(contracts)**: Contracts now default to `enabled: false` on registration (was `true`). Existing API consumers must pass `"enabled": true` in the registration body to activate immediately.
- **bump(api)**: `API_VERSION` 8 → 9 (new simulate endpoint + sandbox mode default change).
- **bump(version)**: Software version 0.6.7 → 0.6.8.

---

## [0.6.7] - 2026-03-23 — Performance: O(1) token balance, activity backfill, UTXO consolidation

### Performance
- **perf(utxo/critical)**: Token balance queries are now **O(1)** instead of O(k) shard scan. Added `token_balance_cache: DashMap<(Arc<str>, Arc<str>), Decimal>` to `ShardedUtxoSet`, mirroring the existing `native_balance_cache` pattern but keyed by `(address, asset_id)`. Maintained incrementally in `supply_add_compact()` / `supply_sub_compact()` with zero-balance cleanup. Eliminates shard locks + RocksDB fallback for token balance lookups (EDN, custom assets).
- **perf(config)**: Increased `max_utxos` default from 500K to 2M (~64 MB RAM). 500K was too small for production DAGs with millions of UTXOs, causing excessive LRU eviction + RocksDB fallback. 2M covers most production deployments.
- **perf(config)**: Increased `block_cache_size_mb` default from 512 to 1024. With Direct I/O (v0.5.21), RocksDB block cache is the ONLY read cache — 1 GB is the production minimum.

### Added
- **feat(activity)**: Auto activity items backfill on startup. New config flag `rocks.auto_reindex_activity_items` (default: `true`). On startup, waits 30s then calls `reindex_all_activity_items()` in a background `spawn_blocking` task. Ensures 100% of blocks have pre-computed activity items for fast-path queries (1-2ms instead of 10-50ms fallback). Idempotent, safe to run on every restart.
- **feat(admin)**: UTXO consolidation endpoint `POST /admin/consolidate-utxos`. Merges multiple coordinator UTXOs into a single UTXO via self-transfer. Accepts `asset_id` (optional) and `max_inputs` (2-256, default 64). Returns `{ block_id, consolidated_inputs, new_utxo_amount, fee }`. Protected by `require_local_or_admin`. Solves coordinator UTXO proliferation from fee distribution (7200+ UTXOs/hour at 2000 TPS).
- **fix(consolidation)**: Fee output in consolidation endpoint now uses the same `asset_id` as the consolidated inputs. Without this, consolidating custom tokens (EDN) would produce a fee output in PMS, violating UTXO conservation (sum inputs != sum outputs).

---

## [0.6.6] - 2026-03-23 — Fix: Simulator Eden ledger owner keys missing

### Fixed
- **fix(simulator/critical)**: Simulator created Eden ledger without valid `owner_pubkey`/`owner_x25519_pubkey`, preventing fee distribution to creator. Root cause: `GameEngine::setup()` passed `coordinator_address` (bech32m address string) instead of the actual public keys (hex). Solution:
  1. Added `derive_public_keys_from_privkey_hex()` to derive ECDSA k256 + X25519 public keys from the coordinator's secp256k1 private key (same logic as `pms-wallet`).
  2. Modified `GameEngine::setup()` to accept `coordinator_private_key_hex` instead of `coordinator_address`, derive both keys, and pass them as `owner_pubkey`/`owner_x25519_pubkey` when creating the Eden ledger.
  3. Added `derive_address_from_keys()` helper to compute bech32m address for the transfer fee contract.
- **dependencies(simulator)**: Added crypto deps for key derivation: `k256`, `bech32`, `sha2`, `hkdf`, `x25519-dalek` (with `static_secrets` feature).

---

## [0.6.5] - 2026-03-23 — Fix: Custom ledger fees not distributed to owner

### Fixed
- **fix(fees/critical)**: Custom ledger owners (e.g., Eden creator) were not receiving accumulated transaction fees. Root cause: `accumulate_tx_fee()` always credited fees to the coordinator's `node_pk`, but for custom ledgers, fees should be credited to the ledger owner instead. The NodeRegistry was empty, so fees fell back to Treasury. **Solutions**:
  1. Modified `accumulate_tx_fee()` to detect custom ledgers (`ledger_id != "main"`) and credit fees to `ledger.owner_pubkey` instead of `coordinator.node_pk`.
  2. Added auto-registration of custom ledger owners in the NodeRegistry at startup. The owner's wallet address is derived from their Ed25519 + X25519 public keys (same bech32m logic as `Wallet::get_address()`). This enables the periodic fee distribution task to map `owner_pubkey` → `wallet_address` and distribute accumulated fees correctly.
- **fix(fees)**: Added `bech32 = "0.8.1"` dependency to `pms-server` for deriving wallet addresses from public keys in `derive_address_from_keys()` helper.

---

## [0.6.4] - 2026-03-23 — Fix: UTXO double-count destroying supply & address index

### Fixed
- **fix(utxo/critical)**: All plain-payload handlers (`faucet_mint`, `admin_seize`, `admin_reverse`, `create_reward_block`, `perform_fee_distribution`, `perform_daily_inflation_mint`) called `apply_utxo_delta()` or `add_utxo()` AFTER `persist_block()` for plain payloads. Since `persist_block()` already constructs and applies the `UtxoDelta` via `apply_diff()` for plain payloads, the second call caused:
  1. **Supply double-counting**: `supply_add_compact()` called twice per UTXO → supply inflated 2x.
  2. **Address index destruction**: LRU `push()` on existing OutputId evicted the existing entry, then `addr_index_remove(evicted)` deleted the entry that `addr_index_add` just re-added → UTXOs invisible to address queries.
  - Affected: faucet minting, compliance (seize/reverse), fee distribution, inflation minting.
  - Symptom on VPS: simulator agents failed to refuel ("no UTXOs found for address") despite successful faucet calls.

### Infrastructure
- **infra(docker)**: Fixed `Dockerfile.testnet` for `bindgen 0.72+` which no longer uses `clang-sys/runtime` (dlopen). Build scripts now link statically against LLVM. Added `zlib-static`, `llvm18-static`, `ncurses-static` packages and `libstdc++.a` symlink.

---

## [0.6.3] - 2026-03-22 — Fix: OOM crash loop on VPS testnet

### Fixed
- **fix(memory/critical)**: Engine OOM crash loop on 16 GB VPS (RestartCount: 48+, exit code 137). Root cause: glibc ptmalloc2 per-thread arena fragmentation under high-throughput multi-threaded RocksDB workloads caused RSS to grow ~4 GiB/min at 400 TPS. Solution: **jemalloc global allocator** via `tikv-jemallocator` — returns freed pages to OS aggressively, eliminates fragmentation-induced memory bloat.
- **fix(memory)**: `address_index` (DashMap<String, DashSet<OutputId>>) in `ShardedUtxoSet` grew without bound. When LRU cache evicted UTXOs, their `address_index` entries were never cleaned up. Fixed: `add()` and `apply_diff()` now use `push()` instead of `put()` to capture evicted entries and remove them from `address_index`.
- **fix(memory)**: `native_balance_cache` (DashMap<String, Decimal>) never removed zero-balance entries. Fixed: `supply_sub_compact()` now removes entries when balance reaches zero.

### Changed
- **config(testnet)**: Reduced RocksDB memory settings for 16 GB VPS — `write_buffer_size_mb` 32→16, `block_cache_size_mb` 1024→256, `db_write_buffer_size_mb` 512→256, `max_utxos` 2M→250K.
- **config(testnet)**: Reduced simulator TPS from ~2000 to ~300 (snipers 10→3, fast traders 20→8, reduced sends_per_tick across agents).

### Infrastructure
- **infra(docker)**: Overhauled `Dockerfile.testnet` for jemalloc + RocksDB on Alpine/musl:
  - Added `clang18-dev`, `g++`, `linux-headers` build deps for RocksDB C++ compilation + bindgen.
  - Set `RUSTFLAGS="-C target-feature=-crt-static"` — dynamically links musl so build scripts can `dlopen(libclang.so)` (static musl blocks `dlopen`, breaking bindgen's runtime linking).
  - Added `libstdc++`, `libgcc` to runtime Alpine image for dynamically-linked binary.
  - Runtime container uses `user: "1000:1000"` (matching host `pms` user) instead of nonroot UID 65532.

---

## [0.6.2] - 2026-03-22 — Hardening: production-readiness quick wins

### Fixed
- **fix(storage)**: 7x `RwLock::unwrap()` in `InMemoryContractStore` replaced with poison-safe `unwrap_or_else(|p| p.into_inner())` pattern. Prevents cascading panics if a thread panics while holding the contract store lock.
- **fix(validations)**: Removed misleading `// TODO (MVP rapide: stub "Ok(())")` comment on `verify_tx_signatures()` in `check.rs`. Signature verification is fully implemented in `signature.rs` — the outdated comment was dangerous for auditors.
- **fix(tests)**: Re-enabled 2 previously `#[ignore]`'d admin wallet fee tests (`wallet_send_tx_fee_is_materialized_and_zeroed_and_visible_to_admin`, `wallet_send_tx_does_not_duplicate_fee_output_if_already_present`). Root cause: missing change output and manual UTXO persistence in test setup. All 3 wallet fee tests now pass.

### Added
- **feat(metrics)**: Added `pms_api_request_duration_seconds` Prometheus histogram with method/route labels. Buckets: 1ms to 5s. Uses Axum `MatchedPath` for route templates, preventing label cardinality explosion from dynamic URL segments.
- **feat(middleware)**: Added `track_latency` middleware in the global Axum layer stack, recording request duration for all API endpoints.

### Changed
- **config(testnet)**: Reduced `rate_limit_rps` from 50000→10000 and `burst` from 100000→20000 in `config.testnet.toml`. Still 5x the simulator's peak throughput (2000 TPS) but provides basic DoS protection on publicly exposed testnet.

---

## [0.6.1] - 2026-03-21 — Fix: fd exhaustion crash + metrics counter reset + dashboard TPS

### Performance
- **perf(dashboard)**: TPS chart was updating every ~10 seconds instead of every 1 second under high block load. Root cause: `fetchData()` bundled 6 API calls (`/metrics`, `/v1/supply`, `/v1/nodes`, `/v1/peers`, `/admin/ping`, `/v1/tokens`) in a single `Promise.all()` with 2s interval. When the engine was under load, slow endpoints (`/v1/supply`) blocked the lightweight `/metrics` call. Fix: split into two independent polling loops — fast metrics (1s, only `/metrics`) and slow data (5s, supply/nodes/peers/tokens). Added `metricsFetching` guard to prevent overlapping metrics calls. Relaxed `deltaTime < 10` guard to `< 30` so TPS data isn't discarded after brief network hiccups.

### Fixed
- **fix(server/critical)**: Engine crashed silently every ~2-2.5 hours with ExitCode 0 due to **file descriptor exhaustion**. Root cause: Docker default `ulimit -n = 1024` combined with `max_open_files = 1024` in RocksDB config, leaving zero fd headroom for TCP accept, logging, or other I/O. 113K+ "Too many open files" errors occurred before each crash. `listen_tls()` / `listen()` used `?` on `accept()`, causing a single fd error to kill the entire P2P listener and propagate through `srv.run()` to `main()`, which always returned `Ok(())` (exit code 0) regardless of error.
- **fix(server/critical)**: P2P listener `accept()` errors now use retry-with-backoff instead of `?` propagation. A transient OS error (fd exhaustion, EMFILE) no longer kills the server — the listener logs the error and retries after 1s, recovering automatically once fds are freed.
- **fix(main)**: `main()` now calls `std::process::exit(1)` when `srv.run()` returns (either Ok or Err), using `eprintln!` (unbuffered) to guarantee the error message is visible even when tracing can't write due to fd exhaustion. Previously, `main()` always returned `Ok(())` → exit code 0 → Docker reported "clean exit" → misleading diagnostics.
- **fix(metrics/critical)**: `pms_blocks_persisted_total` counter was never initialized for dynamic ledgers (e.g. eden) restored via `load_persisted_ledgers()`. After every engine restart, the dashboard showed a misleading gap (e.g. "DAG Size: 50K" vs "Blocks Persisted: 0") because the Prometheus counter started at 0 while `pms_blocks_total` loaded 50K blocks from RocksDB. Now all dynamic ledgers get their counter seeded from `block_count_estimate()` at startup.
- **fix(persist)**: `try_send()` in the background persist pipeline silently dropped blocks when the channel buffer (10K) was full. Added warning log with drop counter so operators can detect backpressure-induced data loss. Previously, blocks could be lost without any trace in logs.

### Added
- **feat(boot)**: Startup now reads `/proc/self/limits` and logs the fd limit. Warns if `ulimit -n < 8192` with instructions to set Docker ulimits.

### Infrastructure
- **infra(docker)**: `docker-compose.testnet.yml` now sets `ulimits: nofile: { soft: 65536, hard: 65536 }` for the engine container.
- **infra(config)**: `config.testnet.toml` `max_open_files` increased from 1024 to 4096, leaving ample headroom within the new 65536 fd limit.

---

## [0.6.0] - 2026-03-21 — Major structural refactoring: module splits + dead code removal

### Changed
- **refactor(storage)**: Split `rocks_store/store.rs` (2,426 lines) into 5 sub-modules: `activity_index.rs`, `dag_storage_impl.rs`, `maintenance.rs`, `secondary.rs`, and a trimmed `store.rs`. All `pub use` re-exports preserved.
- **refactor(server)**: Split `server.rs` (1,465 lines) into 6 sub-modules: `mod.rs`, `broadcast.rs`, `listener.rs`, `peer.rs`, `sync.rs`, `blocks.rs`.
- **refactor(server)**: Split `api.rs` (1,197 lines) into 7 sub-modules: `mod.rs`, `state.rs`, `middleware.rs`, `routes.rs`, `serve.rs`, `ledger_dispatch.rs`, `tasks.rs`.
- **refactor(server)**: Split `api_fn/activity.rs` (2,334 lines) into 6 sub-modules: `mod.rs`, `cache.rs`, `handler.rs`, `stream.rs`, `classify.rs`, `tests.rs`.
- **refactor(server)**: Split `fee_distribution.rs` (961 lines) into 5 sub-modules: `mod.rs`, `compute.rs`, `distribute.rs`, `inflation.rs`, `tests.rs`.
- **refactor(server)**: Split `api_fn/tx_helpers.rs` (896 lines) into 6 sub-modules: `mod.rs`, `fee_policy.rs`, `coin_selection.rs`, `block_ops.rs`, `fee_accumulation.rs`, `tests.rs`.
- **refactor(core)**: Split `concurrent_dag.rs` (1,829 lines) into 9 sub-modules: `mod.rs`, `core.rs`, `tips.rs`, `spent.rs`, `pruning.rs`, `finality.rs`, `bootstrap.rs`, `forge.rs`, `tests.rs`.
- **refactor(core)**: Split `net_adapter.rs` (1,282 lines) into 6 sub-modules: `mod.rs`, `persist.rs`, `query.rs`, `supply.rs`, `utxo.rs`, `helpers.rs`. Uses delegate-to-helper pattern (Rust constraint: single `impl Trait for Type` per file).
- **refactor(storage)**: Split `helpers.rs` (923 lines) into 5 sub-modules: `mod.rs`, `encoding.rs`, `time_index.rs`, `activity_keys.rs`, `classify.rs`, `tests.rs`.
- **refactor(config)**: Coordinator public key constants (`COORDINATOR_PUBLIC_KEY_MAINNET`, `COORDINATOR_PUBLIC_KEY_TESTNET`) moved from dead `pms-consensus` crate to `pms-config`. Added `NetworkMode::coordinator_public_key()` helper.

### Removed
- **remove(crate)**: Deleted `pms-crypto` — dead code (dummy `add()` function, empty `ed25519.rs`). Zero dependents.
- **remove(crate)**: Deleted `pms-consensus` — dead code (2 unused coordinator key constants, now in `pms-config`). Zero runtime dependents.

### Infrastructure
- **infra**: All 10 file splits preserve public API via `pub use` re-exports. Zero breaking changes.
- **infra**: 8 clippy warnings fixed (empty line after doc comment in server sub-modules).
- **infra**: Activity test imports fixed after module split (`classify::*` explicit import in tests.rs).

---

## [0.5.22] - 2026-03-21 — Memtable OOM fix: multi-ledger memory scaling

### Fixed
- **fix(storage/critical)**: RocksDB memtable OOM from multi-ledger CF explosion. With 2 ledgers (66 CFs), `write_buffer_size_mb=128 × max_write_buffer_number=6 × 66 CFs = 50 GB theoretical max` — heap reached 12 GB (confirmed via `/proc/1/smaps_rollup`) within hours, triggering OOM kills (4 restarts). Fixed by reducing `write_buffer_size_mb` default from 128 to 32 and `max_write_buffer_number` from 6 to 3. New worst-case: `66 × 3 × 32 = 6.3 GB` memtables — safe within 14 GB Docker limit.
- **fix(storage)**: Corrected `db_write_buffer_size_mb` documentation — it is a **flush trigger**, NOT a hard memory cap. Immutable memtables waiting for flush still consume RAM beyond this limit. This misunderstanding was a contributing factor to the v0.5.21 OOM.

### Changed
- **change(config/testnet)**: `write_buffer_size_mb` reduced 128 → 32, `max_write_buffer_number` reduced 6 → 3, `db_write_buffer_size_mb` reduced 1024 → 512. VPS sizing guide rewritten with multi-ledger CF count warnings.
- **change(storage)**: `RocksMemoryConfig` default `write_buffer_size_mb` reduced 128 → 32 for multi-ledger safety. Doc-comments updated with memory scaling formula.

---

## [0.5.21] - 2026-03-21 — Direct I/O: eliminate Docker OOM crashes

### Performance
- **perf(storage/critical)**: RocksDB Direct I/O enabled (`set_use_direct_reads`, `set_use_direct_io_for_flush_and_compaction`). Bypasses kernel page cache entirely, eliminating 4-10 GB of cgroup-accounted memory that caused Docker OOM kills within hours of sustained operation. All SST reads now go exclusively through RocksDB's own block cache. Root cause fix for production crashes: Linux counts page cache towards cgroup `mem_limit`, so Application RSS (2-3 GB) + page cache (4-10 GB) exceeded the 14 GB Docker limit.
- **perf(storage)**: `block_cache_size_mb` default increased 512 → 1024 MB. With Direct I/O, the block cache is the ONLY read cache (kernel page cache bypassed). Larger cache ensures index/filter blocks + hot data blocks remain in RAM.

### Changed
- **change(storage)**: `advise_random_on_open(true)` removed from both `cf_opts_with_bloom()` functions. Direct I/O makes fadvise hints irrelevant — the kernel page cache is no longer used at all.
- **change(config/testnet)**: `block_cache_size_mb` increased 512 → 1024 MB. Memory sizing guide updated for Direct I/O era (cache column doubled in VPS sizing table).

---

## [0.5.20] - 2026-03-21 — RocksDB write stall elimination: sustained high-TPS tuning

### Performance
- **perf(storage/critical)**: RocksDB write stall prevention v2. L0 thresholds doubled again (40/56 → 80/120), pipelined writes enabled (`set_enable_pipelined_write`), background jobs scaled to CPU core count (min 8), sub-compactions increased (3 → 4), memtable merge before flush (`min_write_buffer_number_to_merge = 2`). Eliminates the periodic 20-60 blk/s stalls observed in 75-minute VPS monitoring.
- **perf(storage/critical)**: Multi-block WriteBatch in background persist. New `DagStorage::append_blocks_batch()` batches up to 64 blocks into a single `WriteBatch` + `db.write()` call. Reduces WAL appends and DB mutex acquisitions by up to 64x. RocksStore implementation uses `multi_get_cf` for batch dedup check and single atomic write. Default trait impl falls back to per-block writes for non-RocksDB backends.

### Added
- **feat(storage)**: `DagStorage::append_blocks_batch()` — batch block persistence with default per-block fallback.
- **feat(storage)**: `RocksStore::append_blocks_batch()` — optimized single-WriteBatch implementation for multi-block persistence.

### Changed
- **change(config/testnet)**: `max_utxos` increased 250K → 2M. With simulator generating 2M+ UTXOs, the 250K LRU cache had 87.5% miss rate — every coin selection triggered ~900 RocksDB reads. Memory cost: ~400 MB (acceptable on 16 GB VPS).
- **change(config/testnet)**: `max_write_buffer_number` increased 3 → 6. More memtable buffering before flush stalls under sustained write pressure.

---

## [0.5.19] - 2026-03-20 — Sustained TPS degradation elimination: 4 performance fixes

### Performance
- **perf(server/critical)**: Coin selection O(N log N) → O(1) fast path. `select_utxos()` now collects at most 256 UTXOs via `utxos_for_selection()` with early-exit from the DashSet address index. For coordinator addresses with millions of fee reward UTXOs, this avoids cloning/sorting the entire set. Falls back to full scan + sort only when 256 UTXOs can't cover the target (rare). New `utxos_by_address_for_selection()` method on `ShardedUtxoSet` + `utxos_for_selection()` trait method on `NetDagAdapter`.
- **perf(server/critical)**: Eliminate coordinator UTXO proliferation. All 6 callers of `create_reward_block()` (wallet_send_simple, send_tx, token creation, NFT mint, contract deploy, bridge) now use `accumulate_tx_fee()` which pools fees for periodic consolidated distribution. At 2000 TPS with 10s distribution interval, coordinator UTXO creation drops from 7200/hour to ~360/hour (20x reduction). Root cause fix for sustained TPS degradation.
- **perf(core)**: Background persist batch draining. Consumer loop now drains up to 64 jobs per iteration via non-blocking `try_recv()`. Finality persistence is batched across all jobs in the batch, reducing individual `persist_final()` calls. Reduces channel pressure under high-TPS load.

### Fixed
- **fix(server/critical)**: FeePool race condition — atomic swap eliminates fee loss. `perform_fee_distribution()` previously used read-lock snapshot + later write-lock reset, losing fees accumulated between the two operations. Now uses `std::mem::replace` atomic swap (single write lock, ~1μs). Error recovery via `merge_from()` restores fees to pool on persist failure. Zero fee loss guaranteed.

### Added
- **feat(core)**: `FeePool::merge_from()` — merges another pool's data for error recovery after failed distribution.
- **feat(core)**: `ShardedUtxoSet::utxos_by_address_for_selection()` — early-exit UTXO collection with limit and asset filtering at compact level.
- **feat(interface)**: `NetDagAdapter::utxos_for_selection()` — trait method for limited coin selection with default fallback implementation.
- **feat(server)**: `accumulate_tx_fee()` helper in tx_helpers — unified fee accumulation for all transaction types.

### Changed
- **change(server)**: `create_reward_block()` deprecated in favor of `accumulate_tx_fee()`. Function body preserved for backward compatibility.
- **change(server)**: Fee distribution timing changed from immediate (per-TX Reward block) to periodic (consolidated via `spawn_fee_distributor_task`). Configurable via `distribution_interval_sec` (default 10s on testnet).

---

## [0.5.18] - 2026-03-20 — Fix supply endpoint EDN wallet balances + Eden TPS optimization + deploy resilience

### Added
- **feat(test)**: `test_sustained_tps_stress` — 5-minute sustained TPS stress test in DAG sandbox (80 workers, 300s). Each `send_simple` creates 2 blocks (TX + Reward). Measures TPS in 5s intervals with time-series report (TPS, block count, P50/P95/P99 latencies). Detects degradation by comparing first-minute vs last-minute avg TPS. **Proved: 13,362 avg TPS over 5 min, 4.0M TX, 8.02M blocks (26,712 blk/s), 0 failures, 4.6% degradation — EXCELLENT.** RocksDB `block_count_estimate()` severely undercounts under write pressure (reported 910K vs 8.02M actual) — test uses accurate `TX×2` calculation.

### Fixed
- **fix(infra)**: `deploy-testnet.sh` and `upgrade-testnet.sh` now survive Docker Compose ghost container errors. Root cause: Docker Compose v2 can desync with containerd, leaving phantom container references that cause `"No such container"` errors. Fix: (1) pre-cleanup via `docker compose rm -f -s` + project-label cleanup, (2) `|| true` on `docker compose up` (ghost errors don't abort the script), (3) per-service verification loop — if any service didn't start, it's retried individually. Also added `--remove-orphans` to all `docker compose up` calls.
- **fix(api/critical)**: `GET /v1/supply` wallet balances (`admin_balance`, `node_balance`, `treasury_balance`) always showed PMS native balance, even when `circulating_supply` auto-resolved to edenite on custom ledgers. Dashboard showed "2.36 EDN circulating" but "0 EDN" for all wallets. Root cause: all wallet balance calls used `balance_by_address()` (PMS native only) instead of `balance_by_address_and_asset()` with the resolved asset. Fix: when a custom token is resolved (auto-fallback or explicit `?asset_id=`), wallet balances now use `balance_by_address_and_asset(&addr, resolved_asset)`.

### Added
- **feat(simulator)**: `edn_sends_per_tick` config field for `AgentGameConfig`. Like PMS `sends_per_tick`, allows each agent to send N sequential EDN transfers per tick during Phase 2 instead of 1. Each send re-queries balance for accurate UTXO tracking. Break on error or balance below threshold. Default: 1 (backward compat).
- **feat(test)**: `test_supply_endpoint_edn_balances` — sandbox test validating the supply endpoint fix. Burns 5 cubes → distributes EDN refunds → sends EDN (triggering 5% transfer fee) → queries `/l/eden/v1/supply?asset_id=edenite` → asserts `admin_balance > 0` (was always "0" before the fix). Also validates auto-resolve behavior when PMS native exists on eden.
- **feat(test)**: `get_supply()` helper method on Sandbox struct — queries supply endpoint with optional ledger and asset_id parameters.

### Changed
- **change(simulator)**: Testnet `burn_cooldown_ticks` increased to align with `distribution_interval_sec`: users/miners 10→15, whales 10→5 (×5s=25s), savers 10→3 (×10s=30s). Ensures agents stay in cooldown long enough for `fee_distribution` (now 10s) to deliver EDN UTXOs before Phase 3 remint triggers. Previously, cooldown (10s) expired before distribution (30s) → Phase 2 (send EDN) almost never fired.
- **change(simulator)**: All game-enabled agent groups now have `edn_sends_per_tick` configured: users=5, miners=10, whales=3, savers=5. Estimated Eden TPS boost: ~3→~355 EDN sends/s.
- **change(config)**: Testnet `distribution_interval_sec` reduced 30→10s. Faster EDN UTXO delivery aligns with agent cooldown windows.

### Infrastructure
- **infra(config)**: `agents_testnet.toml` and `agents_dev.toml` updated with new `edn_sends_per_tick` and `burn_cooldown_ticks` values per agent group.

---

## [0.5.17] - 2026-03-19 — EDN lifecycle integration test + diagnostic logging

### Added
- **feat(test)**: `test_pms_throughput_benchmark` — sandbox benchmark proving PMS engine handles ~10,000 TPS (10 workers × 100 tx, 100% success rate). Demonstrates that simulator's ~200 PMS TPS is an agent config bottleneck (1 tx/tick/agent), not an engine limitation. Engine headroom: ~50x.
- **feat(simulator)**: `sends_per_tick` config field for `AgentBehavior::Random` and `Coordinator`. Allows each agent to send N sequential PMS transactions per tick instead of 1. Solves PMS TPS disparity vs Eden (200 vs 2000) by multiplying each agent's throughput. Each send is sequential within a wallet (UTXO chain dependency), but concurrent across agents.
- **feat(test)**: `test_edn_transfer_fee_flow` — comprehensive sandbox test validating the COMPLETE Edenite lifecycle: cube NFT burn → EDN refund (AccumulateRefund) → FeePool distribution → EDN UTXOs → send EDN → 5% TransferFee → coordinator receives EDN. Includes 7 assertions with conservation check (remaining + sent + fee = refund). Validates the exact flow the VPS simulator runs.
- **feat(test)**: Sandbox helpers: `get_utxos()`, `get_asset_balance()`, `register_contract()`, `mint_nft()`, `burn_nft_simple()`, `send_asset()` — reusable building blocks for future integration tests.
- **feat(simulator)**: Diagnostic logging for Phase 2 detection in `RandomAgent::game_tick()` — logs EDN balance checks (non-zero), query failures, and Phase 2 entry events. Helps diagnose why agents may never enter the EDN transfer phase on the VPS.
- **feat(simulator)**: Diagnostic logging in `GameEngine::send_edenite()` — pre-send (amount, recipient, asset) and post-send (block_id, gas_fee, transfer_fee) log lines.
- **feat(simulator)**: `transfer_fee` field added to `SendResponse` struct in `types.rs` for visibility into smart contract transfer fees.
- **feat(contracts)**: Debug logging in `evaluate_transfer()` — logs when no transfer contracts found and when evaluating contracts (count, amount, asset, ledger).

### Fixed
- **fix(test)**: `boot_sandbox()` restructured to load admin wallet FIRST, then write coordinator keys into config file BEFORE `LedgerManager::bootstrap()`. Critical fix: `CoreAdapter::new()` calls `load_config()` internally — programmatic settings modifications were ignored for coordinator key, causing NFT burn validation to reject coordinator-signed burns.

### Performance
- **perf(simulator)**: Lock-free game engine pattern — `GameEngine` write lock no longer held during HTTP calls. Previously, 65 agents competed for a single `RwLock<GameEngine>` write lock held for 1-2 seconds each during Phase 1 (burn) and Phase 3 (remint 80-120 cubes), serializing all game operations. New static methods (`execute_burn_batch()`, `generate_mint_specs()`, `execute_mints_parallel()`) run HTTP calls without any lock. Write lock held only for brief HashMap registry operations (`drain_cubes()`, `restore_cubes()`, `register_minted()`).
- **perf(simulator)**: Cached game client — `RandomAgent` caches `(DagClient, edenite_asset_id)` on first game_tick instead of acquiring a read lock every tick. Phase 2 (send EDN) and EDN balance queries now run entirely lock-free.

### Changed
- **change(simulator)**: `sends_per_tick` added to `AgentBehavior::Random` (default: 1). Agents now send N PMS transactions per tick in a sequential loop. Configured values: snipers=25 (1250 tx/s), fast=15 (480 tx/s), users=5 (60 tx/s), whales=3, savers=5. Theoretical testnet total: ~1793 PMS tx/s (was ~95 with sends_per_tick=1).
- **change(simulator)**: `burn_cooldown_ticks` added to `AgentGameConfig` (default: 10 ticks). After burning cubes, agents wait N ticks before reminting, giving `fee_distribution` time to deliver EDN UTXOs. Without this, agents cycled burn→remint faster than the 600s distribution interval, so Phase 2 (send EDN → trigger TransferFee) was almost never reached. During cooldown, agents check EDN balance each tick and enter Phase 2 immediately when EDN arrives.
- **change(simulator)**: All agent config files (`agents_dev.toml`, `agents_testnet.toml`) updated with `burn_cooldown_ticks = 10` and `sends_per_tick` tuned per agent group.

### Infrastructure
- **infra(docker)**: `docker-compose.testnet.yml` healthcheck timing adjusted: `interval=10s` (was 5s), `timeout=5s` (was 3s), `retries=10` (was 15), `start_period=300s` (was 120s). Prevents premature unhealthy status during initial bootstrap with large block counts.

---

## [0.5.16] - 2026-03-19 — Revert TPS regression + TPS logger + deploy automation + DAG sandbox

### Added
- **feat(server)**: TPS logger — periodic throughput recording for production diagnostics. Spawns a background task that writes a JSONL line every 10 minutes to `{data_dir}/tps_log.jsonl`. Each deployment gets a unique UUID so operators can distinguish restarts from sustained runs. Fields: `ts`, `epoch_ms`, `deployment_id`, `ledger`, `tps_60s`, `block_count`, `circulating_supply`, `total_burned`, `node_pk`, `uptime_min`.
- **feat(deploy)**: `deploy-testnet.sh` now supports `--yes`/`-y` non-interactive mode for CI/CD and AI-driven deploys. Auto-answers prompts with defaults, reuses admin token from backup, skips macOS file picker dialog. Backup saved to external drive (`/Volumes/.../pms-key/`) in auto mode.
- **feat(test)**: DAG Sandbox (`dag_sandbox.rs`) — production-like in-process PMS engine for integration tests. Boots full LedgerManager + EventBus + ContractListener + fee distribution on a random port with tempdir RocksDB. Reusable `boot_sandbox()` returns a `Sandbox` struct with helpers: `create_ledger()`, `deposit_gas_pool()`, `faucet_mint()`, `send_simple()`, `get_balance()`, `distribute_fees()`. First test: `test_coordinator_receives_eden_fees` verifies coordinator receives fee revenue from eden transactions via immediate Reward blocks.
- **feat(test)**: Local TPS Benchmark (`local_bench.rs`) — in-process benchmark simulating 6 vCores (VPS constraint). Achieves ~9,000 TPS locally vs 2,000 TPS on VPS. Dev mode config generation (no coordinator key enforcement).
- **change(api)**: `FeePoolRefundSink` made `pub` in `api.rs` for integration test wiring.

### Fixed
- **fix(server/critical)**: UTXOs endpoint (`GET /v1/wallet/{address}/utxos`) was missing `asset_id` field in the response. `UtxoFlatItem` dropped `asset_id` from `TxOutput` during conversion — clients filtering by asset always saw 0 balance (e.g., EDN). Added `asset_id: Option<String>` to `UtxoFlatItem`.
- **fix(simulator)**: `UtxoEntry` field names (`txid`/`index`) didn't match server's camelCase format (`txId`/`outIdx`). Deserialization failed silently → balance always 0. Added `#[serde(alias)]` to accept both formats.
- **fix(server)**: Activity cache eviction was broken — cache could grow unboundedly. Fixed: two-phase eviction.
- **fix(server)**: Node registry `cleanup_stale()` was defined but never called. Fixed: piggyback cleanup on `register()`.

### Reverted
- **revert(server/critical)**: Restored `add_utxo()`/`apply_utxo_delta()` calls after `persist_block()` in 6 code paths: `fee_distribution.rs` (Mint + Reward), `wallet_factory.rs` (faucet mint), `tx_helpers.rs` (per-tx reward), `token.rs` (token mint), `compliance.rs` (Seize + Reverse). Removing these caused the 22x TPS regression (2000→90). The `add_utxo` calls make UTXOs immediately available in RAM for subsequent transactions; without them, the system stalled waiting for `persist_block`'s async delta propagation.
- **revert(storage)**: Removed `use_direct_io_for_flush_and_compaction(true)` (does not help, made newly-flushed SSTs cold). Kept `advise_random_on_open(true)` — essential to prevent OOM on 8 GB Docker cgroups. The TPS=90 was misattributed to `advise_random`; the true cause was the `add_utxo` removals above.

### Performance
- **perf(server/critical)**: `internal_health()` endpoint called `all_block_ids()`, loading ALL block IDs into a `Vec<String>`. At 15M blocks, this allocated **~1.2 GB per call**. Replaced with `block_count_estimate()` (O(1), 0 bytes).
- **perf(main)**: `block_count()` at startup replaced with `block_count_estimate()` — eliminates a full RocksDB table scan (O(N) with N=15M) during boot.
- **perf(wallet)**: `gather_wallet_utxos_dec()` and `gather_address_utxos_dec()` no longer call `all_block_ids()` for large `scan_limit` values. Now always uses bounded `recent_ids(scan_limit)`.

### Changed
- **change(api)**: `API_VERSION` bumped 6 → 7 (UTXOs endpoint now includes `asset_id` field).

---

## [0.5.14] - 2026-03-18 — Fix custom ledger path routing + balance endpoint enhancement

### Fixed
- **fix(server/critical)**: All per-ledger endpoints with path parameters (`/l/{id}/v1/wallet/{address}/utxos`, `/v1/nft/{token_id}`, `/v1/blocks/{id}`, `/v1/wallet/{address}/nfts`, `/v1/wallet/{address}/activity`, `/v1/tokens/{asset_id}`) returned **500 Internal Server Error** on custom ledgers. Root cause: Axum's outer `Path` extraction (`ledger_id`, `rest`) leaked into the inner router via request extensions, causing handlers expecting 1 path param to receive 3. Fix: reset `parts.extensions` before forwarding to inner router. **Impact**: This bug silently broke the simulator's EDN balance queries — agents could never see their EDN → never triggered Phase 2 (EDN transfers) → TransferFee contract never fired → coordinator received 0 fees.

### Added
- **feat(api)**: `POST /v1/balance` now accepts optional `ledger_id` and `asset_id` fields. Clients can query any ledger's balance from the main endpoint (e.g., `{"address":"8e1...", "ledger_id":"eden", "asset_id":"edenite"}`). Response echoes back `ledger_id` and `asset_id` for clarity.
- **feat(interface)**: Added `balance_by_address_and_asset()` to `NetDagAdapter` trait — supports asset-filtered balance queries (PMS native O(1) cache, custom tokens via shard scan).

### Changed
- **change(api)**: `API_VERSION` bumped 5 → 6 (new balance endpoint fields, path routing fix).

---

## [0.5.13] - 2026-03-18 — Fix bootstrap OOM: chronological loading + ghost cleanup

### Performance
- **perf(core/critical)**: Bootstrap now loads blocks **chronologically** via `by_time` CF reverse iterator instead of lexicographically. Block IDs are hashes, so lexicographic "newest N" selects random blocks — causing 99.8% orphan tips (49,904/50,000 on Eden with 13.2M blocks). Chronological loading preserves parent-child locality, reducing orphans to ~2-5%.
- **perf(ledger/critical)**: Eliminated ~1.16 GB allocation in `LedgerInstance::bootstrap()`. `all_block_ids()` loaded all 13.2M IDs into a Vec just to check `.is_empty()`. Replaced with `is_empty()` — a single RocksDB iterator seek (O(1), 0 bytes).
- **perf(core)**: `block_count_estimate()` uses RocksDB `estimate-num-keys` property (O(1)) instead of full table scan for diagnostic logging.
- **perf(core)**: Ghost entry cleanup after bootstrap. Parent IDs referenced by loaded blocks but outside the loaded window created phantom entries in `children_count`/`children_idx` DashMaps (~100-800 MB). `cleanup_ghost_entries()` removes them in a single pass.
- **perf(server)**: `get_block_parents()` fallback in `tx_helpers.rs` replaced `all_block_ids()` (full scan) with `recent_ids(1)` (O(1)).

### Added
- **feat(storage)**: 3 new `DagStorage` trait methods with optimized `RocksStore` overrides:
  - `is_empty()` — O(1) single iterator seek on `idx_blocks` CF
  - `newest_block_ids_by_time(n)` — reverse iterator on `by_time` CF for chronological ordering, with fallback to lexicographic when `by_time` is empty (e.g. after `import_json`)
  - `block_count_estimate()` — O(1) via RocksDB `rocksdb.estimate-num-keys` property
- **feat(core)**: `ConcurrentDag::cleanup_ghost_entries()` — post-bootstrap pass that removes DashMap entries for parent block IDs not in the loaded block set.

### Changed
- **change(core)**: Bootstrap `insertion_order` now uses the loading order directly (oldest-first from chronological iterator) instead of re-sorting lexicographically. This means `prune_oldest()` evicts truly oldest blocks first.
- **change(core)**: Bootstrap diagnostic log changed from `warn` to `info` level and reports "post-ghost-cleanup" counts.

---

## [0.5.12] - 2026-03-18 — Fix PMS fee bootstrap deadlock on custom ledgers

### Fixed
- **fix(server/critical)**: Custom asset transfers (e.g., EDN on eden) failed with "insufficient PMS for fee" because agents had no PMS on the custom ledger. This created a chicken-and-egg deadlock: PMS fees required PMS to exist, but PMS could only appear via fee distribution which required successful transfers. Now, when PMS is unavailable for the protocol fee on custom asset transfers, the fee is gracefully waived. Smart contract transfer fees (in the custom asset) still apply, providing fee revenue to the ledger creator.
- **fix(server)**: `create_reward_block()` silently swallowed errors (returned `None` without logging). Added structured logging for forge failures, persist rejections, and persist errors — makes debugging fee distribution issues on custom ledgers visible in production logs.
- **fix(server)**: `perform_fee_distribution()` used `state._cfg.network.network_id` (ServerConfig) instead of `state.settings.network.network_id` (Settings). While functionally equivalent today (both load global config), `_cfg` is an internal field not intended for fee distribution. Switched to the canonical `settings` field for consistency and future-proofing.

---

## [0.5.11] - 2026-03-17 — Fix bootstrap OOM on large ledgers

### Performance
- **perf(core/critical)**: Bootstrap no longer loads all block IDs into memory. Added `newest_block_ids(n)` to `DagStorage` trait — uses a **reverse RocksDB iterator** to read only the N newest IDs. For Eden (7.8M blocks), this reduces bootstrap memory from ~500 MB (full `Vec<String>`) to ~1.6 MB (25K IDs only). Eliminates the primary cause of OOM kills on 8 GB VPS.
- **perf(storage)**: `RocksStore::newest_block_ids()` override uses `IteratorMode::End` to read N keys in reverse order, then reverses for ascending lex order. O(N) instead of O(total_blocks).

### Changed
- **change(docker/testnet)**: Engine memory limit raised from 6g→7g to provide headroom on 8 GB VPS.
- **change(config/testnet)**: Added RAM scaling guide (8/16/32 GB) as comments in `[rocks]` section for easy tuning.

---

## [0.5.10] - 2026-03-17 — Fix transfer fees broken on custom ledgers

### Fixed
- **fix(server/critical)**: Transfer fees (smart contract `OnTransfer` trigger) never applied on custom ledgers. `evaluate_transfer()` in `prepare_tx()` and `wallet_send_simple()` queried the per-ledger store (empty `contracts` CF) instead of the main store where contracts are registered. Added `contract_store: Arc<dyn ContractStorage>` to `AppState` — always points to main RocksDB. Burn refunds were unaffected (used separate `main_store_for_contracts`).
- **fix(tests)**: Updated all test files constructing `Settings` inline to include fields added in v0.5.7–v0.5.9 (`Rocks::max_open_files`, `P2pConfig` scaling limits).

---

## [0.5.9] - 2026-03-17 — Configurable P2P scaling limits

### Added
- **feat(config)**: 6 new `[p2p]` TOML settings for P2P resource limits: `max_connections` (default 256), `per_peer_queue_cap` (default 2000), `max_orphans` (default 2000), `max_inflight_requests` (default 10000), `max_parent_deps` (default 5000), `max_peer_retries` (default 20).
- **feat(server)**: `Server` struct stores P2P limits from config instead of reading hardcoded constants. `api_only()` uses `P2pConfig::default()` values.

### Changed
- **change(server)**: All P2P resource limits (`MAX_PEER_CONNECTIONS`, `PER_PEER_Q_CAP`, `MAX_INFLIGHT_GETBLOCK`, `MAX_ORPHANS`, `MAX_PARENT_DEPS`) replaced with configurable fields read from `[p2p]` TOML section. No recompilation needed for scaling.
- **change(main)**: `MAX_PEER_RETRIES` hardcoded constant removed — now reads `max_peer_retries` from `[p2p]` config.
- **change(config/testnet)**: Added P2P limits documentation to testnet config with recommended values for 8 GB VPS.

---

## [0.5.8] - 2026-03-17 — Pre-production stability audit (crash prevention)

### Fixed
- **fix(storage/critical)**: RocksDB `max_open_files` now configurable (default 512). Previously unlimited — with 66+ CFs on VPS (ulimit=1024), FD exhaustion caused crashes.
- **fix(fee_distribution/critical)**: Replaced all `[0]` index accesses on treasury wallet lists with `.first()` / `.cloned()`. Empty treasury config no longer panics.
- **fix(storage/critical)**: Activity pagination iterator `list_wallet_activity_paginated()` no longer panics on empty/exhausted iterators. Defensive `Option` handling replaces `.unwrap()` chain.
- **fix(storage/critical)**: `from_be_i64()`, `be_to_ts()`, `le_to_u64()` now return 0 for malformed input instead of panicking on non-8-byte slices.
- **fix(ledger/critical)**: `ensure_schema()` failure is now fatal (`bail!`) instead of silently logged as `warn!`. Prevents operating on outdated/corrupted schema.
- **fix(server)**: TLS config `.unwrap()` replaced with proper error message when `api_tls_enabled=true` but `[tls]` section missing.
- **fix(bridge)**: `disable_bridge()` `.expect()` replaced with `anyhow::bail!` to handle race condition where link is deleted between disable and get.
- **fix(economics)**: `TpsTracker` mutex lock uses `unwrap_or_else(|e| e.into_inner())` to recover from poison instead of cascading panics.

### Added
- **feat(main/critical)**: Graceful SIGTERM/SIGINT shutdown handler. On Docker stop: flushes RocksDB WAL for all ledgers before exiting. Prevents WAL corruption from mid-write kills.
- **feat(server)**: P2P connection semaphore (max 256 concurrent inbound connections). Prevents OOM from connection bombs.
- **feat(config)**: `max_open_files` field in `[rocks]` config section and `RocksMemoryConfig` struct.
- **feat(validation)**: `skip_utxo_checks=true` now emits `tracing::error!` audit log. Flags accidental bypass of double-spend detection in production.

### Changed
- **change(main)**: Peer retry loop now uses exponential backoff (5s→60s) with max 20 attempts instead of retrying forever. Prevents leaked tasks for unreachable peers.
- **change(limits)**: Reduced P2P memory constants for VPS: `PER_PEER_Q_CAP` 10K→2K, `MAX_INFLIGHT_GETBLOCK` 100K→10K, `MAX_ORPHANS` 10K→2K, `MAX_PARENT_DEPS` 20K→5K. Saves ~240 MB under load.

---

## [0.5.7] - 2026-03-17 — Configurable RocksDB memory tuning (OOM prevention)

### Added
- **feat(config)**: 4 new `[rocks]` settings for RocksDB memory control: `write_buffer_size_mb` (per-CF memtable, default 128), `max_write_buffer_number` (per-CF, default 3), `block_cache_size_mb` (shared LRU, default 512), `db_write_buffer_size_mb` (global memtable cap, default 512).
- **feat(storage)**: `RocksMemoryConfig` struct — encapsulates RocksDB memory tuning parameters, passed to `new()` and `open_db_multi_prefix()`. `Default` impl preserves backward-compatible values.
- **feat(storage)**: Global memtable budget via `set_db_write_buffer_size()` — caps total memtable memory across ALL column families. Critical for multi-ledger setups where N×33 CFs can spike and OOM.
- **feat(storage)**: Startup log line showing applied memory tuning (`write_buffer_mb`, `max_write_buffers`, `block_cache_mb`, `db_write_buffer_mb`).

### Changed
- **change(storage)**: `apply_db_tuning()` now accepts `&RocksMemoryConfig` instead of using hardcoded values. All 4 memory pools are configurable.
- **change(storage)**: `RocksStore::new()` and `open_db_multi_prefix()` now require a `&RocksMemoryConfig` parameter.
- **change(config/testnet)**: Testnet config tuned for 8 GB VPS with multiple ledgers: `write_buffer_size_mb=64`, `block_cache_size_mb=256`, `db_write_buffer_size_mb=512`.

---

## [0.5.6] - 2026-03-16 — Multi-wallet TransferFee splits + Ledger ownership transfer

### Added
- **feat(contracts)**: `TransferFeeSplit` struct — each split has `address: String` and `share_bps: u32` (basis points out of 10,000). `ContractAction::TransferFee` now uses `splits: Vec<TransferFeeSplit>` instead of a single `beneficiary_address`. Dust-free rounding: last split gets `total - sum(previous)`.
- **feat(contracts)**: `ContractAction::validate()` method — validates TransferFee splits sum to 10,000, non-empty, positive shares, non-empty addresses.
- **feat(contracts)**: Contract update endpoint `PUT /admin/contracts/{contract_id}` — partial update of scope, actions, enabled. Auto-bumps contract version. Validates TransferFee splits on update.
- **feat(storage)**: `update_contract()` method on `ContractStorage` trait + RocksDB and InMemory implementations.
- **feat(storage)**: `LedgerDefStorage` trait — `get_ledger_def()`, `put_ledger_def()`, `list_ledger_defs()`, `update_owner()`. RocksDB implementation in new `ledger_defs` column family.
- **feat(storage)**: Schema migration 8→9 — adds `ledger_defs` column family for persisting ledger definitions.
- **feat(ledger)**: Ledger ownership transfer via DAG block — `POST /admin/ledgers/{ledger_id}/transfer-ownership` creates an encrypted `LedgerOwnershipTransfer` block in the DAG for full traceability, then applies state change to RocksDB + RAM.
- **feat(ledger)**: `owner_pubkey` and `owner_x25519_pubkey` fields in `CreateLedgerRequest` — specify ownership and encryption key at creation time.
- **feat(ledger)**: Ledger definition persistence — dynamically created ledgers and ownership changes survive restarts. `load_persisted_ledgers()` called at startup.
- **feat(ledger)**: `LedgerManager::update_def()` — hot-swap a ledger's definition in-memory without restart.
- **feat(payload)**: New `PlainPayload::LedgerOwnershipTransfer` variant (#18) — records ledger ownership changes in the DAG. Contains cleartext `ledger_id` for routing/validation + `EncryptedPayload` with `OwnershipTransferData` (new_owner_pubkey, reason). Encrypted for coordinator + current owner + new owner (X25519+AES-256-GCM).
- **feat(config)**: `owner_x25519_pubkey: Option<String>` on `LedgerDef` — stores the owner's X25519 public key for encrypted DAG blocks.

### Changed
- **change(contracts)**: `ContractAction::TransferFee` now uses `splits: Vec<TransferFeeSplit>` instead of single `beneficiary_address: String`. Breaking change for contract registration payloads.
- **change(config)**: Added `Serialize` derive to `LedgerDef`, `LedgerFeesOverride`, `LedgerValidationOverride` (needed for RocksDB JSON persistence).
- **change(server)**: `API_VERSION` 4 → 5 (contract update endpoint + TransferFee splits format + ownership transfer).
- **change(storage)**: `CURRENT_VER` 8 → 9 (new `ledger_defs` column family).
- **change(simulator)**: Updated `ContractActionSim::TransferFee` to use `TransferFeeSplitSim` splits format.
- **change(ledger)**: Ownership transfer refactored from direct RocksDB write to DAG-block-first pattern (encrypted `LedgerOwnershipTransfer` block → persist → apply state). Follows blockchain convention: all state mutations go through the DAG.

---

## [0.5.5] - 2026-03-16 — Smart contract transfer fees (deductive, per-ledger)

### Added
- **feat(contracts)**: New `OnTransfer` trigger in `ContractTrigger` — fires on UTXO token transfers. Supports `asset_id` filter (None = any asset, Some("edenite") = specific).
- **feat(contracts)**: New `TransferFee` action in `ContractAction` — routes a fee to a fixed `beneficiary_address`. Uses `TransferFeeFormula` (PercentageBps or FixedAmount).
- **feat(contracts)**: New `TransferFeeFormula` enum — `PercentageBps { rate_bps }` (fee = amount * bps / 10000) and `FixedAmount { amount }` (flat fee per transfer).
- **feat(contracts)**: `evaluate_transfer()` in `pms-contracts/engine.rs` — evaluates transfer fee contracts at TX preparation time. Returns `Vec<TransferFeeResult>` with beneficiary + fee amount.
- **feat(storage)**: `find_transfer_contracts()` method on `ContractStorage` trait + RocksDB and InMemory implementations. Filters by `asset_id`, `ledger_id`, scope, and enabled status.
- **feat(server)**: Transfer fee outputs added to `prepare_tx()` and `wallet_send_simple()`. The fee is an additional `TxOutput` in the transaction (deductive: sender pays amount + fee). No minting — pure UTXO output.
- **feat(server)**: `transfer_fee` field added to `PrepareTxResponse` and `SendSimpleResponse` — clients can display the total cost breakdown.
- **feat(simulator)**: `GameEngine::setup()` now registers a 5% transfer fee contract on the game ledger, routing fees to the coordinator wallet (ledger creator revenue).

### Changed
- **change(server)**: `API_VERSION` 3 → 4 (tx/prepare and send_simple responses now include `transfer_fee` field).

---

## [0.5.4] - 2026-03-16 — Fix backup path writing to container layer instead of volume

### Fixed
- **fix(storage/critical)**: RocksDB checkpoints (backups) were written to `./backups/pms` (relative CWD), which in Docker resolves to the container's writable layer instead of the mounted volume. On testnet with hourly checkpoints and 7 retained copies, this filled the entire 237 GB disk. Backup path now derived from the DB path itself (`db_path.parent()/backups/pms`), guaranteeing checkpoints land on the same volume as the data.

### Changed
- **change(storage)**: Added `db_path: PathBuf` field to `RocksStore` struct. Populated from the actual DB path in all constructors (`new()`, `from_shared_db()`, `open_read_only()`, `open_secondary()`).
- **change(storage)**: Reduced checkpoint rotation from 7 to 3 retained copies (75 GB → 75 GB max instead of 175 GB).

---

## [0.5.3] - 2026-03-16 — Fix EventBus routing: burns on custom ledgers now reach ContractListener

### Fixed
- **fix(contracts/critical)**: Burns on custom ledgers (eden, etc.) emitted `NftBurnProcessed` on the **per-ledger** EventBus, but the `ContractListener` was subscribed to the **main** EventBus only. Events never reached the listener → zero EDN distributed. Fixed by adding `contract_event_bus: Option<EventBus>` to `AppState`, always pointing to the main adapter's bus. `emit_nft_burn_processed()` now uses this shared bus regardless of which ledger the burn occurs on.

---

## [0.5.2] - 2026-03-15 — Extract contract engine into pms-contracts crate + EventBus decoupling

### Changed
- **refactor(contracts)**: Extracted contract evaluation engine into new `pms-contracts` crate. `contract_engine.rs` moved from `pms-server` to `pms-contracts/src/engine.rs`. The `ContractResult` type, `evaluate_nft_burn()`, formula evaluation, and all 9 unit tests moved intact.
- **refactor(contracts)**: Decoupled contract evaluation from NFT burn handlers via EventBus. The 3 direct calls to `evaluate_contracts_after_burn()` in `nft.rs` are replaced by `NftBurnProcessed` event emissions. A new `ContractListener` subscribes to these events and evaluates contracts asynchronously.
- **refactor(contracts)**: Introduced `RefundSink` trait in `pms-contracts` to decouple refund accumulation from `pms-server`'s `FeePoolRegistry`. `FeePoolRefundSink` in `api.rs` bridges the two.
- **refactor(server)**: Removed `contract_store` field from `AppState` — the contract listener receives its own `Arc<dyn ContractStorage>` at startup, always pointing to the main RocksDB.

### Added
- **feat(event)**: New `PmsEvent::NftBurnProcessed` variant carrying `block_id`, `ledger_id`, `burner_address`, `token_ids`, and pre-fetched `NftMetadata`. Emitted by burn handlers BEFORE `apply_action()` (which destroys metadata references).
- **feat(contracts)**: `pms-contracts` crate — dedicated crate for contract engine + EventBus listener. Contains `engine.rs` (evaluation), `listener.rs` (subscriber + `RefundSink` trait).

---

## [0.5.1] - 2026-03-15 — Fix EDN burn refunds not distributed on custom ledgers

### Fixed
- **fix(contracts/critical)**: Smart contracts registered on the main ledger were invisible to NFT burn handlers on custom ledgers (e.g. eden). `evaluate_contracts_after_burn()` used `state.store` (per-ledger RocksDB) for contract lookups, but contracts are only stored in the **main** RocksDB. Burns produced zero refunds → agents never received EDN. Fixed by adding `contract_store` field to `AppState` that always points to the main store, and using it for contract lookups regardless of which ledger the burn occurs on.

---

## [0.5.0] - 2026-03-15 — Service Status Monitoring + Deploy Fixes

### Added
- **feat(gateway)**: Background infrastructure health checker. Polls Engine, Prometheus, Simulator, and Caddy every 20s with concurrent requests and caches the results. New endpoint `GET /services/status` returns a JSON snapshot with service name, status (`up`/`down`/`degraded`), latency, and optional detail (e.g. block count for Engine). Configurable via `SERVICES_MONITOR` and `SERVICES_CHECK_INTERVAL` env vars.
- **feat(dashboard)**: Service status bar in pms-dashboard (Svelte). Displays colored dots (green/red/orange) with service names in the header top-left area. Self-contained component with 30s polling to `/services/status`. Responsive: hides names on mobile, shows only dots.
- **feat(simulator)**: Credential validation with backoff retry. `SimConfig::validate_credentials()` checks all required secrets (API key, admin token, coordinator key/address). Startup loop retries up to 10 times with increasing delays (30s, 45s, 60s, ... +15s per attempt). Exits with code 0 after exhaustion so `on-failure` restart policy stops.

### Fixed
- **fix(deploy)**: Fix TOML config corruption in `deploy-testnet.sh` and `upgrade-testnet.sh`. SSH `sed` commands with double-quoted TOML values stripped the `"` chars. Fixed by switching to heredocs.
- **fix(deploy)**: Fix deploy starting simulator before API key exists. Core services start first, then API key is created, then simulator starts only if all credentials are present.
- **fix(deploy)**: Fix SCP "No space left on device" — services are now stopped before uploading images to free disk space.

### Infrastructure
- **docker-compose**: Added `SERVICES_MONITOR` and `SERVICES_CHECK_INTERVAL` env vars to gateway service.
- **docker-compose**: Changed simulator restart policy from `unless-stopped` to `on-failure`.
- **dashboard**: Added Vite dev proxy `/services` → gateway (8443) for local development.

---

## [0.4.4] - 2026-03-15 — OOM Fix + Containerd Cleanup

### Fixed
- **infra(critical)**: Fix engine OOM-kill at 4GB container limit. With 632K accumulated blocks, 500K UTXO cache, and 512MB RocksDB block cache, the engine exceeded the 4GB memory cap — triggering 108 container restarts and generating 200GB+ of containerd snapshots.
- **deploy(critical)**: Fix TOML config corruption in `deploy-testnet.sh` and `upgrade-testnet.sh`. Config value restoration used `ssh "sed ..."` where double quotes from TOML values (e.g. `key = "02abc..."`) broke SSH shell quoting — the `"` were stripped, producing invalid TOML (`key = 02abc...`). Engine crashed on restart with a parse error. Fixed by switching all SSH `sed` commands to heredocs (`<< EOF`) where `"` is always literal.
- **simulator(critical)**: Fix crash-loop when `PMS_API_KEY`, `PMS_COORDINATOR_KEY`, or `PMS_COORDINATOR_ADDR` are missing. Simulator now validates all required credentials at startup with a backoff retry loop (30s, 45s, 60s, ... +15s per attempt, max 10 attempts ~16 min). After 10 failed attempts, exits gracefully (code 0) so Docker `on-failure` restart policy does NOT restart it. Previously, the simulator crash-looped indefinitely on missing env vars, filling the disk with containerd snapshots.
- **deploy**: Fix deploy script starting simulator before API key exists. Core services (Engine, Gateway, Caddy, Prometheus) are now started first, then the SDK API key is created, then the simulator is started with all credentials. The simulator is not started at all if any credential is missing.

### Infrastructure
- **docker-compose**: Bumped engine `mem_limit` from 4GB to 6GB (`memswap_limit` too) to prevent OOM kills with large block histories.
- **docker-compose**: Changed simulator restart policy from `unless-stopped` to `on-failure`. The simulator exits with code 0 after exhausting startup retries (missing credentials), so Docker won't restart it endlessly. Operational crashes (exit 1) still trigger restarts.
- **config**: Reduced `max_utxos` from 500,000 to 250,000 in testnet config to lower memory footprint.
- **deploy**: Added containerd snapshot prune documentation and `docker image prune` to deploy script.
- **deploy**: Deploy script now stops all services BEFORE uploading images (prevents crash-loop from filling disk during SCP transfer).
- **systemd**: Added `pms-containerd-cleanup.timer` (daily at 4 AM) to prevent containerd snapshot accumulation from container restarts.

---

## [0.4.3] - 2026-03-14 — Gateway TLS Fix + Error Logging + Deploy Cleanup

### Added
- **config**: New `api_tls_enabled` option in `[client]` section. When `false`, the HTTP API serves plain HTTP even if `[tls]` is configured. P2P TLS is unaffected. Default: `true` (backward compatible). Not used in testnet (simulates prod with full HTTPS).
- **gateway**: `EngineClient` now logs upstream URL and TLS mode on initialization.

### Fixed
- **gateway(critical)**: Fix persistent 502 Bad Gateway caused by silent client fallback. `Client::builder().build()` previously fell back to `Client::new()` on error, losing the `danger_accept_invalid_certs(true)` setting — all HTTPS requests to self-signed engine then failed with opaque "error sending request" messages. Now panics with a clear error message instead of silently degrading.
- **gateway**: Fix opaque error logging — proxy errors now use `{:?}` (Debug format) to show the full reqwest error chain (TLS failures, DNS errors, connection refused) instead of just the top-level "error sending request for url" message.
- **gateway**: `danger_accept_invalid_certs` now only applied when upstream is HTTPS (not for HTTP upstreams).

### Infrastructure
- **deploy**: Added `docker rmi` for old images before `docker load` to prevent containerd snapshot bloat (`/var/lib/containerd/io.containerd.snapshotter.v1.overlayfs/` grew to 213G+ in production).
- **deploy**: Added post-load `docker image prune -f` for dangling layers cleanup.

---

## [0.3.1] - 2026-03-14 — Documentation Obsidian Vault

### Added
- **docs**: Obsidian vault with 28 feature documentation files in `documentation/features/`.
- **docs**: Map of Content (`documentation/MOC.md`) indexing all fiches by category (Infrastructure, Données, Protocole & Consensus, Fonctionnalités).
- **docs**: API reference docs in `documentation/api/` (15 endpoint category files).
- **docs**: Added `.obsidian/` to `.gitignore` (user-specific workspace settings).
- **rules**: Documentation rules in CLAUDE.md — mandatory Obsidian fiches and rustdoc on all public items.
- **rules**: Changelog update rule in CLAUDE.md — mandatory update after every conversation with code changes.

---

## [0.3.0] - Unreleased — Economics System

### Economics Features

All economics features are **opt-in and disabled by default**. No configuration change is required — existing setups continue to work identically.

#### Feature 1: Fee Burn (Deflationary Mechanism)
- Configurable percentage of transaction fees permanently burned (removed from circulation).
- **Config (TOML):** `burn_rate_bps = 3000` in `[fees]` (3000 = 30%, default: `0` = disabled).
- **Runtime hot-swap:** `POST /admin/config` with `{ "SetBurnRate": { "bps": 3000 } }`.
- Set to `0` to disable.
- Burn stats exposed via `/v1/supply` (`total_burned` field).

#### Feature 2: Contract Deployment Fee
- Fee charged when registering a smart contract via `POST /admin/contracts`.
- **Config:** `contract_deployment_fee = "10.0"` in `[fees]` (default: not set = free).
- **Runtime hot-swap:** `{ "SetContractDeploymentFee": { "fee": "10.0" } }` (or `{ "fee": null }` to disable).

#### Feature 3: Cross-Ledger Fee Multiplier
- Bridge transfers between ledgers cost X times more than intra-ledger transfers.
- **Config:** `cross_ledger_fee_multiplier = 2.0` in `[fees]` (default: `2.0`).
- Set to `1.0` to effectively disable the surcharge.

#### Feature 4: Storage Fees (Per-KB Surcharge) — DISABLED BY DEFAULT
- Charges proportional to payload size (relevant for large NFT metadata).
- **Config:** `storage_fee_per_kb = "0.01"` in `[fees]` (default: not set = **disabled**).
- **Runtime hot-swap:** `{ "SetStorageFeePerKb": { "fee": "0.01" } }` (or `{ "fee": null }` to disable).
- Applied on: NFT mints (metadata size), transactions (payload size).

#### Feature 5: Dynamic Fees (Congestion-Based) — DISABLED BY DEFAULT
- Fee multiplier based on current TPS: `multiplier = max(1.0, current_tps / target_tps)`, capped at `max_fee_multiplier`.
- **Config fields in `[fees]`:**
  - `dynamic_fee_enabled = false` (default: `false` = **disabled**)
  - `target_tps = 100` (default: 100 — fees start increasing above this)
  - `max_fee_multiplier = 5.0` (default: 5.0 — max 5x fee increase)
- **Runtime hot-swap:** `{ "SetDynamicFee": { "enabled": true, "target_tps": 100, "max_multiplier": 5.0 } }`.
- When disabled, fee multiplier is always `1.0` (no effect).

#### Feature 6: Gas Pool (Per-Ledger Anti-Spam)
- Each custom ledger has a PMS gas pool. Every transaction consumes gas. Depleted pool = ledger rejects transactions (HTTP 402).
- Main ledger is exempt (no gas check).
- **Config:** `gas_per_tx = "0.001"` and `gas_pool_min_balance = "10.0"` in `[fees]` (default: not set = disabled).
- **API endpoints:**
  - `POST /admin/gas-pool/deposit` — Deposit PMS into a ledger's gas pool.
  - `POST /admin/gas-pool/withdraw` — Withdraw from gas pool.
  - `GET /v1/gas-pool/{ledger_id}` — View pool balance and stats.
- Gas pools are auto-created (balance=0) when a ledger is created.

#### Subscription (Removed)
- Ledger subscription (annual fee) was removed from the engine. This is a billing/business concern better handled at the dashboard/application layer, not in the protocol engine.

### New Crates
- **`pms-types-economics`**: Shared types (`GasPool`, `FeeBurnResult`, `DynamicFeeInfo`).
- **`pms-economics`**: Pure business logic (fee burn, gas pool, storage fee, dynamic fee). No I/O dependencies.

### Storage
- **New RocksDB column families:** `gas_pools`, `ledger_subscriptions` (inert, kept for migration compatibility).
- **Migration 7 → 8** (`mig_7_to_8`): Creates new CFs. Auto-applied on startup.
- **Schema version:** `CURRENT_VER` 7 → 8.

### Gateway
- **refactor(gateway)**: Replaced ~100 explicit proxy routes with catch-all fallback. New Engine endpoints are now automatically proxied without gateway code changes.
- Only 7 routes remain explicit: 5 using `/internal/*` API with typed payloads + 2 SSE stream endpoints requiring streaming proxy.

### Version Bumps
- Software: `0.2.7` → `0.3.0` (MINOR — new features)
- Schema DB: `7` → `8` (new CFs)
- API: `1` → `2` (new endpoints)
- DAG Protocol: `1.1.0` → `1.2.0` (backward-compatible)

---

## [0.2.7] - 2026-03-14

### Fixed
- **fix(rocks)**: Pin L0 index/filter blocks in cache + increase shared LRU block cache 256MB → 512MB. v0.2.6's bloom filters on 31 CFs caused catastrophic cache thrashing (index+filter blocks from 31 CFs competed for 256MB), collapsing TPS to 0-2. `set_pin_l0_filter_and_index_blocks_in_cache(true)` prevents L0 eviction.

---

## [0.2.6] - 2026-03-14

### Performance
- **perf(rocks)**: Apply bloom filters (10-bit) + shared block cache to ALL 31 column families. Only 7/31 CFs had bloom filters — the other 24 (including hot-path CFs: `children_count`, `children_set`, `addr_activity`, `node_block_counts`, `tx_applied`) used `Options::default()`. As DB grew, point lookups on unfiltered CFs required scanning multiple SSTable levels, causing progressive TPS degradation (170 → 120 over ~1h).

---

## [0.2.5] - 2026-03-13

### Performance
- **perf(storage)**: Fix RocksDB L0 write stall causing TPS cliff 120 → 20. Root cause: default L0 thresholds (slowdown=20, stop=24) hit after ~40-60 min of sustained 120 TPS.
  - Centralize DB tuning in `apply_db_tuning()` (eliminates `new()`/`open_db_multi_prefix()` drift)
  - Raise L0 thresholds: slowdown 20 → 40, stop 24 → 56
  - `max_subcompactions=3` for parallel L0 draining, `max_background_jobs` 4 → 6
  - Rate-limit `compact_all()` with 200ms inter-CF pause, move interval 1h → 6h
  - Amortize `trim_tips()` every 64 blocks via `maybe_trim_tips()` + `persist_counter`
  - Stale-while-revalidate `top_tips()` cache (5s stale window)

---

## [0.2.4] - 2026-03-13

### Fixed
- **fix(storage)**: `ensure_column_families()` at bootstrap — creates any missing CFs from `CF_NAMES` at runtime. Fixes testnet crash with `missing column family eden:contracts` on DBs created before the smart contract system.
- **fix(storage)**: `add_ledger()` now checks ALL CFs unconditionally (not just `blocks`).

---

## [0.2.3] - 2026-03-13

### Performance
12 hot-path optimizations eliminating allocations, lock contention, and redundant RocksDB I/O:

**Storage layer:**
- Cache CF name strings in HashMap (eliminates ~11 `format!()` per block)
- Cache `RuntimeConfig` with 500ms TTL + write-through invalidation
- In-memory `DashSet` for frozen addresses (skip RocksDB on `is_frozen`)
- RocksDB write buffer 32MB×2 → 128MB×3 (reduce flush stalls)
- `persist_final()` → WriteBatch (batch all finalized block writes)
- `trim_tips()` → WriteBatch (batch tip deletes)
- `make_utxo_key()` pre-allocated buffer with `itoa` (no `format!()`)
- `multi_get_cf()` for parent counts in `append_block_atomic()`

**P2P / server layer:**
- `inflight_fetch`: Tokio `Mutex<HashMap>` → `DashMap` (lock-free)
- `seen_invs`: `tokio::sync::Mutex` → `std::sync::Mutex` (no `.await` in CS)
- Broadcast: serialize once as `Arc<str>`, clone ref per peer

**DAG (RAM layer):**
- `find_tips()`: `.take(MAX_TIPS_CAP)` before collect (avoid full scan)

---

## [0.2.1] - 2026-03-13

### Added
- **Smart Contract System**: Rule-based declarative contracts (coordinator-only).
  - `ContractScope::Global` / `ContractScope::Ledger(vec)` for per-ledger targeting
  - Triggers: `OnNftBurn`, `OnTokenBurn`; Formulas: `FixedRate`, `AttributeFormula`, `FixedAmount`
  - Contract engine, RocksDB storage (`contracts` CF), admin API endpoints (`POST/GET/DELETE /admin/contracts`)
  - Integrated into all 3 NFT burn handlers via `evaluate_contracts_after_burn()`
  - DB schema v5 → v6 migration for `contracts` CF

### Performance
- **fix(perf)**: Resolve TPS degradation with 2.6M+ blocks (4 critical bottlenecks):
  - K-depth finalization: O(N×BFS) → O(k²) incremental via `ancestors_within_depth()`
  - Cache `Settings` + `WireMeta` in `CoreAdapter` — removes `load_config()` disk I/O per block
  - `GetTips` frequency 200ms/1024 → 10s/64 — reduces network amplification
  - Prune finalized `HashSet` in `prune_oldest()` — bounds RAM (~340MB saved)

---

## [0.2.0] - 2026-03-12

Cumulative release covering all work from initial deployment (2026-01-08) through pre-versioning era. Includes architecture rewrite, feature additions, performance optimizations, and production hardening.

### Architecture

- **Centralized Private DAG**: Major rewrite from decentralized multi-writer to centralized single-writer model. Coordinator is the sole block authority — no PoW, no trustless consensus.
- **Single Writer Protocol Lock** (`enforce_single_writer` in `[validation]` config): linear chain (1 parent per block except genesis), immediate finality, coordinator signature verification at protocol level.
- **Engine + Gateway separation**: 4-process VPS architecture (Engine, Gateway, Prometheus, Caddy) with internal API.
- **Multi-ledger system**: Multiple isolated ledgers on a single engine instance. Each ledger has its own DAG, UTXO set, and block history.

### Features

#### Security & Authentication
- **API key authentication**: SHA-256 hashed keys, constant-time comparison, granular per-group/per-endpoint scopes (`wallet`, `nft`, `dag`, `supply`, `tokens`, `history`, `coordinator`, `*`). Admin CRUD: `POST/GET/DELETE /admin/api-keys`.
- **Admin Config API**: `GET/POST /admin/config` for runtime configuration changes (fee_rate_bps, base_fee, coordinator_fee_bps, treasury_fee_bps, min_pow_bits, max_mint_per_block, mint_enabled).
- **RuntimeConfig**: Simplified for Single Writer mode — `coordinator_fee_bps` (67%) + `treasury_fee_bps` (33%) with `validate_fee_split()`.

#### Treasury & Fees
- **Encrypted Reward Blocks**: Each reward output encrypted individually for [recipient, coordinator]. X25519 key extracted from bech32 address.
- **Automated Fee Distribution**: Background task with configurable `distribution_interval_sec`. Coordinator auto-creates Mint blocks for accumulated fees and burn refunds.
- **Fee system reinforcement**: Basis-point precision (bps), ascending tier validation, immediate mint fee enforcement.
- **Fee precision fix**: Fees could exceed 8 decimals. Enriched `Amount` type with arithmetic operators that auto-round to 8 decimals.

#### NFTs & Tokens
- **NFT Burn-to-Mint & Refund**: "Cube" NFTs trigger refund `(weight * size * density) / 100`.
- **NFT queries**: `GET /v1/wallet/:address/nfts` with dual-write to `nfts` and `nfts_by_owner` CFs.
- **Coordinator-Only Minting**: `Mint` action restricted to Coordinator public key.
- **Multi-token**: Custom token creation and management.

#### Wallet & Activity
- **Wallet API**: Return `x25519_sk_hex` in wallet create/restore responses.
- **Per-address activity index** (`addr_activity` CF): O(wallet_blocks) instead of O(total_blocks) for activity queries. Migration 3 → 4.
- **Per-type activity index** (`addr_type_activity` CF): Filtered queries (e.g. `?type=mint`) scan only matching category. 9 categories. Migration 4 → 5.
- **Pre-computed activity items**: `activity_items` CF written at block time. LRU cache (10K entries, 30s TTL). `POST /admin/reindex-activity-items` for backfill.
- **Encrypted activity visibility**: Encrypted payloads now appear in activity feeds with minimal "encrypted" placeholder when no decryption key provided.
- **`POST /v1/tx/prepare`**: Server-side unsigned transaction preparation (UTXO selection, fee calculation).
- **`GET /v1/balance`**: Address-only balance queries without private keys.

#### Bridge & Multi-Ledger
- **Bridge module**: Cross-ledger token transfers with lock/mint mechanism.
- **Compliance module**: KYC/AML compliance framework.
- **Wallet factory**: Custodial wallet creation and management.

#### Simulator
- **Game engine**: Edenite game with cube NFT burn → EDN reward loop.
- **252-agent profiles** (7 types: user, fast, miner, whale, sniper, saver, observer).
- **Coordinator agent**: Sends 0.1-1 PMS per minute using node wallet.
- **OOM prevention**: Bounded channels, cube_registry cleanup, Docker memory limits.

### Performance

#### Lock-Free DAG (2437 → 4028 TPS)
- `ConcurrentDag` with `DashMap<BlockId, Block>` for lock-free storage.
- `DashSet` for concurrent spent outpoint tracking.
- Parents validated via `parents_exist_in_store()` (lock-free).
- IOTA-style genesis bootstrap for `min_parents = 2`.
- **Benchmark: 4028 TPS** (10 workers × 1000 tx, 2.48s, 0 failures).

#### Memory Optimizations
- **LRU UTXO cache**: Replace unbounded HashMap with bounded `LruCache` + RocksDB fallback. Configurable via `max_utxos` (default 500k).
- **UTXO streaming bootstrap**: `stream_all_utxos()` via `sync_channel` (10k buffer). O(buffer_size) instead of O(total_utxos).
- **CompactOutput** (~32 bytes vs ~148 bytes) with `Arc<str>` interning.
- **Address index**: `DashMap<String, DashSet<OutputId>>` for O(k) balance queries.
- **Shared RocksDB block cache**: Single 256MB cache across all CFs (~1.5 GB saved).
- **DAG pruning**: Insertion-order pruning with configurable `max_dag_blocks` (default 50K).

#### Storage Optimizations
- Selective DAG loading at bootstrap: only load newest `max_dag_blocks` from RocksDB (50K reads instead of 971K).
- Activity endpoint: `multi_get_cf` batch reads, bloom filter on `activity_items` CF.
- `native_balance_cache` (`DashMap`) for O(1) `balance_by_address()`.

### Fixed
- **Tipless DAG**: `prune_oldest()` could remove ALL tips, silently blocking fee distribution indefinitely (231k PMS blocked on testnet).
- **RocksDB tip protection**: `trim_tips()` and `remove_tip()` now refuse to delete the last remaining tip (dual-layer consistency with RAM fix).
- **Activity classification**: Sender resolved BEFORE UTXO spend. `transfer_self` only when ALL outputs return to sender.
- **Activity timestamps**: Fix mismatch between `id2ts` and `addr_type_activity` timestamps.
- **Fee output classification**: TxUtxo fee outputs classified as `fee_received` instead of `transfer_in`.
- **UTXO key separator**: Standardize on `#` (was inconsistent between write and parse paths).
- **Encrypted UTXO delta**: Encrypted transactions now correctly update UTXO cache.
- **Coordinator keys**: Allow custom coordinator keys in Testnet/Mainnet mode (don't override).
- **Mint policy**: Reject mint with empty `signer_pubkeys` in Testnet/Mainnet (Dev mode only allows bypass).
- **DAG pruning bugs**: Ghost entries, children_count overwrite, tip-skipping causing unbounded growth, poisoned mutex recovery.
- **Metrics**: DAG Size gauge now reflects actual in-memory count (not cumulative).
- **UTXO deadlock**: Fix lock ordering deadlock in `ShardedUtxoSet` under concurrent load.
- **Silent errors**: Replace `unwrap()` with poison recovery, bound PoW mining loop, replace `eprintln!` with tracing.

### Infrastructure
- **CI/CD**: GitHub Actions with formatting, clippy (hard failure), security audit, Docker builds (engine + gateway).
- **Testnet deployment**: `deploy-testnet.sh` with simulator (97 agents), `upgrade-testnet.sh` for code updates.
- **Production deployment**: `deploy.sh` with pre-flight checks, auto-generated API keys, secure credential backup.
- **Docker**: Memory limits, healthcheck start_period 120s, optimized `.dockerignore`.
- **Dependency security**: `time` crate updated for RUSTSEC-2026-0009.

### Version Bumps
- Software: `0.1.0` → `0.2.0`
- Schema DB: `3` → `7` (migrations 3→4, 4→5, 5→6, 6→7)
- API: `1` (introduced)

---

## [0.1.0] - 2025-12-28

### Added

#### Core DAG
- Block structure with parents, payload, nonce, signature.
- UTXO ledger with atomic updates.
- Tips selection algorithm.
- Orphan block handling with parent dependency tracking.

#### P2P Network
- TLS mutual authentication.
- Gossip protocol for block propagation.
- `GetTips`, `GetBlock`, `Inv`, `Blocks` messages.
- Rate limiting and anti-flood protection.

#### Storage
- RocksDB persistence with column families.
- Background maintenance (flush, compaction).
- Crash recovery support.

#### API
- REST endpoints: `/submit/block`, `/wallet/tx/send`, `/wallet/balance`.
- Health checks: `/live`, `/ready`, `/healthz`.
- Metrics endpoint: `/metrics`.
- Admin routes with token authentication.

#### Wallet
- Ed25519 + X25519 keypair generation.
- Bech32 address encoding.
- Transaction signing.
- UTXO scanning and balance calculation.

#### Security
- Payload encryption (X25519 + ChaCha20-Poly1305).
- Block signature verification.
- IP-based rate limiting.
- Proof-of-Work validation.

### Infrastructure
- Docker multi-node setup (3 nodes + Caddy).
- CI workflow + Grafana dashboard.
- TLS certificate generation scripts.
- Configuration management (TOML).

---

## Version History

| Version | Date | Highlights |
|---------|------|------------|
| 0.7.4 | 2026-04-25 | Production hardening sprint: admin-auth timing-safe + CSRF defense, coordinator key encrypted-at-rest (AES-256-GCM + Argon2id), enriched /healthz (4 real checks), 10 new Prometheus metrics + 5s sampler, trust model documentation, curative tips rebuild (H3), 4 chaos recovery tests, in-band coordinator key rotation |
| 0.7.3 | 2026-04-23 | trim_tips zombie eviction (H3), atomic encrypted UTXO delta (H1), persist hot-path clone reduction (H6) |
| 0.7.2 | 2026-04-22 | Security audit sprint: persist back-pressure, spent-tracking storage fallback, AAD binding, Wallet Debug redaction, bridge multiplier validation, rand unification (no RC), parking_lot migration, treasury misconfig surfacing, freeze/unfreeze race fix |
| 0.3.0 | Unreleased | Economics system (fee burn, gas pools, dynamic fees) |
| 0.2.7 | 2026-03-14 | Pin L0 index/filter + 512MB cache |
| 0.2.6 | 2026-03-14 | Bloom filters on all 31 CFs |
| 0.2.5 | 2026-03-13 | Fix RocksDB L0 write stall (120→20 TPS cliff) |
| 0.2.4 | 2026-03-13 | Fix missing CF crash at bootstrap |
| 0.2.3 | 2026-03-13 | 12 hot-path optimizations |
| 0.2.1 | 2026-03-13 | Smart contracts + TPS fix at scale |
| 0.2.0 | 2026-03-12 | Architecture rewrite, features, 4028 TPS |
| 0.1.0 | 2025-12-28 | Initial DAG implementation |
