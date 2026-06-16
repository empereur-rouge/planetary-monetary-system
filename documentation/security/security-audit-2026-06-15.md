---
tags: [security, audit, reference]
created: 2026-06-15
updated: 2026-06-15
version: v0.21.0
---

# Audit de sécurité — moteur DAG (v0.21.0)

## Contexte & méthode

Déclencheur : un bug d'incohérence `prepare_tx` ↔ `wallet_send_tx` sur le destinataire
des frais (sharding coordinateur) a atteint le testnet. Question légitime : **« comment
est-ce passé, et y en a-t-il d'autres ? »**

Méthode : 6 audits parallèles (lecture seule) sur authority/consensus/clés, bridge,
émission/supply, crypto, surface HTTP, compliance/multi-ledger. Les trouvailles
**CRITIQUES** ont été re-vérifiées manuellement (marquées `[vérifié]`). Les autres sont
`[agent-confirmé]` (citations de code à l'appui) ou `[à confirmer]`.

> ⚠️ Ceci n'est pas un substitut à un audit professionnel externe avant mainnet. Le fil
> rouge des bugs : **logique dupliquée entre deux chemins + tests qui ne couvrent que le
> chemin par défaut + absence d'invariants de conservation testés.**

## Verdict

- **Cœur de confiance SOLIDE** : autorité single-writer (fail-closed en prod), gating
  coordinator-only exhaustif des payloads privilégiés, rotation de clé auto-autorisée,
  signatures liées au signataire + réseau + payload, crypto (ECDSA/X25519-AES-GCM avec
  AAD binding) correcte, isolation multi-ledger des stores, idempotence par block-id.
  **Aucune faille critique dans cette couche.**
- **Mais** : la couche applicative (handlers HTTP, émission de frais, chemins chiffrés,
  bridge, contrats) présente **plusieurs failles d'intégrité de supply et de bypass de
  validation**, dont 2 clusters CRITIQUES vérifiés.

---

## Cause racine A — Les payloads chiffrés / non-`TxUtxo` sautent toute la validation hot-path  `[vérifié]`

Toute la validation financière du hot-path est gardée derrière
`if let Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx)))`
([persist.rs:1029](../../crates/pms-core/src/net_adapter/persist.rs)). Le chemin
**chiffré** que produit `/wallet/tx/send` (et `/v1/wallet/send-simple`) la saute en
entier. Les seuls checks pour une tx chiffrée sont ceux que le handler refait à la main
sur le plaintext — et il en manque (grep `wallet_send_tx` : 0 occurrence de
`is_frozen`/`locked_until`/`spend_condition`/`seen`).

| Contrôle manquant sur le chemin chiffré | Présent dans le hot-path | Gravité | Joignable |
|---|---|---|---|
| **Dédup d'inputs** (`[A,A]` → conservation compte 2×A, delta dépense 1×A) → **création de monnaie** | `fetch_input_outputs` → `DoubleSpend` ([transactions.rs:101](../../crates/pms-core/src/validations/transactions.rs)) | **CRITIQUE** | API-key |
| **Compliance/gel** (adresse gelée peut envoyer/recevoir) | `is_frozen` inputs+outputs ([persist.rs:1075](../../crates/pms-core/src/net_adapter/persist.rs)) | HAUTE | API-key |
| **Time-lock** des inputs (vesting/escrow/collatéral dépensables avant terme) | `check_input_time_locks` | HAUTE | API-key |
| **MultiSig/HashLock** (quorum M-of-N, préimage jamais vérifiés ; seul le cas PubKey) | `check_spend_authorization` | HAUTE | API-key |
| Borne `max_fee_per_tx` | oui | BASSE | API-key |

**Concerne** : `wallet_send_tx` `[vérifié]` ET `wallet_send_simple` `[agent-confirmé]`
(custodial, endpoint le plus utilisé). Mint vers une adresse gelée : non gardé non plus.

**Correctif (haute-levier, une fois)** : déplacer le check de gel + la validation pour
opérer sur le **`UtxoDelta` construit** (adresses des inputs+outputs) juste avant
`apply_diff`, **quel que soit le chiffrement** ; et faire passer le plaintext de
`wallet_send_tx`/`wallet_send_simple` par le **même** `validate_transaction_full`
(dédup, time-locks, spend-conditions) avant chiffrement. Ferme tout le cluster d'un coup
et empêche la re-dérive.

---

## Cause racine B — Mints natifs non réconciliés / non gatés → inflation de supply

### B1. Double-matérialisation des frais (fonctionnement normal)  `[vérifié]`

Pour toute tx avec frais : (1) l'émetteur finance un output de frais vers le
coordinateur/shard (conservé), (2) `accumulate_tx_fee` crédite le pool
([fee_accumulation.rs](../../crates/pms-server/src/api_fn/tx_helpers/fee_accumulation.rs)),
(3) le distributeur **mint du PMS frais** (`PlainPayload::Mint`, **zéro `TxInput`** dans
tout `fee_distribution/`, [distribute.rs:295](../../crates/pms-server/src/fee_distribution/distribute.rs))
vers les nodes/treasury. L'UTXO de frais on-chain n'est **jamais dépensé** (la
consolidation ne touche que l'adresse maître, pas les shards). → **+frais de supply à
chaque cycle**, sur-émission continue, non débitée de l'`EmissionGate`. **CRITIQUE**
pour un moteur dont la promesse est la rigueur monétaire.

### B2. Mint de remboursement NFT-burn non plafonné  `[agent-confirmé]`

Le chemin `OnTokenBurn/MintNative` est gaté par `EmissionGate` ; le chemin
`OnNftBurn/AccumulateRefund` ne l'est pas. Un remboursement `asset_id:None` (PMS natif)
est minté sans plafond ni kill-switch via le distributeur. **Latent** (le jeu Edenite
utilise `asset_id:"edenite"`), mais c'est une primitive d'inflation illimitée si un
contrat de refund natif est enregistré. Gravité **CRITIQUE (latent)**.

### B3. Bridge `BridgeMint` non adossé  `[agent-confirmé]`

`BridgeMint` n'est validé que par signature coordinateur + champs non-vides
([authority.rs:109](../../crates/pms-core/src/validations/authority.rs)) ; **aucune**
réconciliation `sum(outputs)==lock.amount`, `asset_id`, existence/consommation du lock.
L'anti-replay (`mark_bridge_lock_consumed`) n'est appelé que par le **producteur**
(`engine.rs`), jamais dans le chemin de persistance validé → un `BridgeMint` ingéré en
P2P ou rejoué (nonce différent → nouvel id) re-mint. Bypasse aussi `EmissionGate`.
**Mitigé** : bridge admin-gated + OFF par défaut + signature coordinateur requise (donc
exige la clé coordinateur ou un rejeu de bloc signé), mais le **protocole** n'a aucune
garantie de conservation. Gravité **CRITIQUE (mitigé par admin-gate)**.

### B4. Faucet custom-ledger mint natif non gaté ; `BridgeLock` détruit le change  `[agent-confirmé]`
Faucet sur ledger custom mint `asset_id:None` sans `EmissionGate` (MEDIUM, coordinator).
`BridgeLock` brûle l'excédent d'inputs (pas de change) — conservation `>=` au lieu de `==`.

**Correctif B** : (a) rendre la distribution de frais **conservatrice** (dépenser les
UTXO de frais accumulés, OU ne plus émettre l'output on-chain) ; (b) router **tout** mint
natif (distributeur, refund NFT, bridge, faucet custom) par `EmissionGate` ; (c) bridge :
réconcilier mint↔lock atomiquement dans la persistance + change sur `BridgeLock`.

---

## Cause racine C — Surface HTTP  `[agent-confirmé]`

- **C1 — `/internal/*` exposé sans auth via le fallback du gateway.** Les routes
  `internal_routes()` sont `.merge()`-ées dans le routeur public
  ([routes.rs:609](../../crates/pms-server/src/api/routes.rs)) et le gateway proxie tout
  chemin non-matché. → `GET /internal/utxos/{addr}` fuite l'UTXO-set de n'importe quelle
  adresse sans clé ; `POST /internal/submit_block` contourne `require_writable` (la valve
  read-only). Contenu par : la persistance exige toujours la signature coordinateur (donc
  pas de forge de bloc). Gravité **HAUTE** (confidentialité + bypass valve, pas vol).
  Fix : ne pas merger `/internal` dans le routeur public ; denylist `/internal/` au gateway.
- **C2 — Fuite d'état dans les erreurs.** `prepare_tx`, `burn_nft_simple`, `wallet_send_tx`,
  `get_token`… renvoient des messages ad-hoc divulguant solde exact, outpoints UTXO,
  propriétaire NFT, statut gelé → vecteurs d'énumération. Fix : migrer vers `ApiError`
  (codes numériques stables, déjà mandaté). HAUTE.
- **C3 — Pas de séparation read/write des scopes d'API-key** ; une clé « read » peut
  écrire. **C4 — Store de clés vide = fail-open** (toutes routes ouvertes). MEDIUM.

---

## Cause racine D — Ops coordinateur sans validation de conservation  `[agent-confirmé]`

`Seize`/`Reverse` n'appliquent leur delta UTXO qu'avec un check « inputs/outputs
non-vides » — pas de conservation, pas de vérif d'existence, pas de check double-spend
([persist.rs:766-801](../../crates/pms-core/src/net_adapter/persist.rs)) + TOCTOU entre
snapshot et apply (pas de `compliance_lock` tenu). Autorisation coordinateur OK. Peut
corrompre le compteur de supply. MEDIUM. Fix : router par une validation conservatrice +
tenir le `compliance_lock`.

---

## Cause racine E — Défense en profondeur & robustesse  `[agent-confirmé]`

- **E1** Malléabilité ECDSA high-S non rejetée + double encodage (DER/raw) — BASSE
  (neutralisée par le design block-id qui exclut la signature ; defense-in-depth).
- **E2** Caps `max_inputs/max_outputs/max_tx_bytes` = config morte sur le chemin prod
  (les deux, plain + chiffré) → amplification CPU (DoS). MEDIUM.
- **E3** Arithmétique `rust_decimal` non-checkée dans `contracts/engine.rs` → panics sur
  input attaquant (tue la task listener → refunds gelés). MEDIUM.
- **E4** Gas-pool dual-store : dépôt (main) vs consume (per-ledger) sur CFs différents
  (dormant tant que `gas_per_tx=0`). MEDIUM.
- **E5** Propriété per-ledger (`owner_pubkey`) jamais vérifiée (audit-only). MEDIUM.
- **E6** Chemins gated-par-config jamais testés (profil du bug d'origine) :
  `daily_inflation_enabled` (ON en prod, 0 test), `auto_consolidate_interval_secs`,
  `activity_retention_days`, ARM→503 du read-only guard, `enforce_fee_recipient`+
  `allowed_fee_addresses` (branche morte, shard-aveugle si activée). MEDIUM.

---

## Confirmé SAÌN (à ne pas re-toucher)

Single-writer fail-closed + testé ; `validate_payload_authority` exhaustif ; rotation de
clé auto-autorisée + grace keys sans autorité ; signatures liées signataire/réseau/payload ;
chiffrement AAD-bindé (anti-swap recipient, testé) ; clé coordinateur at-rest (Argon2id) ;
clés privées red
actées en Debug + zeroize ; comparaisons constantes (admin token, API key) ;
isolation des stores multi-ledger + `contract_store` épinglé au main partout ; idempotence
par block-id ; pas de spoof XFF pour l'auth (IP via ConnectInfo TCP only).

---

## Plan de remédiation priorisé

| Rang | Cause racine | Gravité | Confiance | Effort | Bloque mainnet ? |
|---|---|---|---|---|---|
| 1 | **A** — validation/freeze sur le delta (chemin chiffré) | CRITIQUE | vérifié | M | **OUI** |
| 2 | **B1** — frais brûlés à la source (net-zéro) | CRITIQUE | ✅ **cœur fait v0.24.0** (2a+2b) ; 2c/2d à suivre | M | **OUI** |
| 3 | **B2/B3/B4** — gater tout mint natif par EmissionGate + réconcilier bridge | CRITIQUE (latent/mitigé) | agent | M-L | **OUI** |
| 4 | **C1** — fermer `/internal/*` public + denylist gateway | HAUTE | agent | S | oui |
| 5 | **C2** — migrer handlers vers ApiError (anti-énumération) | HAUTE | agent | M | recommandé |
| 6 | **D** — conservation seize/reverse + lock | MEDIUM | agent | M | recommandé |
| 7 | **E2/E3/C3/C4/E4/E5** — DoS caps, checked-math, scopes, gas-pool, ownership | MEDIUM | agent | M | durcissement |
| 8 | **E1/E6** — low-S, tests des chemins gated-par-config | BASSE-MEDIUM | agent | M | durcissement |

**Tests manquants (la vraie cause des passages)** — à ajouter avec les fix :
1. **Invariant de conservation de supply** : total PMS natif avant/après (tx avec frais +
   cycle de distribution) == initial + émission budgétée. Aurait attrapé B1.
2. Test du chemin chiffré couvrant gel/time-lock/multisig/hashlock/input-dupliqué (cause A).
3. Tests des chemins gated-par-config en position **ON** (sharding ✅ fait, inflation,
   consolidation, rétention, read-only ARM).
4. Test bridge mint↔lock (montant/asset/replay).

## Séquence recommandée
Phase 1 (bloquant mainnet) : rangs 1-3 — un commit par cause racine, chacun avec son test
de conservation/bypass. Phase 2 : rangs 4-6. Phase 3 : durcissement 7-8. `/simplify` entre
chaque phase.

---

## Addendum 2026-06-16 — post `/code-review` du rang 1

Le rang 1 (cause A) est corrigé et committé (validation partagée
`validate_plain_txutxo` / `validate_txutxo_full`, 6 tests e2e verts). Le
`/code-review` haute-recall sur ce diff a confirmé **aucune régression** (refactor
strict-superset) mais a remonté un finding **plus profond, pré-existant** :

### ✅ CORRIGÉ (v0.23.0) — Rang 1-bis : TOCTOU concurrent sur l'application du delta chiffré
> **Statut : fermé.** Guard de claim atomique (`try_mark_spent`) avant `apply_diff`
> dans `do_persist_block_internal` (plain + chiffré) + test de concurrence
> `concurrent_double_spend_is_rejected`. Détails ci-dessous.

`persist_block_with_delta` appelle `do_persist_block_internal` **directement** (pas
via un channel sérialisé) → une requête par tâche axum, concurrentes. Pour un
payload **chiffré**, la validation (double-spend inclus, via la lecture du
`ShardedUtxoSet`) tourne dans le **handler**, puis le delta est appliqué plus tard
par `apply_diff` ([utxo.rs:350](../../crates/pms-core/src/utxo.rs)) — qui retourne
`()` et **ne rejette pas** un input déjà dépensé (seul l'idempotence par block-id
est vérifiée, [persist.rs:1412](../../crates/pms-core/src/net_adapter/persist.rs)).
Deux sends chiffrés concurrents dépensant le même UTXO peuvent donc tous deux
passer la validation (input vu vivant) puis tous deux appliquer leur delta →
**double-dépense / inflation**. Le chemin plain est moins exposé (validation +
apply dans le même `do_persist_block_internal`) mais mérite la même vérification
(deux `do_persist` concurrents). Non introduit par le fix du rang 1 ; le fix
améliore au contraire la validation.
**Correctif** : re-vérifier l'existence/non-gel des inputs **au moment de
l'apply**, sous le verrou, OU faire que `apply_diff` rejette une dépense d'input
absent (et propager le rejet). Test : deux sends concurrents sur le même UTXO →
un seul accepté, supply inchangée. *À traiter avant ou avec le rang 2.*

### Fixes de revue appliqués (dans le commit de suivi du rang 1)
- `wallet_send_simple` : auto-ajout xpk via `fee_recipient_addresses` (shards inclus).
- `wallet_send_tx` : réutilise l'émetteur validé (supprime un `get_utxo` redondant).
- `validate_txutxo_full` : applique la runtime-config (parité policy avec le hot-path).

### Findings BAS notés (non bloquants)
- Time-lock validé à l'instant de l'appel (handler) vs persist — fenêtre = latence
  d'une requête, ne matère qu'au bord exact du lock.
- `is_frozen` fail-open sur erreur store (pré-existant) — pour un gate compliance
  bancaire, envisager fail-closed.
