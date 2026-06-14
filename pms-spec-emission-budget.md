# PMS — Spec : Budget d'émission partagé

> Spec d'implémentation pour `plan.md` **§3.1 — Le budget partagé (la règle non-négociable)**.
> Couvre : périmètre exact, modèle de période, calcul du budget, compteur persisté, point
> d'enforcement (résolution du TOCTOU), comportement à l'épuisement, paramètres/gouvernance,
> métriques, plan de test et plan d'implémentation phasé.
>
> **Statut des valeurs :** ✅ décidé / 🔧 à calibrer — même convention que `plan.md`.
> **Ancrage code :** chaque affirmation pointe un `fichier:ligne` réel (état v0.11.3).

---

## 0. Objet & invariant

L'invariant non-négociable du `plan.md` (§3.1) :

> **Toutes les voies de mint puisent dans le MÊME budget d'émission par période. Total émis ≤ budget, sinon rejet / file / dégradation.**

Sans ce budget commun, N voies de mint = N planches à billets. Avec lui, on peut ouvrir
autant de voies qu'on veut sans risque inflationniste. Cette spec rend le budget **inviolable
par le code**, pas seulement par convention.

Deux propriétés de sécurité à garantir formellement :

| Propriété | Énoncé | Risque si violée |
|---|---|---|
| **P1 — Plafond** | Sur toute période *p*, `Σ émission_p ≤ budget_p` | Sur-émission silencieuse (dévaluation) |
| **P2 — Exactement-une-fois** | Chaque bloc de mint décrémente le budget exactement une fois, y compris après crash/replay/re-gossip | Double-décompte (sous-émission) ou double-crédit (sur-émission) |

---

## 1. Périmètre — ce que le budget gate (et ce qu'il ne gate PAS)

**Définition opérante de l'« émission » :** création de PMS natif *qui n'existait pas auparavant*
sur le **ledger main**. C'est l'augmentation de la supply native (`asset_id = None`) par un mint.

> ⚠️ **Distinction de correction critique :** « émission » ≠ « redistribution ». La distribution
> de fees re-créé des UTXOs PMS, mais le PMS correspondant a déjà été collecté auprès des
> utilisateurs (déduit de leurs sorties de tx, accumulé dans la `FeePool`). C'est **supply-neutre
> sur le cycle**. La compter dans le budget la bloquerait à tort (les payouts de fees échoueraient
> dès le budget épuisé) ET fausserait la compta. Elle est donc **hors budget**.

### 1.1 Voies GATÉES (décrémentent le budget) — PMS natif, ledger main

