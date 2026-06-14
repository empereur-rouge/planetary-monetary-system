# Spec — Semi-fongibles (SFT, façon ERC-1155) sur le moteur UTXO

> Statut : spec d'implémentation. Modèle validé : **classes fongibles posées sur
> le moteur UTXO existant**, métadonnées de classe **publiques**. Décision prise
> avec l'utilisateur (alternative « éditions NFT » écartée — voir §2).

## 1. Objectif

Ajouter un troisième modèle d'asset, entre le **token fongible** (un `asset_id`,
divisible, grande supply, métadonnées plates) et le **NFT** (unique, quantité 1,
indivisible, métadonnées riches chiffrées) :

- Un **semi-fongible (SFT)** = une **classe** avec des métadonnées riches
  (nom, image/URI, attributs) qui est **fongible à l'intérieur de la classe**
  (quantité par détenteur, divisible selon `decimals`) et **distincte entre
  classes**. Cas d'usage metaverse/jeu : stacks d'objets identiques (500 épées de
  fer), tickets (1000 billets identiques), ressources.

## 2. Modèle retenu — pourquoi UTXO et pas « éditions NFT »

Le moteur fongible est **déjà générique** : un UTXO porte `(adresse, montant,
asset_id)` ; balance / supply / transfert / burn sont agnostiques de l'asset
(cf. `crates/pms-core/src/validations/transactions.rs`, `rocks_store/utxo.rs`).
Une classe SFT = un `asset_id` fongible **avec un registre de métadonnées riches**.

**Conséquence décisive** : une classe SFT **hérite gratuitement** des primitives
protocole v0.10.0 parce que ses soldes vivent dans des UTXO :
time-lock (`locked_until`), spend-conditions (multisig/hashlock), demurrage,
mint contraint (max_supply / mint_authority / collatéral), conservation stricte.

L'alternative « éditions NFT » (ajouter une quantité au ledger NFT CF-based,
1-owner-par-token_id) imposerait de réinventer la comptabilité fongible côté NFT
et de **perdre** ces primitives (les NFT ne sont pas des UTXO). Écartée.

## 3. Namespace d'`asset_id` — `collection:class`

L'`asset_id` d'une classe SFT est **`<collection>:<class>`** (ex.
`edenite-game:iron-sword`), `collection` et `class` chacun `[a-z0-9-]{1,32}`.

- **Collision-free par construction** : le format token interdit `:`
  (`[a-z0-9_]{1,32}`), le PMS natif est `asset_id = None`. Donc un `asset_id`
  contenant `:` ne peut JAMAIS désigner un token ou le natif.
- **Regroupement collection gratuit** : le préfixe avant `:` est la collection.
  Supply d'une collection = somme des `asset_id` commençant par `<collection>:`.
- **URL-safe** : `:` n'est pas un séparateur de segment de path → utilisable tel
  quel dans `/v1/sft/classes/{asset_id}`.

## 4. Types & stockage

### `SftClass` (métadonnées de classe, PUBLIQUES) — `pms-types-payload`
```
SftClass {
    asset_id: String,            // "collection:class" (dérivé, = clé de registre)
    collection_id: String,       // "edenite-game"
    class_id: String,            // "iron-sword"
    name: String,                // "Épée de fer"
    uri: Option<String>,         // image/asset
    attributes: Option<String>,  // JSON libre (attributs de jeu)
    decimals: u8,                // 0 = items entiers ; >0 = divisible
    max_supply: Option<String>,  // cap (None = illimité)
    creator: String,             // pubkey créateur (immuable)
    mint_authority: String,      // pubkey autorisée à mint
}
```
Pas de chiffrement : la définition d'une classe est un **catalogue partagé** par
N détenteurs (≠ NFT dont la donnée est par-instance/privée).

### CF `sft_classes` (par ledger) — `pms-storage`
`asset_id` → `SftClass` JSON. Trait `SftClassStorage`
(put / get / list / list_by_collection), ajouté à `EngineStorage`. Ajouté aux
DEUX listes de `store.rs` (`required` + `CF_NAMES`) + `mig_X_to_Y` +
`CURRENT_VER`++.

## 5. Protocole — payload & validation

- **Payload** `PlainPayload::SftClassCreate(SftClass)` — **coordinator-only**
  (`require_coordinator`, ajout aux match exhaustifs authority/check + block_id
  type-string). Enregistre la classe (registry).
