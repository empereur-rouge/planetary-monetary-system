---
tags: [feature]
created: 2026-07-10
updated: 2026-07-10
version: v0.31.0
---

# Provisionnement custodial d'assets — sans token admin (protocole 2.8)

## Résumé

Permet à **N opérateurs custodiaux indépendants** (chacun exploitant une instance
creator-studio qui détient les clés de SES créateurs) de **créer et minter leurs
propres éditions capées + royalty** — tokens fongibles OU classes [[semi-fungibles|SFT]] —
en s'authentifiant par la **clé du créateur / `mint_authority`**, PAS par le token
**admin** du DAG partagé. Donner l'admin à un opérateur reviendrait à lui donner le
contrôle du DAG ; ce rail supprime ce besoin.

Le Coordinator forge/signe toujours le bloc ([[validation-consensus|single-writer]]),
mais ne peut PLUS ni créer sous la collection d'un autre, ni minter la classe/le
token d'un créateur **sans sa clé** : l'autorité de mint est prouvée par une
**signature détachée embarquée** dans le payload et **vérifiée au consensus** (même
modèle que [[marketplace-royalty|RoyaltyUpdate]]).

Avant 2.8, provisionner une édition capée exigeait l'admin (`POST /admin/sft/{classes,mint}`,
`POST /admin/tokens/{create,mint}`) — le SEUL point du rail qui l'exigeait. C'était
incohérent : `mintNft` (unique, auto-limité) et `send-simple` (custodial) ne
l'exigeaient pas, et un mint SFT est déjà auto-limité par `max_supply`. 2.8 ferme
l'incohérence.

## Modèle

