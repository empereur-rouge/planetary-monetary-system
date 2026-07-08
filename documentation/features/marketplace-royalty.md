---
tags: [feature]
created: 2026-07-08
updated: 2026-07-08
version: v0.27.0
---

# Marketplace — Règlement atomique + Royalty de revente (protocole 2.7)

## Résumé

Primitive **marketplace enforced-consensus** : une vente/revente déplace un item
(token OU classe SFT) du vendeur vers l'acheteur ET le paiement de l'acheteur vers
le vendeur **dans UN SEUL bloc** (atomicité tout-ou-rien), en **prélevant et versant
la royalty de revente au consensus** — impossible de sous-payer le créateur, même en
contournant le handler. Fonctionne pour **tout token, toute classe SFT, tout ledger**,
la royalty étant reversée dans **l'asset de paiement** (pas forcément PMS natif).

Répond à deux garanties NFT-marketplace : **Q2** (« à chaque revente, le créateur
touche X% du prix, prélevé par le DAG ») et **Q3** (« vente/revente atomique PMS↔actif
en une settlement »).

## Modèle

- **Politique royalty par-asset** : `royalty_bps` (part en basis points du prix) +
  `royalty_beneficiary` (défaut = `creator`) portés par [[semi-fungibles|SftClass]] ET
  `TokenMetadata`, immuables après enregistrement, **ré-dérivés du registre par le
  validateur** (jamais lus depuis le bloc).
- **Payload `MarketSettle`** : embarque une transaction UTXO **co-signée** (vendeur
  déverrouille l'item, acheteur le paiement — chaque input signé par son propriétaire,
  cf. [[block-payloads|TxUtxo]]). Owner-signé (pas coordinator-only) ; le bloc reste
  forgé/signé par le Coordinator en [[validation-consensus|single-writer]]. Payload
  **plain** (vente publique-by-design : prix, parties, royalty auditables).
- **Gate consensus** (`validations::market`) : après la validation UTXO complète
  partagée avec TxUtxo (`validate_plain_txutxo` — signatures, ownership, conservation
  par-asset, [[compliance]]), impose un **accounting EXACT** : le vendeur et le
  bénéficiaire reçoivent *exactement* leurs parts déclarées (`net_to_seller` =
  `price − royalty`, et `royalty`), dans le *seul* asset de paiement ; l'acheteur reçoit
  *exactement* `quantity` de l'item ; tout autre crédit (tiers, asset-side, sur-paiement,
  prix sous-déclaré) est **rejeté**.

## Configuration

Aucune activation : route montée en standard. `POST /v1/market/settle` est
API-key-gated + gated read-only (produit un bloc). La royalty se configure à la
création de l'asset (`royalty_bps`/`royalty_beneficiary` sur `/admin/tokens/create` ou
`/admin/sft/classes`) ; absente ⇒ swap atomique pur (royalty 0).

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-types-payload` | `src/payload.rs` | `PlainPayload::MarketSettle` + champs royalty sur `TokenMetadata`/`SftClass` + `TokenMetadata::effective_royalty()` |
| `pms-core` | `src/validations/market.rs` | Validateur pur : `compute_royalty` (checked), `validate_settlement` (accounting exact), `MAX_SETTLEMENT_AMOUNT` |
| `pms-core` | `src/net_adapter/persist.rs` | Gate consensus (résolution fail-closed + `validate_settlement`), delta UTXO (fusionné avec TxUtxo), `resolve_asset_metadata_strict` |
| `pms-core` | `src/validations/authority.rs` | `MarketSettle` dans le groupe owner-signé (pas coordinator-only) |
| `pms-server` | `src/api_fn/market.rs` | Endpoint `POST /v1/market/settle` (custodial one-shot : coin selection, co-signature, forge) |
| `pms-storage` | `src/rocks_store/token_registry.rs` | Validation royalty au registre token |
| `pms-storage` | `src/helpers/classify.rs` | Classification activité (`market_buy`/`market_sell`/`royalty_received`) |
| `pms-server` | `src/api_fn/activity/classify.rs` | Classification activité (async + SSE) |
| `pms-wallet` | `src/history.rs` | Prédicats d'implication d'adresse (feed/historique) |

## Fonctions Clés

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `validate_settlement` | `pms-core/src/validations/market.rs` | Accounting exact : n'autorise QUE les crédits (buyer↦item, seller↦net, beneficiary↦royalty) aux montants exacts |
| `compute_royalty` | `pms-core/src/validations/market.rs` | `price × bps / 10000` floored, **checked** (overflow → `None`) |
| `effective_royalty` | `pms-types-payload/src/payload.rs` | Résout `(bps, bénéficiaire)` — défaut bénéficiaire = `creator` |
| `resolve_asset_metadata_strict` | `pms-core/src/net_adapter/persist.rs` | Résolution **fail-closed** (erreur store → rejet, pas royalty 0) |
| `market_settle` | `pms-server/src/api_fn/market.rs` | Build + co-signe + forge + persist un settlement |

## Endpoints API

| Méthode | Path | Description |
|---------|------|-------------|
| POST | `/v1/market/settle` | Règlement atomique vente/revente (custodial). Body : `seller_private_key_b64`, `buyer_private_key_b64`, `asset_sold`, `quantity`, `price_asset?`, `price` |
| POST | `/admin/tokens/create` | + `royalty_bps?`, `royalty_beneficiary?` |
| POST | `/admin/sft/classes` | + `royalty_bps?`, `royalty_beneficiary?` |

## Séquence end-to-end

1. **Créer l'édition** : `POST /admin/sft/classes { collection_id, class_id, decimals:0, max_supply:"10", royalty_bps:2000, royalty_beneficiary:<créateur> }` → cap **enforced** ([[semi-fungibles]]).
2. **1ʳᵉ vente** : `POST /v1/market/settle { asset_sold:"col:class", quantity:"1", price:"100" }` (prix en PMS ou en token custom via `price_asset`).
3. **Revente** : idem — à CHAQUE settlement, le créateur touche `royalty_bps` % du prix, versé par le DAG, atomiquement avec le transfert de l'item.

## Limitation connue (décision produit)

La royalty est enforced **sur les settlements** ; un transfert `TxUtxo` nu du même
actif ne prélève rien. Pour rendre la royalty *totalement* inévitable, il faudrait
restreindre les transferts des actifs royalty-bearing au seul `MarketSettle` (coût :
lookup registre par transfert custom + actifs non-librement-transférables) — non
activé par défaut.

## Interactions

Liens : [[semi-fungibles]] (item vendable plafonné), [[smart-contracts]] (TransferFee —
alternative « frais sur transfert »), [[block-payloads]], [[validation-consensus]],
[[activity-system]], [[compliance]].