| Voie | Chemin code | Trigger | Statut | Notes |
|---|---|---|---|---|
| Baseline taux cible (résidu, §2.3) | `perform_daily_inflation_mint` — [inflation.rs:20](crates/pms-server/src/fee_distribution/inflation.rs#L20) | tâche `spawn_inflation_mint_task` ([tasks.rs:129](crates/pms-server/src/api/tasks.rs#L129)) | ✅ Phase 1 | Aujourd'hui : `supply × rate / 365` non borné par un couloir |
| Voie A — on-ramp fiat→PMS | *net-new* (Phase 1) | route admin/SDK on-ramp | ✅ cible Phase 1 | À créer ; doit décrémenter le budget |
| Voie B — pont scrip→PMS | réutilise `BridgeMint` — [pms-bridge/src/engine.rs:291](crates/pms-bridge/src/engine.rs#L291), **vers main, asset natif** | route `/admin/bridge/transfer` | 🔧 Phase 2 | Mint asymétrique (taux R) ; R dégrade quand le budget se vide (§5) |
| Faucet (main, natif) | `faucet_mint` — [wallet_factory.rs:257](crates/pms-server/src/api_fn/wallet_factory.rs#L257) | `POST /admin/faucet` | ✅ | Dev/testnet uniquement (rejeté en prod) — gater pour cohérence des tests |

### 1.2 Voies HORS budget (ne décrémentent PAS)

| Chemin | Raison | Réf |
|---|---|---|
| Distribution de fees | Recyclage de fees déjà collectées, supply-neutre | `perform_fee_distribution` — [distribute.rs:33](crates/pms-server/src/fee_distribution/distribute.rs#L33) |
| Mint de **token nommé** (asset_id ≠ None) | Hors PMS natif ; borné par son propre `max_supply` | `admin_mint_token` — [token.rs:300](crates/pms-server/src/api_fn/token.rs#L300) |
| Mint sur **ledger custom** (natif du ledger) | Hors budget PMS ; économie propre au ledger | `faucet_mint` / `BridgeMint` ciblant un ledger ≠ main |
| Mint NFT | N'augmente pas la supply d'un asset (registre d'ownership chiffré) | `mint_nft` — [nft.rs:232](crates/pms-server/src/api_fn/nft.rs#L232) |
| `Reward` déprécié | Mort (jamais appelé) | `create_reward_block` — [fee_accumulation.rs:71](crates/pms-server/src/api_fn/tx_helpers/fee_accumulation.rs#L71) |

> **Règle d'extension (à inscrire dans `CLAUDE.md` une fois la feature mergée) :** toute
> nouvelle route/tâche qui produit un mint de PMS natif sur le ledger main **DOIT** passer par
> le gate d'émission (§4). Comme pour la catégorisation read-only, c'est binaire : *gated* (crée
> du PMS natif sur main) ou *hors budget* (tout le reste). L'omettre rouvre une planche à billets.

---

## 2. Modèle du budget

### 2.1 La période (epoch) — dérivée des timestamps de bloc

**Problème constaté :** il n'existe AUCUN marqueur de période persisté. `spawn_inflation_mint_task`
est un `tokio::time::interval` ([tasks.rs:141](crates/pms-server/src/api/tasks.rs#L141)) remis à
zéro à chaque restart — un nœud qui redémarre toutes les < `interval_sec` ne minterait jamais ;
la cadence ne survit pas au reboot. De plus le diviseur `/365` est **hardcodé** et découplé de
l'intervalle réel ([inflation.rs:56](crates/pms-server/src/fee_distribution/inflation.rs#L56)) :
en testnet (intervalle 120 s) le moteur minte « une journée d'inflation » toutes les 2 min.

**Décision ✅ :** l'epoch est **dérivé du timestamp du bloc**, pas du timer.

```
epoch_id(block_ts_ms) = block_ts_ms / EPOCH_DURATION_MS        // division entière
```

- `EPOCH_DURATION_MS` 🔧 : défaut **86 400 000** (1 jour) en mainnet ; surchargé court en testnet.
- Le timestamp de bloc est posé par le Coordinator (single-writer custodial → autoritaire).
- **Conséquence clé :** l'epoch est déterministe et reconstructible au replay depuis le DAG seul —
  pas de « last_mint_ts » volatile à persister séparément. Deux nœuds rejouant le même DAG
  calculent le même découpage en epochs.

> Le timer tokio reste le *déclencheur* de la baseline (« réveille-toi, vérifie s'il reste du
> budget cette période »), mais il n'est plus l'*autorité* sur ce qu'est une période. Cela
> corrige aussi le découplage `/365` : l'émission devient proportionnelle au temps réel écoulé.

### 2.2 Calcul du budget par période

```
supply_ref   = circulating_supply(asset=None)          // RAM cache O(1), utxo.rs:468
target_rate  = annual_target_rate                       // ✅ 2%/an (plan §2.3)
ceiling_rate = annual_ceiling_rate                      // ✅ 10%/an (plan §2.3) — couloir dur
floor_rate   = annual_floor_rate                        // 🔧 plancher anti-asphyxie (plan §8.1)

frac_year    = EPOCH_DURATION_MS / MS_PER_YEAR           // part d'année que couvre la période
budget_p     = supply_ref × clamp(effective_rate, floor_rate, ceiling_rate) × frac_year
```

- `effective_rate` = `target_rate` en **Phase 1** (taux cible seul, plan §2.2 « démarré avec un »).
  En Phase 2/3 : `target_rate` ajusté par Signal 1 (burn) et Signal 2 (activité) — §2.4.
- **Le `clamp` au `ceiling_rate` est le couloir dur du `plan.md` §2.3.** Même si la config
  pousse `effective_rate` à 50 %, `budget_p` est plafonné à 10 %/an-équivalent. C'est la
  matérialisation en code de la promesse « règle inviolable que l'opérateur ne peut pas franchir
  par surprise » (plan §2.1). *(Rendre le `ceiling_rate` lui-même immuable-sauf-timelock est
  l'objet de l'option 3 — gouvernance ; cette spec applique le couloir tel que configuré.)*
- `supply_ref` 🔧 : **décision ouverte** — supply au *début* de l'epoch (gelée, plus prévisible)
  vs supply *courante* à chaque mint (réactive). Recommandation : **figée au début de l'epoch**
  (calculée à la première émission de la période, stockée avec le compteur §3) → budget stable
  et auditável sur toute la période, pas de cible mouvante.

### 2.3 La baseline inflation comme **résidu** (pas une voie additionnelle)

Le `plan.md` §2.2 est explicite : *« l'émission n'est PAS la somme de trois règles… c'est un
objectif »*. Donc la baseline ne s'**additionne** pas aux voies A/B/C/D : elle **complète** jusqu'à
la cible.

```
résidu_p = max(0, budget_p − émis_par_les_voies_durant_p)
```

À chaque tick de `spawn_inflation_mint_task`, on minte `min(résidu_p, budget_restant_p)` vers
coordinator/treasury (préserve le comportement actuel de répartition creator/treasury/burn).

- **Avantage :** l'émission totale par période **converge vers la cible** quelle que soit
  l'activité des voies. Si les voies ont déjà émis tout le budget, la baseline minte 0. Si aucune
  voie n'est active (lancement, §3.2 du plan), la baseline porte ~tout le budget — exactement le
  comportement v0.11.3, mais désormais **borné par le couloir**.
- 🔧 **Alternative de politique** (`plan.md` §8.5 « allocation du budget entre les 4 voies ») :
  sous-allocations fixes par voie au lieu du résidu. Le résidu est recommandé pour le lancement
  (1 seule voie + baseline) ; les sous-allocations deviennent utiles quand 3-4 voies se
  concurrencent pour le budget.

### 2.4 Hooks Signal 1 / Signal 2 (Phase 2/3)

`effective_rate` est une fonction pure laissée extensible :

```
effective_rate = base_target
               + k_burn × (burn_rate_observé − burn_rate_neutre)        // Signal 1 (Phase 2)
               + k_activity × activity_index                            // Signal 2 (Phase 3)
```

- En Phase 1, `k_burn = k_activity = 0` → `effective_rate = base_target`. Les hooks existent dans
  la signature mais sont neutres, conformément au déploiement séquencé du `plan.md` §2.2.
- **Tension à trancher (cf. mon analyse §3 du review) :** le coefficient `k_burn` *est* la
  politique. `k_burn` tel que la compensation = 100 % du burn → masse stable, jamais déflationniste.
  `k_burn < 100 %` → net déflationniste mais la masse se contracte sous forte activité. On ne peut
  pas tenir les deux promesses du plan §2.4 simultanément ; choisir `k_burn` 🔧.

---

## 3. Le compteur — persistance, atomicité, replay

### 3.1 État persisté

Un **singleton** par ledger (seul *main* en a un en Phase 1) :

```rust
struct EmissionEpochState {
    epoch_id: u64,        // dérivé du dernier timestamp de bloc de mint
    supply_ref: Decimal,  // supply figée au début de l'epoch (§2.2)
    budget: Decimal,      // budget_p calculé une fois au début de l'epoch
    emitted: Decimal,     // cumul émis dans cet epoch (monotone croissant intra-epoch)
}
```

**Stockage 🔧→✅ recommandé : réutiliser un CF existant + clé dédiée**, pas un nouveau CF.
Précédent exact : `reserve_snapshot.rs` ancre `b"last_reserve_snapshot"` dans le CF `last_ms`
existant pour **éviter un nouveau CF + une migration** ([reserve_snapshot.rs:12](crates/pms-storage/src/rocks_store/reserve_snapshot.rs#L12)).
Même approche ici : clé `b"emission_epoch_state"` dans le CF `node_fee_pool`, valeur JSON
(encodage cohérent avec `total_burned` / `runtime_config`). **Bénéfice :** aucune des deux listes
de CF de [store.rs](crates/pms-storage/src/rocks_store/store.rs) (la `required` à ~:288 ET la
`CF_NAMES` à ~:449 — qui doivent rester synchrones) n'est touchée, et **pas de bump `CURRENT_VER`
ni de `mig_X_to_Y()`**.

### 3.2 Atomicité — écriture « counter-first » dans le chemin de forge (implémenté)

> **Décision d'implémentation (v0.12.0).** Deux options ont été pesées :
> **(Beta)** écrire le compteur DANS le `WriteBatch` du bloc
> ([dag_storage_impl.rs:828](crates/pms-storage/src/rocks_store/dag_storage_impl.rs#L828)) — atomicité
> parfaite mais met la politique d'émission dans la couche de stockage (`append_blocks_batch`
> devrait parser chaque payload, savoir ce qu'est « un mint gaté », gérer le rollover intra-batch) ;
> **(Alpha)** écrire le compteur de façon synchrone dans le chemin de forge, **avant** de forger le
> bloc, sous le mutex du gate. **Alpha a été retenue** — elle garde la politique d'émission hors du
> stockage et reste **P1-safe**, au prix d'une sous-émission conservatrice possible au crash (jamais
> de dépassement). C'est l'option décrite en §4.2 et prouvée par les tests `t7`/`t10`.

Le compteur est persisté par un `put_cf` synchrone (`record_emission_epoch_state`,
[emission_storage.rs](crates/pms-storage/src/rocks_store/emission_storage.rs)) **sous le mutex du
gate**, dans `EmissionGate::reserve`, **avant** que l'appelant ne forge le bloc. Ordonnancement
« counter-first » : la valeur durable est, à tout instant, `≥` ce qui a réellement été émis (le bloc
n'atteint le disque que *plus tard*, via le pipeline de persist asynchrone). Donc :

- **bloc perdu après réservation** (crash avant que le pipeline ne draine le bloc) ⇒ compteur
  sur-compte ⇒ **sous-émission** conservatrice cet epoch, auto-réparée au rollover. P1 préservé.
- **jamais** de cas « bloc sur disque mais compteur non écrit » (le compteur précède toujours le
  bloc) ⇒ pas de sur-émission. C'est ce que garantit P1.

> Pourquoi pas l'enforcement dans `append_blocks_batch` (le consommateur sérialisé) ? Y rejeter un
> bloc imposerait de rollback un bloc déjà validé, inséré en RAM DAG et broadcasté — bien plus
> salissant que le mutex au forge, pour aucun gain (les mints sont rares). Cf. §4.2.

### 3.3 Replay / boot — lu, jamais re-sommé

- **Au boot :** `EmissionGate::load(&store)` lit `emission_epoch_state` depuis RocksDB → initialise le
  miroir RAM (§4.2). On ne re-somme **jamais** les blocs de mint : le DAG RAM ne garde que les N plus
  récents (`bootstrap_from_store`, [bootstrap.rs](crates/pms-core/src/concurrent_dag/bootstrap.rs)) et
  un scan O(tous-les-blocs) du CF `blocks` est inacceptable sur 20 M blocs. C'est la même philosophie
  que la supply, reconstruite depuis l'état final (CF `utxo`) et non par replay des deltas.
- **Idempotence au replay (P2) — structurelle :** en Alpha, le compteur n'est muté QUE dans le chemin
  de forge (`reserve`/`release`), jamais dans `persist_block`. Un bloc re-gossipé/rejoué passe par
  `persist_block` (dédupé par block-id à [persist.rs:1045](crates/pms-core/src/net_adapter/persist.rs#L1045),
  fix audit S4 v0.11.1) **sans jamais toucher le gate**. Le compteur ne peut donc pas être
  re-décrémenté à l'ingestion ; au boot on lit simplement la valeur stockée (test `t7`).
- **Rollover au boot :** après lecture, si l'horloge a dépassé `epoch_id` stocké, le prochain mint
  recalcule `budget_p` pour le nouvel epoch (§2.2) — pas de catch-up rétroactif (une période ratée
  pour cause de downtime n'est pas « rattrapée », cohérent avec « émission proportionnelle au temps
  *en service* »).

---

## 4. Point d'enforcement — résoudre le TOCTOU

### 4.1 Le problème

Il n'existe **aucun forge lock global**. Le chemin de forge d'un mint (lire tips → lire supply →
construire/signer → `persist_block`) est **par-requête et concurrent**. Le check `max_supply`
actuel est un read-then-write classique ([mint.rs:225-241](crates/pms-core/src/validations/mint.rs#L225-L241),
câblé à [persist.rs:301](crates/pms-core/src/net_adapter/persist.rs#L301)) : deux mints concurrents
lisent chacun `circulating=900`, valident chacun `900+100≤1000`, et persistent → 1100. Le cap
actuel est donc **soft/racy** (toléré car la supply se réconcilie via les UTXOs). Un budget
d'émission **dur** (P1) doit fermer cette fenêtre.

### 4.2 Solution ✅ : un mutex d'émission dédié (les mints sont basse fréquence)

Contrairement aux tx normales (haute TPS), **les mints gatés sont rares** : baseline = 1/période,
on-ramp = cadence humaine, bridge = admin. Les sérialiser ne coûte rien en perf. On introduit sur
`AppState` :

```rust
emission_gate: Arc<tokio::sync::Mutex<EmissionEpochState>>,   // miroir RAM, source de vérité runtime
```

Chemin de toute émission gatée (baseline, voie A, voie B vers main, faucet main) :

```
EmissionGate::reserve (sous le mutex) :
1. lock emission_gate                                   // sérialise tous les forges de mint natif main
2. roll-over si epoch_id(now) > state.epoch_id          // recalcule budget_p (§2.2), emitted=0
3. amount = montant demandé (ou, baseline: résidu = budget_restant)
4. si amount > budget_restant  → EMISSION_REJECTIONS++ + REJET (§5)   // P1 appliqué ici, race-free
5. state.emitted += amount  ;  put_cf(emission_epoch_state)  // réservation + write COUNTER-FIRST
6. si put_cf échoue → state.emitted -= amount + Err(Persist) // rien réservé, l'appelant ne forge pas
7. unlock ; return Reservation { amount }
puis, côté appelant : forge + sign + persist_block(amount) ; si Err → gate.release(amount)
```

- **P1 garanti** : check + réservation + write durable (étapes 4-5) sont atomiques sous le mutex →
  pas de TOCTOU, et la valeur durable précède toujours le bloc (counter-first, §3.2).
- **Miroir RAM = vérité runtime ; RocksDB = ancre de recovery.** Le miroir et la valeur durable sont
  mis à jour ensemble à la *réservation* (étape 5), avant le forge. Au boot, miroir := valeur durable (§3.3).
- **Rollback (étape 7)** : même idée que `FeePool::merge_from` qui restaure les fees sur échec de
  persist. Sans rollback, un échec de persist fuirait du budget (sous-émission).
- **Coexistence read-only** : le gate est un **second** garde indépendant, APRÈS le check
  `state.read_only.is_armed()` existant ([tasks.rs:154](crates/pms-server/src/api/tasks.rs#L154)) —
  les deux font `continue`/rejet. Une période sautée pour read-only n'avance aucun compteur (le
  rollover est piloté par `epoch_id(block_ts)`, pas par le nombre de ticks).

> **Pourquoi pas l'enforcement dans `append_blocks_batch` (le consommateur sérialisé) ?** C'est
> l'autre point naturellement sérialisé, mais y rejeter un bloc signifierait *rollback* d'un bloc
> déjà validé, inséré en RAM DAG et broadcasté — bien plus salissant que le mutex au forge, pour
> aucun gain (les mints sont rares). Le mutex est retenu.

---

## 5. Comportement à l'épuisement

Le `plan.md` §3.1 liste trois options sans trancher. **Décision par voie :**

| Voie | Politique à l'épuisement | Statut |
|---|---|---|
| Baseline (résidu) | N'épuise jamais par construction : minte `min(résidu, budget_restant)`, donc ≤ budget | ✅ |
| Voie A — on-ramp fiat | **Rejet** propre : `503 {"code": 5xxx, "message": "emission budget exhausted"}` + `Retry-After`. L'utilisateur a payé du fiat → la couche applicative met en file/rembourse hors-DAG (jamais de mint au-delà du couloir) | ✅ principe / 🔧 code 5xxx |
| Voie B — pont scrip→PMS | **Dégradation du taux R** : R chute quand `budget_restant` baisse (déjà prévu plan §3.3 « R se dégrade quand le volume de conversion monte »). À budget nul, R→0 = conversion gelée jusqu'au prochain epoch | 🔧 forme de la courbe |

- **Reject = défaut baseline Phase 1.** Pas de file durable (complexité) au lancement ; la mise
  en file est une responsabilité de la couche on-ramp hors-DAG (plan §6 « pricing du contenu,
  on-ramp fiat→PMS » est hors DAG).
- Nouveau code d'erreur `ApiError` dédié (suit la grille `documentation/api/error-codes.md`,
  tranche `5xxx` resource/quota, message public vague anti-enumeration).

---

## 6. Paramètres & gouvernance

| Paramètre | Où | Hot-swap aujourd'hui | Reco |
|---|---|---|---|
| `annual_target_rate` (2 %) | `FeesSettings` ([config.rs:767](crates/pms-config/src/config.rs#L767) `annual_inflation_percent`) | ❌ boot-only, capté au spawn du task | Migrer vers `RuntimeConfig` + variant `ConfigUpdate::SetTargetRate` pour pilotage opérateur |
| `annual_ceiling_rate` (10 %) | *net-new* | — | **Constitution** : timelock 45 j (option 3 gouvernance). Ne PAS rendre hot-swappable instantané |
| `annual_floor_rate` 🔧 | *net-new* | — | Politique : timelock 15 j |
| `EPOCH_DURATION_MS` | *net-new* | — | `RuntimeConfig` ; changement = politique (15 j) |
| `k_burn`, `k_activity` 🔧 | *net-new* (Phase 2/3) | — | `RuntimeConfig` |

> **Tension structurelle (à régler en option 3) :** `admin_update_config`
> ([admin.rs:549](crates/pms-server/src/admin.rs#L549)) applique tout changement **instantanément,
> sans timelock**. Mettre le couloir (`ceiling`/`floor`) sous timelock contredit ce hot-swap
> instantané — c'est précisément le travail de l'option 3 (gouvernance). Pour cette spec, le
> couloir est appliqué tel que configuré ; le rendre *non-modifiable-par-surprise* est l'étape
> suivante. En attendant, garder `ceiling_rate` en `FeesSettings` boot-only (un changement exige
> un redéploiement tracé) est un garde-fou intérimaire acceptable.

---

## 7. Métriques

Aucune métrique mint/supply/inflation n'existe (vérifié dans
[metrics.rs](crates/pms-server/src/metrics.rs) et `pms-core/src/metrics.rs`). Net-new, suivant le
pattern `Lazy<Gauge>` + `.set()` depuis le sampler ([tasks.rs](crates/pms-server/src/api/tasks.rs)
`sample_ledger`). Analogue existant le plus proche : `pms_fees_distributed_total`.

```
pms_emission_budget_total{ledger}      gauge   // budget_p de l'epoch courant
pms_emission_budget_consumed{ledger}   gauge   // emitted
pms_emission_budget_remaining{ledger}  gauge   // budget − emitted   → alerte si proche 0
pms_emission_minted_total{ledger,voie} counter // cumul émis par voie (baseline|onramp|bridge|faucet)
pms_emission_rejections_total{voie}    counter // mints refusés pour budget épuisé
pms_emission_effective_rate{ledger}    gauge   // effective_rate appliqué (audit du couloir)
```

Alerte Prometheus recommandée : `pms_emission_effective_rate > ceiling_rate` = **critique**
(le couloir aurait été franchi — ne doit jamais arriver si le clamp est correct ; c'est un canari
de bug).

---

## 8. Plan de test (règles anti-faux-tests du `CLAUDE.md`)

Tous en sandbox in-process ([dag_sandbox.rs](crates/pms-server/tests/dag_sandbox.rs)),
assertions sur **valeurs golden hardcodées**, exécutés isolés `--nocapture`.

| # | Test | Assert (golden, pas re-dérivé) |
|---|---|---|
| T1 | Budget dérivé du couloir | supply=1000, target=2 %, epoch=1 j → `budget == 1000 × 0.02 / 365 == 0.05479452` (valeur en dur) |
| T2 | **Clamp au plafond (cœur de P1)** | config target=50 %, ceiling=10 % → `budget == 1000 × 0.10/365`, **PAS** 0.50/365. Prouve que le couloir borne, pas la config |
| T3 | Baseline = résidu | voie A émet 0.03 dans l'epoch puis baseline → baseline minte `budget − 0.03`, total période `== budget` exact |
| T4 | **Épuisement → rejet (P1)** | budget rempli par voie A, mint A suivant → assert `code == 5xxx` ET supply **inchangée** (pas de mint fantôme). Asserte la RAISON (pas juste `!is_ok()`) |
| T5 | **TOCTOU fermé** | 2 mints concurrents `tokio::join!` qui sommés dépassent le budget → exactement **un** réussit, l'autre rejeté ; supply finale ≤ budget |
| T6 | **Idempotence/replay (P2)** | persister 2× le même bloc de mint → 2ᵉ = `AlreadyExists`, `emitted` **inchangé** (pas de double-décompte) |
| T7 | **Crash-consistency (P2)** | reconstruire le state depuis le store (pattern `replay_determinism.rs`) → `emission_epoch_state` rechargé == valeur d'avant, supply == golden |
| T8 | Rollover d'epoch | 2 mints à `epoch_id` différents (timestamps espacés) → `emitted` reset entre les deux, chacun borné par son propre `budget_p` |
| T9 | Fees hors budget | distribuer des fees avec budget à 0 → la distribution **réussit** (recyclage, non gatée) ; `emitted` inchangé |
| T10 | Rollback sur échec persist | forcer `persist_block`→Err après réservation → `emitted` revenu à sa valeur d'avant (pas de fuite de budget) |

> T2, T4, T5, T6, T7 sont les tests **non-négociables** : ils prouvent P1 et P2. Un PR qui ne les
> a pas verts ne mérite pas le merge.

---

## 9. Plan d'implémentation (commits atomiques, un par phase — `CLAUDE.md` §3)

> Chaque commit compile, ses tests passent, `/simplify` entre chaque, bump de version.
>
> **Statut : étapes 1-3 + 5 livrées ensemble en v0.12.0** (un seul incrément cohérent — gate +
> baseline + métriques + tests). L'atomicité a été implémentée en **counter-first dans le forge
> path** (Alpha, §3.2), pas dans le `WriteBatch` (Beta) : plus simple, P1-safe, garde la politique
> hors du stockage. Étape 4 (on-ramp) et l'orchestrateur `emit_gated` partagé restent à venir.

1. **Compteur + persistance + calcul + gate baseline (v0.12.0).** `EmissionEpochState`,
   `record/latest_emission_epoch_state` (clé `node_fee_pool/emission_epoch_state`, pattern
   `reserve_snapshot`), `compute_epoch_budget`/`effective_rate_pct` purs, `EmissionGate::reserve`
   (counter-first), baseline en résidu. Tests T1-T8, T10 (verts).
2. **Calcul du budget + couloir.** Fonction pure `compute_epoch_budget(supply, rates, epoch_dur)`
   avec `clamp`. Tests T1, T2, T8. Toujours pas d'enforcement (calcule et expose en métrique).
3. **Gate d'enforcement (mutex) sur la baseline.** `emission_gate` sur `AppState`, brancher
   `perform_daily_inflation_mint` comme résidu (§2.3). Tests T3, T5, T9, T10. **La baseline est
   désormais bornée par le couloir** — c'est le gain de sécurité principal, livrable seul.
4. **Voie A (on-ramp) gatée + rejet.** Nouvelle route on-ramp, code `ApiError` 5xxx, métriques
   rejections. Test T4.
5. **Métriques + alertes** (peut fusionner avec 3). Gauges/counters §7, alerte canari `effective_rate > ceiling`.
6. *(Phase 2)* Voie B (pont scrip, taux R dégradant) + Signal 1 (burn). Hors scope de cette spec
   (option « scrip ledger + farm »).

Bumps attendus : `Cargo.toml` (MINOR — feature), `API_VERSION` (route on-ramp à l'étape 4),
**pas** de `DAG_VERSION`/`CURRENT_VER` (réutilisation de CF, payloads inchangés en Phase 1). La
baseline reste un `PlainPayload::Mint` — pas de nouveau type de bloc.

---

## 10. Décisions ouvertes (🔧)

| # | Décision | Reco par défaut |
|---|---|---|
| D1 | `supply_ref` figée début d'epoch vs courante | **Figée** (budget stable/auditable) |
| D2 | Allocation budget entre voies : résidu vs sous-allocations (`plan.md` §8.5) | **Résidu** au lancement (1 voie + baseline) |
| D3 | `annual_floor_rate` (`plan.md` §8.1) | À décider — 0 % acceptable en Phase 1 (pas de plancher actif) |
| D4 | `EPOCH_DURATION_MS` mainnet | 1 jour (aligne sur `daily_inflation_interval_sec`=86400) |
| D5 | `k_burn` (compensation du burn) — la tension stabilité/déflation du plan §2.4 | À calibrer Phase 2 sur données réelles |
| D6 | Couloir hot-swappable vs timelocké | Timelocké (option 3 gouvernance) ; boot-only en intérim |

---

## Références croisées

- `plan.md` §2 (politique d'émission), §3.1 (budget partagé), §3.3 (pont voie B), §8 (params 🔧).
- Code : `inflation.rs`, `append_blocks_batch` (dag_storage_impl.rs), `persist.rs` (dedup S4),
  `reserve_snapshot.rs` (pattern de persistance), `utxo.rs` (supply cache),
  `runtime.rs`/`config.rs` (params), `admin.rs` (hot-swap instantané — à timelock-er en option 3).
- Tests de référence : `dag_sandbox.rs`, `replay_determinism.rs`, `dag_integrity.rs`.