- **Autorité déléguée, vérifiée au consensus** : nouveau payload
  `PlainPayload::CustodialMint { asset_id, outputs, auth_pubkey_hex,
  auth_signature_b64, mint_nonce }`. Le validateur exige (a) que `asset_id` résolve à
  un asset enregistré (token OU classe SFT) en **fail-closed**, (b) que
  `auth_signature_b64` soit une signature valide de `auth_pubkey_hex` sur
  [`custodial_mint_signing_message`](#signing-message), (c) que `auth_pubkey_hex`
  **dérive l'adresse `mint_authority`** enregistrée (`unlock_matches_address` — gère
  pubkey-hex ET bech32m). Le gate coordinateur global du `Mint` classique NE
  s'applique PAS (payload distinct) : l'autorité dérive **entièrement** de la
  signature du `mint_authority`. Le natif (`asset_id = None`) ne peut pas être minté
  ici (tout output doit porter l'`asset_id` déclaré enregistré).
- **Create = dérivation de clé** : `POST /v1/sft/classes` / `POST /v1/tokens/create`
  posent `creator = mint_authority =` l'adresse **dérivée de `creator_private_key_b64`**
  (jamais fournie en clair). Un opérateur ne peut donc pas usurper le `creator` d'un
  autre (base de l'anti-squat). La validation (format, unicité, royalty) vit au
  consensus (arms `SftClassCreate` / registre token).
- **Cap auto-limité + enforced** : `max_supply` déclaré à la création ; le consensus
  rejette `circulating + mint > max_supply` (`MaxSupplyExceeded`), sous un **lock
  per-asset** fermant le TOCTOU d'inflation entre deux mints concurrents.
- **Anti-replay** : `mint_nonce` (unique par mint) consommé **une seule fois**
  `(asset_id, mint_nonce)` — CF durable `custodial_mint_consumed` écrite dans le MÊME
  WriteBatch que le bloc + claim RAM atomique au commit-point (modèle
  [[block-payloads|BridgeMint]]). Un payload signé rejoué dans un nouveau bloc est rejeté.
- **Anti-squat de collection** ([Q4](#q4-namespacing)) : CF `sft_collections`
  (`collection_id → owner`). La 1ʳᵉ classe d'une collection en fixe le propriétaire ;
  toute classe suivante doit porter le même `creator` (claim atomique RAM + durable).
- **Compliance** : rejet **fail-closed** si une adresse d'output est gelée.

## Configuration

Aucune activation : routes montées en standard. Auth : **API-key** (+ scope), gated
read-only (produisent un bloc). Nouveau scope API-key `"sft"` (les routes
`/v1/tokens/*` réutilisent `"tokens"`). Un opérateur reçoit une clé API scopée
`["sft","tokens","wallet"]` — jamais le token admin.

## Signing message

`custodial_mint_signing_message(network_id, asset_id, outputs, mint_nonce)` =
SHA-256 hex du JSON canonique compact
`{domain:"pms-custodial-mint-v1", network_id, asset_id, outputs, mint_nonce}`. Lie
**tous** les champs d'output honorés au consensus (`address`, `amount`, `asset_id`,
`locked_until`, `spend_condition`) SAUF `created_at` (assigné par le système,
anti-antidatage). **Source unique** partagée par le signeur (endpoint/SDK) et le
validateur — vecteurs golden pinnés côté Rust (`payload.rs`) ET SDK TS pour parité.

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-types-payload` | `src/payload.rs` | `PlainPayload::CustodialMint` + `custodial_mint_signing_message` + `custodial_mint_consumed_key` (+ golden test) |
| `pms-core` | `src/net_adapter/persist.rs` | Arm consensus `CustodialMint` (résolution fail-closed, vérif signature + binding, anti-replay, freeze, cap sous lock per-asset, commit-point claim) + claim de collection dans l'arm `SftClassCreate` |
| `pms-core` | `src/validations/mint.rs` | `validate_custom_asset_mint_amounts` (money-rules partagées Mint/CustodialMint, checked arithmetic) |
| `pms-core` | `src/validations/authority.rs` | `CustodialMint` dans le groupe owner/authority-signé (pas coordinator-only) |
| `pms-core` | `src/concurrent_dag/spent.rs` | Claims RAM atomiques : `try_consume_custodial_mint`, `try_claim_collection` |
| `pms-core` | `src/core_adapter.rs` | `custodial_mint_locks` (lock async per-asset) |
| `pms-storage` | `src/rocks_store/dag_storage_impl.rs` | `block_anti_replay_markers` (parse unique) + écriture batch CF `custodial_mint_consumed` |
| `pms-storage` | `src/rocks_store/{store.rs,migration.rs,sft_storage.rs}` | CFs `custodial_mint_consumed` + `sft_collections`, `mig_12_to_13`, `put/get_collection_owner` |
| `pms-storage` | `src/traits.rs` | `DagStorage::is_custodial_mint_consumed` |
| `pms-server` | `src/api_fn/custodial.rs` | Handlers `/v1` create/mint/prepare (custodial + pré-signé) |
| `pms-server` | `src/api/routes.rs`, `src/api_keys.rs` | Câblage routes + scope `"sft"` |

## Fonctions Clés

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `custodial_mint_signing_message` | `pms-types-payload/src/payload.rs` | Message canonique signé par le `mint_authority` (bind outputs + nonce + network_id) |
| `validate_custom_asset_mint_amounts` | `pms-core/src/validations/mint.rs` | Money-rules SANS autorité (granularité + cap + collatéral), partagées Mint/CustodialMint |
| `try_consume_custodial_mint` | `pms-core/src/concurrent_dag/spent.rs` | Claim atomique one-shot du nonce (commit-point) |
| `try_claim_collection` | `pms-core/src/concurrent_dag/spent.rs` | Claim atomique first-owner d'une collection (anti-squat) |
| `custodial_mint` / `create_sft_class_custodial` / `create_token_custodial` | `pms-server/src/api_fn/custodial.rs` | Handlers `/v1` |

## Endpoints API

| Méthode | Path | Scope | Description |
|---------|------|-------|-------------|
| POST | `/v1/sft/classes` | `sft` | Crée une classe SFT ; `creator = mint_authority =` addr(`creator_private_key_b64`) |
| POST | `/v1/tokens/create` | `tokens` | Idem pour un token fongible |
| POST | `/v1/sft/mint` | `sft` | Mint SFT borné par `max_supply`, forge `CustodialMint` |
| POST | `/v1/tokens/mint` | `tokens` | Mint token borné par `max_supply`, forge `CustodialMint` |
| POST | `/v1/sft/mint/prepare` | `sft` | Renvoie `{message_hex, mint_nonce, mint_authority}` à signer (voie pré-signée) |
| POST | `/v1/tokens/mint/prepare` | `tokens` | Idem token |

Les mints acceptent **soit** `mint_authority_private_key_b64` (custodial : le serveur
signe, vérifie == `mint_authority` sinon **403 `1050`**), **soit**
`(auth_pubkey_hex + auth_signature_b64 + mint_nonce)` pré-signés (le coordinateur ne
voit jamais la clé). Les routes `/admin/sft/*` et `/admin/tokens/*` (coordinator-authority)
restent inchangées.

## Q1–Q5 (réponse à la demande d'origine)

- **Q1 (create custodial)** : `POST /v1/sft/classes` — EXISTE (v0.31.0).
- **Q2 (mint custodial)** : `POST /v1/sft/mint` via `CustodialMint` — EXISTE ; a exigé
  le changement de protocole (l'autorité de mint était sur le signataire du bloc =
  coordinateur ; désormais signature `mint_authority` embarquée).
- **Q3 (chemin non-admin préexistant)** : n'existait PAS pour SFT/token avant 2.8
  (existait pour royalty/market/NFT-issuer).
- **Q4 (anti-squat namespace)** : CF `sft_collections` — la 1ʳᵉ classe fixe le
  propriétaire de la collection ; les suivantes exigent le même `creator`.
- **Q5 (royalty enforced)** : inchangée — `royalty_bps`/`royalty_beneficiary` posés à
  la création sont **ré-dérivés du registre au consensus** au `MarketSettle`
  ([[marketplace-royalty]]), indépendamment du chemin de create/mint.

## Sécurité (revue consensus + /simplify, v0.31.0)

- **Autorité** : la signature `mint_authority` remplace le gate coordinateur pour le
  cas délégué ; `unlock_matches_address` (pubkey-hex ET bech32m) empêche l'usurpation.
- **Anti-replay** : nonce consommé une fois (durable + RAM atomique) — un payload signé
  rejoué (block-id distinct) est rejeté.
- **Anti-inflation (TOCTOU)** : cap ré-vérifié sous lock per-asset tenu jusqu'à
  `apply_diff` (prouvé par test concurrent : 60 + 60 sous cap 100 → un seul passe).
- **Fail-closed** : asset inconnu / natif / erreur store / adresse gelée → rejet
  (jamais fail-open sur les gates financiers/compliance).
- **Overflow** : `checked_add`/`checked_mul` dans cap/collatéral (pas de panic sur
  `max_supply` proche de `Decimal::MAX`).
- **Perf** : marqueurs anti-replay (bridge + custodial) extraits en **un seul parse**
  par bloc + pré-filtre substring (hot path 10K+ TPS).

## Séquence end-to-end (test `test_custodial_provisioning_end_to_end`)

1. `POST /v1/sft/classes { collection_id:"studio", class_id:"ticket", decimals:0, max_supply:"1000", royalty_bps:1000, royalty_beneficiary:<créateur>, creator_private_key_b64:<clé créateur> }` → `mint_authority = <créateur>`.
2. `POST /v1/sft/mint { asset_id:"studio:ticket", to:<vendeur>, amount:"5", mint_authority_private_key_b64:<clé créateur> }` → vendeur détient 5.
   (Une mauvaise clé → **403**.)
3. Transfert `POST /v1/wallet/send-simple { asset_id:"studio:ticket", … }` — rail UTXO générique ([[utxo-system]]).
4. `POST /v1/market/settle { asset_sold:"studio:ticket", quantity:"1", price:"100" }` → royalty 10 % (=10) versée au créateur, enforced au consensus.

**ZÉRO token admin sur tout le flux.**

## Versions

`Cargo` `0.30.3 → 0.31.0` (MINOR) · `DAG_VERSION` `3.14.0 → 3.15.0` (payload additif) ·
`CURRENT_VER` `12 → 13` (`mig_12_to_13`, 2 CFs) · `API_VERSION` `38 → 39`.
⚠️ Mixed-version P2P : upgrade coordonné AVANT tout mint custodial (un nœud < 3.15.0
ne désérialise pas `CustodialMint`).

## Interactions

Liens : [[semi-fungibles]] (classe capée mintée custodialement), [[token-system]]
(token fongible custodial), [[marketplace-royalty]] (royalty enforced au settlement,
même modèle de signature détachée), [[block-payloads]] (`CustodialMint`),
[[validation-consensus]] (single-writer + arms), [[utxo-system]] (soldes + cap),
[[compliance]], [[api-key-authentication]] (scope `sft`).
