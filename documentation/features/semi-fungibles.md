---
tags: [feature]
created: 2026-06-14
updated: 2026-06-15
version: v0.20.0
---

# Semi-fongibles (SFT, façon ERC-1155)

## Résumé

Troisième modèle d'asset de PMS, entre le **token fongible** et le **NFT** : une
**classe semi-fongible** a des métadonnées riches **publiques** (nom, URI,
attributs) et est **fongible à l'intérieur de la classe** (quantité par détenteur,
divisible selon `decimals`) tout en étant **distincte entre classes**. Cas d'usage
metaverse/jeu : stacks d'objets identiques (500 épées de fer), tickets (1000 billets
identiques), ressources.

**Modèle retenu** (`pms-spec-semi-fungibles.md`) : la classe est **posée sur le
moteur UTXO existant** — son `asset_id` est `"{collection}:{class}"`, ses soldes
vivent dans les UTXO comme un token fongible. Conséquence décisive : une classe
SFT **hérite** du time-lock (`locked_until`) et des spend-conditions
(multisig/hashlock) — automatiques car per-UTXO — et peut activer le **demurrage**
(`demurrage_bps_per_day`, opt-in par classe, v0.20.0) qui décote ses UTXO par le
même mécanisme que les tokens (protocole 2.5). Le seul code neuf = un **registre de
classes** + des handlers ; mint/transfert/burn réutilisent
`Mint`/`TxUtxo`/`TokenBurn`.

## Namespace `collection:class`

L'`asset_id` d'une classe = `"{collection}:{class}"` (ex. `edenite-game:iron-sword`),
chaque segment `[a-z0-9-]{1,32}`. Le `:` :
- **garantit l'absence de collision** avec un token (dont l'`asset_id` ne contient
  pas `:`) et le PMS natif (`asset_id = None`) — `get_token` et `get_sft_class` ne
  peuvent jamais matcher le même id ;
- **donne le regroupement par collection gratuitement** (le préfixe = la collection) ;
- est **URL-safe** (utilisable dans `/v1/sft/classes/{asset_id}`).

## Configuration

Aucune activation : routes montées en standard. `POST /admin/sft/*` sont
admin-gated + gated read-only (produisent des blocs) ; `GET /v1/sft/*` sont publics
(catalogue). `mint_authority`/`creator` d'une classe = le coordinateur (qui forge
les blocs Mint).

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-types-payload` | `src/payload.rs` | Type `SftClass` (public) + `PlainPayload::SftClassCreate` + `SftClass::to_token_metadata()` |
| `pms-storage` | `src/token_store.rs` | Trait `SftClassStorage` (dans `EngineStorage`) |
| `pms-storage` | `src/rocks_store/sft_storage.rs` | Impl RocksDB (CF `sft_classes`) + scan par collection |
| `pms-core` | `src/net_adapter/persist.rs` | Validation `SftClassCreate` (format/unicité) + mint contraint SFT-aware |
| `pms-server` | `src/api_fn/sft.rs` | Endpoints REST (create/mint/list/get/collection) |

## Fonctions Clés

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `SftClass::to_token_metadata` | `pms-types-payload/src/payload.rs` | Vue `TokenMetadata` d'une classe → réutilise la validation de mint contraint (authority/cap) |
| `SftClassStorage::list_sft_classes_by_collection` | `pms-storage/src/rocks_store/sft_storage.rs` | Scan par préfixe `"{collection}:"` |
| persist `SftClassCreate` arm | `pms-core/src/net_adapter/persist.rs` | Valide segments/asset_id/decimals/max_supply>0 + unicité, enregistre après persist |
| `admin_mint_sft` | `pms-server/src/api_fn/sft.rs` | Pré-check `max_supply` (message clair) + forge `Mint` (cap enforcé au protocole) |

## Endpoints API

| Méthode | Path | Accès | Description |
|---------|------|-------|-------------|
| POST | `/admin/sft/classes` | admin (gated) | Crée une classe (`collection_id`, `class_id`, `name`, `uri?`, `attributes?`, `decimals`, `max_supply?`, `demurrage_bps_per_day?`) |
| POST | `/admin/sft/mint` | admin (gated) | Mint `amount` d'une classe (`asset_id`, `to`) — contraint par `max_supply` |
| GET | `/v1/sft/classes` | **public** | Toutes les classes |
| GET | `/v1/sft/classes/{asset_id}` | **public** | Détail d'une classe (`asset_id = collection:class`) |
| GET | `/v1/sft/collections/{collection}` | **public** | Classes d'une collection |

**Réutilisés** (zéro code SFT) : balance/supply via `/v1/balance` + `/v1/supply`
(`asset_id = "collection:class"`) ; **transfert** via `/v1/wallet/send-simple` ;
**burn** via `/v1/wallet/token/burn` (fire `OnTokenBurn{asset_id}` → burn-to-refund).

## Tests

- `pms-storage` : `sft_class_roundtrip_and_listing` (S1 : round-trip + listing par collection).
- `pms-core` (`tests/sft_class_test.rs`) : S2 — classe valide enregistrée ; asset_id
  incohérent / decimals>18 / segment invalide / max_supply≤0 / doublon → rejetés.
- `pms-server` (`tests/dag_sandbox.rs::test_sft_lifecycle`) : e2e — création + catalogue
  public + mint contraint (S3 over-cap → 400) + **fongibilité** (S5 transfert 100→70/30).

## Interactions
Liens : [[token-system]] (les SFT réutilisent le moteur UTXO + la validation de mint
contraint), [[protocol-primitives]] (time-lock & spend-conditions hérités per-UTXO),
[[nft-system]] (le NFT = l'autre extrême : unique, indivisible, métadonnées chiffrées),
[[smart-contracts]] (`OnTokenBurn` sur une classe SFT).