- **Validation persist** (hot path) du `SftClassCreate` :
  - `asset_id == format!("{collection_id}:{class_id}")` (cohérence),
  - `collection_id` / `class_id` ∈ `[a-z0-9-]{1,32}`,
  - `name` non vide (≤128), `decimals ≤ 18`, `max_supply` parsable si présent,
  - `creator` / `mint_authority` non vides,
  - **unicité** : `asset_id` pas déjà enregistré (anti-overwrite, comme la
    gouvernance).
- **Mint** : **réutilise** `PlainPayload::Mint { outputs }` (UTXO avec
  `asset_id = "collection:class"`). Le handler valide : classe enregistrée,
  `signer == mint_authority`, `(circulating + minted) ≤ max_supply` via
  `circulating_supply_by_asset(asset_id)`.
- **Transfert** : **réutilise** le chemin générique (`TxUtxo` /
  `wallet/send-simple`) — `asset_id = "collection:class"`. Zéro code neuf :
  time-lock / demurrage / spend-conditions s'appliquent automatiquement.
- **Burn** : **réutilise** `PlainPayload::TokenBurn` (asset_id = classe) → fire
  `OnTokenBurn { asset_id }`. Un contrat peut donc faire burn-to-refund sur une
  classe SFT sans nouveau trigger.

## 6. API

| Méthode | Path | Accès | Rôle |
|---|---|---|---|
| POST | `/admin/sft/classes` | admin (gated) | Crée une classe (forge `SftClassCreate`) |
| POST | `/admin/sft/mint` | admin (gated) | Mint une quantité d'une classe (valide classe+cap+authority, forge `Mint`) |
| GET | `/v1/sft/classes` | public | Liste toutes les classes |
| GET | `/v1/sft/classes/{asset_id}` | public | Détail d'une classe (`asset_id = collection:class`) |
| GET | `/v1/sft/collections/{collection}` | public | Classes d'une collection |

Balance & supply : **endpoints existants** (`/v1/balance`, `/v1/supply` avec
`asset_id = "collection:class"`) — pas de nouveau code. Transfert : endpoints
wallet existants. Burn : `/v1/wallet/token/burn` existant.

## 7. Versions

`Cargo` MINOR, `DAG_VERSION` MINOR (nouveau payload additif, pas de wipe),
`CURRENT_VER`++ (nouveau CF), `API_VERSION`++ (nouvelles routes).

## 8. Plan de test (anti-faux-tests)

| # | Test | Assert |
|---|---|---|
| S1 | registry round-trip + unicité | put/get/list ; doublon `asset_id` rejeté |
| S2 | create validation | asset_id≠collection:class → rejet ; decimals>18 → rejet ; name vide → rejet |
| S3 | mint respecte le cap | mint > max_supply → rejet ; ≤ cap → UTXO créé, balance = montant |
| S4 | mint authority | signer ≠ mint_authority → rejet |
| S5 | fongibilité e2e (sandbox) | mint 100 à A → A transfère 30 à B → balance A=70, B=30 (chemin UTXO générique) |
| S6 | héritage primitives | un mint SFT avec `locked_until` futur → indépensable avant échéance (réutilise le time-lock) |
| S7 | burn → contrat | `OnTokenBurn{asset_id=collection:class}` fire sur burn SFT |
| S8 | collision namespace | impossible de créer un token dont l'asset_id contient `:` (déjà interdit) ; SFT et token coexistent |

## 9. Plan d'implémentation (commits atomiques)

1. **P1 — Protocole** : `SftClass`, CF `sft_classes` + trait + migration, payload
   `SftClassCreate` (coordinator-only), validation persist (format/unicité),
   `CURRENT_VER`++/`DAG_VERSION`. Tests S1, S2. *Pas encore branché sur l'admin.*
2. **P2 — Mint + endpoints** : create / mint (valide classe+cap+authority) /
   list / collection / get. Transfert & burn réutilisent l'existant. Tests S3-S8
   (dont sandbox e2e). `API_VERSION`++.
3. **P3 — SDK + docs** : `createSftClass` / `mintSft` / `getSftClasses` /
   `getSftClass` (+ transfert/burn déjà couverts). Fiche Obsidian + MOC + bump SDK.

## 10. Hors scope (v1)

- Hiérarchie collection en tant qu'objet de 1ʳᵉ classe (registre dédié de
  collections) — le préfixe `collection:` suffit pour grouper en v1.
- Métadonnées chiffrées par classe (écarté : classe = catalogue public).
- Mise à jour de métadonnées de classe après création (immuable en v1 ;
  re-création sous un nouveau `class_id` si besoin).
