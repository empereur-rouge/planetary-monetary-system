# PMS — Spec : Gouvernance timelock

> Spec d'implémentation pour `plan.md` **§4 — Gouvernance** (gouvernée, pas figée).
> Couvre : le modèle proposal→timelock→enact ancré dans le DAG, les paliers
> d'impact (7/15/45 j), l'asymétrie *tighten-now / loosen-later*, les contrôles
> d'urgence qui restent instantanés, la migration du couloir d'émission sous
> gouvernance, la transparence (annonce + droit de sortie), le plan de test et
> le plan d'implémentation phasé.
>
> **Statut :** ✅ décidé / 🔧 à calibrer. **Ancrage code :** chaque affirmation
> pointe un `fichier:ligne` réel (état v0.14.0). À lire avec
> `pms-spec-emission-budget.md` (le couloir que la gouvernance protège).

---

## 0. Objet & invariant

`plan.md` §4.1 : *« La protection des détenteurs n'est PAS l'immuabilité — c'est
l'impossibilité de changer par surprise. »*

> **Invariant G (no-surprise) :** tout changement de paramètre gouverné est
> **(a) annoncé** (visible publiquement avant effet), **(b) timelocké** (délai
> incompressible avant application, fonction de l'impact), **(c) ancré dans le
> DAG** (horodaté, signé Coordinator, vérifiable a posteriori). Pendant le
> timelock, la communauté a un **droit de sortie informée**.

État actuel à corriger : `admin_update_config`
([admin.rs:549](crates/pms-server/src/admin.rs#L549)) applique tout changement
**instantanément** et **hors DAG** (id synthétique `admin-<ts>`,
[admin.rs:563](crates/pms-server/src/admin.rs#L563), écriture RocksDB directe via
`apply_config_update`). Aucun délai, aucune annonce, aucun bloc. C'est l'inverse
de l'invariant G.

---

## 1. Le modèle — proposal → timelock → enact, tout ancré DAG

```
 PROPOSE (annonce)            TIMELOCK (délai)              ENACT (effet)
 ────────────────            ────────────────              ─────────────
 GovernanceProposal          le changement est             GovernanceEnact
 { update, tier,             VISIBLE mais NON appliqué.     { proposal_id }
   reason,                   GET /v1/governance/pending     → apply_config_update
   announced_at,             expose l'annonce + le          (le VRAI changement)
   enact_after }             enact_after. Sortie informée.  rejeté si now<enact_after
   → bloc DAG signé                                          → bloc DAG signé
```

- **Deux blocs**, conformes à la règle « toute mutation d'état = bloc DAG » :
  `GovernanceProposal` (annonce, n'applique rien) et `GovernanceEnact` (applique
  le `ConfigUpdate`). Le seam d'application existe déjà : `PlainPayload::ConfigUpdate`
  est appliqué dans le hot path ([persist.rs:419](crates/pms-core/src/net_adapter/persist.rs#L419))
  via `apply_config_update` keyé par le **vrai** `wb.id` — `GovernanceEnact`
  réutilise ce chemin.
- **Le timelock est appliqué à la VALIDATION de l'enact** : un `GovernanceEnact`
  dont la proposition a `now < enact_after` est **rejeté** (`PutResult::Rejected`),
  comme un UTXO time-locké refuse la dépense
  ([transactions.rs:140](crates/pms-core/src/validations/transactions.rs#L140)).
  Le délai est donc inviolable au niveau protocole, pas seulement applicatif.
- **`enact_after = announced_at + tier.duration`** — figé à la proposition,
  dérivé du timestamp du bloc proposal (déterministe, comme l'epoch d'émission).

---

## 2. Paliers d'impact (plan §4.2)

| Palier | Durée ✅ | Exemples de params | Tranche |
|---|---|---|---|
| **Operator** | **7 j** | `fee_rate_bps`, `base_fee`, fee tiers, `min_pow_bits`, `max_mint_per_block` (hausse) | calibrage |
| **Policy** | **15 j** | ouvrir/changer une voie de mint, `burn_rate_bps`, `target_rate` d'émission, distribution des fees | politique |
| **Constitution** | **45 j** | **le couloir 10 % (ceiling/floor)**, le pouvoir de mint, qui gouverne | inviolable |

- Le **palier est porté par la proposition** (`tier: GovernanceTier`), pas
  déduit automatiquement — l'opérateur déclare l'impact, et la **validation
  impose un palier MINIMUM par paramètre** (table param→palier-min) pour qu'on ne
  puisse pas faire passer un changement de couloir en « operator 7 j ».
- 🔧 La table param→palier-min exacte (quel `ConfigUpdate` exige quel palier
  minimum) est à figer — défaut sûr : tout ce qui touche le couloir d'émission
  ou le pouvoir de mint = **Constitution (45 j)** ; les fees = Operator ; ouvrir
  une voie / burn rate / target rate = Policy.

---

## 3. Asymétrie *tighten-now / loosen-later* + contrôles d'urgence

> **Principe ✅ :** **resserrer** (réduire un risque) peut être rapide/instantané ;
> **desserrer** (augmenter un risque pour les détenteurs) exige le timelock plein.
> C'est le pattern standard (et la sécurité réelle) : on n'attend pas 45 j pour
> stopper une fuite, mais on attend 45 j pour s'autoriser à émettre plus.

| Action | Direction | Timelock |
|---|---|---|
| `mint_enabled = false` (halte mint) | resserre | **instantané** (urgence) |
| `mint_enabled = true` (reprise) | desserre | palier du paramètre |
| baisser `annual_ceiling_percent` | resserre | court / instantané |
| **hausser** `annual_ceiling_percent` | desserre | **Constitution 45 j** |
| `max_mint_per_block` ↓ | resserre | instantané |  `max_mint_per_block` ↑ | desserre | Operator 7 j |

**Contrôles d'urgence qui restent INSTANTANÉS (jamais timelockés)** :
- **Read-only mode** ([read_only.rs](crates/pms-server/src/read_only.rs)) — soupape
  RAM (mémoire/disque/rocksdb/manuel), pas de la config gouvernée. Inchangé.
- **`mint_enabled = false`** — kill-switch d'urgence. ⚠️ **Aujourd'hui dormant** :
  le champ existe ([runtime.rs:111](crates/pms-config/src/runtime.rs#L111)) mais
  **n'est lu nulle part** (aucun handler de mint ne le vérifie). À **câbler** dans
  ce chantier (sinon le kill-switch ne tue rien) — check dans `EmissionGate::reserve`
  + les voies de mint natif.

---

## 4. Ancrage DAG — `GovernanceProposal` / `GovernanceEnact`

Modèle de référence : `ReserveSnapshot` (bloc signé Coordinator, CF dédié).

1. **Payloads** ([payload.rs](crates/pms-types-payload/src/payload.rs)) :
   ```
   GovernanceProposal { proposal_id, update: ConfigUpdate, tier: GovernanceTier,
                        reason: String, announced_at_ms: u64, enact_after_ms: u64 }
   GovernanceEnact   { proposal_id, reason: String }
   GovernanceCancel  { proposal_id, reason: String }   // annule pendant le timelock
   ```
   `proposal_id` = SHA-256(update + tier + announced_at) — déterministe.
2. **Autorité** ([authority.rs:203](crates/pms-core/src/validations/authority.rs#L203))
   : les trois sont **coordinator-only** (`require_coordinator`) + ajout aux match
   exhaustifs (le compilateur force). Dans le modèle corporate (plan §0), c'est
   l'entreprise qui gouverne — mais sous processus public.
3. **Validation du timelock** (hot path persist) :
   - `GovernanceProposal` : `enact_after_ms == announced_at_ms + tier.duration` ;
     `tier ≥ palier-min(update)` (table §2) ; `update.validate()` (cohérence fees).
   - `GovernanceEnact` : la proposition existe, statut `Pending`, et
     **`now_ms ≥ proposal.enact_after_ms`** sinon `Rejected("timelock not elapsed")`.
   - `GovernanceCancel` : proposition `Pending` (on ne peut pas annuler un enact).
4. **Application** : à la persistance d'un `GovernanceEnact`, appliquer
   `proposal.update` via `apply_config_update(update, &enact_block_id, ts)`
   ([config_store.rs:24](crates/pms-storage/src/config_store.rs#L24)) — keyé par le
   **vrai** bloc enact (traçabilité). Marque la proposition `Enacted`.
5. **CF `governance_proposals`** (`proposal_id → {update, tier, announced_at,
   enact_after, status}`). Ajouter aux **deux** listes CF de
   [store.rs](crates/pms-storage/src/rocks_store/store.rs) (`required` ~:306 ET
   `CF_NAMES` ~:467) + `mig_X_to_Y()` + bump `CURRENT_VER`. (Un nouveau CF est
   justifié ici — état indexé, requêtable, ≠ snapshot ponctuel.)
6. **Le seam HTTP** : `admin_update_config` ([admin.rs:549](crates/pms-server/src/admin.rs#L549))
   ne s'applique plus instantanément → il **PROPOSE** (forge un `GovernanceProposal`,
   tier déclaré dans la requête). L'application instantanée hors-DAG est **supprimée**
   (sauf urgence §3). Nouveaux endpoints §7.

---

## 5. Migration du couloir d'émission sous gouvernance

Le couloir (`annual_ceiling_percent`, `annual_floor_percent`,
`annual_inflation_percent`, `emission_epoch_duration_sec`) est **boot-only** dans
`FeesSettings` ([config.rs:845](crates/pms-config/src/config.rs#L845)). Pour le
gouverner :
1. Le **mirrorer dans `RuntimeConfig`** + un `ConfigUpdate::SetEmissionCorridor
   { ceiling, floor, target, epoch_duration }`.
2. `EmissionGate` lit le couloir depuis `RuntimeConfig` (runtime) au lieu de
   `FeesSettings` (boot) — fallback sur `FeesSettings` au premier boot (seed).
3. `SetEmissionCorridor` est **Constitution (45 j)** dans la table §2 — et la
   **hausse** du ceiling est strictement loosen (45 j), la baisse peut être courte.

Cette migration rend enfin vraie la promesse `plan.md` §2.1 (« règle inviolable
que l'opérateur ne peut pas franchir par surprise ») : le couloir n'est modifiable
que par un processus annoncé + timelocké de 45 j, tracé dans le DAG.

---

## 6. Enactment — tâche d'auto-enact

Pattern : `spawn_reserve_snapshot_task`
([tasks.rs:190](crates/pms-server/src/api/tasks.rs#L190)).

- `spawn_governance_enact_task(state)` : `tokio::interval`, **check
  `state.read_only.is_armed()` → `continue`** (règle CLAUDE.md : tâche de fond
  produisant des blocs), scanne le CF `governance_proposals` pour les `Pending`
  dont `enact_after ≤ now`, et forge un `GovernanceEnact` pour chacun.
- L'enact est **idempotent** : si la proposition est déjà `Enacted` (course
  tâche/admin), le second enact est rejeté (statut ≠ Pending).
- 🔧 Intervalle de scan : défaut 1 h (le timelock est en jours — pas besoin de
  finesse). Manuel `POST /admin/governance/enact/{id}` aussi possible (l'opérateur
  applique dès l'expiration sans attendre le tick).

---

## 7. Transparence — annonce + droit de sortie

| Méthode | Path | Rôle |
|---|---|---|
| `POST` | `/admin/governance/propose` | Crée un `GovernanceProposal` (body `{update, tier, reason}`). Remplace l'application instantanée. `admin_writable`. |
| `GET` | `/v1/governance/pending` | **PUBLIC** — liste les propositions `Pending` (update, tier, enact_after, reason). C'est **l'annonce** : tout détenteur voit ce qui va changer et quand (droit de sortie). |
| `GET` | `/v1/governance/history` | **PUBLIC** — propositions enacted/cancelled (audit). |
| `POST` | `/admin/governance/enact/{id}` | Force l'enact après expiration (sinon la tâche le fait). `admin_writable`. |
| `POST` | `/admin/governance/cancel/{id}` | Annule une proposition `Pending`. `admin_writable`. |

L'exposition **publique** de `/v1/governance/pending` est le cœur du « no-surprise » :
le timelock ne protège que s'il est **visible**.

---

## 8. Plan de test (anti-faux-tests CLAUDE.md)

| # | Test | Assert (golden) |
|---|---|---|
| G1 | enact_after dérivé du tier | proposal Constitution → `enact_after == announced_at + 45j` (en ms) |
| G2 | **timelock inviolable** | `GovernanceEnact` à `now < enact_after` → `PutResult::Rejected("timelock")` ; le param est INCHANGÉ |
| G3 | enact après expiration | à `now ≥ enact_after`, enact applique → `RuntimeConfig` reflète l'update |
| G4 | **palier minimum imposé** | proposer une hausse de ceiling en tier Operator → **rejeté** (exige Constitution) |
| G5 | tighten-now | `mint_enabled=false` (urgence) → instantané, pas de timelock ; reprise → timelocké |
| G6 | kill-switch câblé | `mint_enabled=false` → `EmissionGate::reserve` (et voies natives) refusent le mint |
| G7 | cancel | `GovernanceCancel` sur Pending → statut Cancelled, jamais appliqué ; cancel sur Enacted → rejeté |
| G8 | DAG-ancré + public | la proposition est un bloc signé Coordinator ; `GET /v1/governance/pending` la liste |
| G9 | couloir gouverné e2e | proposer ceiling 10→20 % (Constitution) → avant 45 j le budget reste à 10 % ; après enact, 20 % |

G2 + G4 + G6 sont non-négociables (l'inviolabilité du délai, du palier, et du kill-switch).

---

## 9. Plan d'implémentation (commits atomiques)

1. **Payloads + CF + validation timelock.** `GovernanceProposal/Enact/Cancel`,
   `GovernanceTier`, CF `governance_proposals` (+ `CURRENT_VER`/migration),
   autorité coordinator-only, validation `enact_after`/palier-min/timelock-elapsed.
   Tests G1, G2, G7, G8. *Pas encore branché sur l'admin.*
2. **Apply + enact task + endpoints.** Apply `ConfigUpdate` sur enact (réutilise
   `apply_config_update`), `spawn_governance_enact_task` (+ read-only guard),
   endpoints propose/pending/history/enact/cancel. Rewire `admin_update_config`
   → propose. Tests G3, G8.
3. **Asymétrie + kill-switch.** Table param→palier-min, direction tighten/loosen,
   câbler `mint_enabled` (le rendre enforcé dans `EmissionGate`/voies). Tests G4, G5, G6.
4. **Couloir sous gouvernance.** `SetEmissionCorridor` + miroir RuntimeConfig +
   `EmissionGate` lit le runtime. Test G9. (Dépend de la spec emission-budget.)

Bumps : `Cargo.toml` (MINOR), `DAG_VERSION` (MINOR — nouveaux payloads additifs,
pas de wipe), `CURRENT_VER` (nouveau CF), `API_VERSION` (nouvelles routes).

---

## 10. Décisions ouvertes (🔧)

| # | Décision | Reco par défaut |
|---|---|---|
| D1 | Table exacte param→palier-min | couloir/pouvoir-de-mint = Constitution ; fees = Operator ; voie/burn/target = Policy |
| D2 | Durées exactes (7/15/45 j fermes ?) | oui (plan §4.2 ✅) ; testnet : durées raccourcies (ex. minutes) pour les tests |
| D3 | Auto-enact vs manuel-only | **les deux** : tâche auto (tick 1 h) + endpoint manuel |
| D4 | Qui peut annuler une proposition `Pending` | l'opérateur (admin) ; plus tard : vote consultatif (plan §4.3) |
| D5 | `admin_update_config` instantané : supprimé ou gardé pour urgence ? | supprimé pour les params gouvernés ; un chemin d'urgence séparé pour le *tighten* instantané (mint disable) |
| D6 | Votes (consultatifs) | hors scope de cette spec — plan §4.3, après le timelock |

---

## Références croisées
- `plan.md` §4 (gouvernance), §2.1 (règle inviolable que le couloir protège).
- `pms-spec-emission-budget.md` §6 (la tension `admin_update_config` instantané que cette spec résout).
- Code : `admin.rs` (seam HTTP), `runtime.rs` (ConfigUpdate/RuntimeConfig),
  `config_store.rs` (apply_config_update + config_history), `persist.rs:419`
  (ConfigUpdate DAG path = le seam d'apply), `reserves.rs` (template bloc signé),
  `tasks.rs` (template tâche de fond), `read_only.rs` (urgence instantanée).
