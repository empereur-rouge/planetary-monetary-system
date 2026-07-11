---
tags: [feature]
created: 2026-07-11
updated: 2026-07-11
version: v0.32.0
---

# Formes d'adresse & récupération des fonds « piégés » (hex ↔ bech32m)

## Résumé

Un wallet à clé unique a **deux encodages d'adresse-propriétaire équivalents** qui
peuvent chacun indexer des UTXOs :

- **bech32m** — la forme **canonique** que le nœud dérive nativement
  (`Wallet::get_address(hrp)` = `bech32m(hrp, sha256(secp_pub)[..20] ‖ x25519_pub)`) ;
- **pubkey secp256k1 hex brute** — la forme `address` historique du SDK TypeScript
  (`toHex(uncompressed_pubkey)`, 130 chars).

Comme l'index d'adresse est keyé par le **string-propriétaire exact** écrit à la
création de l'output, des fonds mintés vers la forme hex étaient invisibles aux
chemins de dépense qui dérivent la forme bech32m → `3001 InsufficientBalance`,
fonds **indépensables/invendables** (« piégés »). Cette feature lève le piège sur
trois fronts, plus un garde-fou et un endpoint :

1. **Coin-selection multi-forme** — la sélection unit les formes du dépensier.
2. **Validation de settlement par identité** — le consensus attribue les flux par
   identité d'adresse, pas par string brute.
3. **Garde-fou de rejet** — un `to` non canonique est rejeté au mint (fail-fast).
4. **Endpoint adresse canonique** — le client obtient la forme bech32m
   autoritaire du nœud (dérivation locale impossible, cf. §x25519).

## Le piège (précondition)

`send-simple` / `market/settle` / `wallet/token/burn` reconstruisent le wallet du
dépensier depuis sa clé privée puis appellent `get_address(hrp)` → **bech32m**. Si
les fonds ont été mintés vers la **pubkey hex** (ex : SDK faisant
`mint({to: wallet.address})`), ils sont indexés sous le hex → la sélection bech32m
ne les voit pas. L'autorisation de dépense, elle, acceptait **déjà** les deux
formes ([[wallet-encryption|ownership binding C-1]], `unlock_matches_address`) —
l'asymétrie n'était qu'au niveau **indexation/lookup** et **comptabilité de
settlement**.

## 1. Coin-selection multi-forme

`spend_address_forms(hrp, secp_pub, x25519_pub)` (crate `pms-wallet`) renvoie les
formes-propriétaire équivalentes **canonique d'abord** : `[bech32m, hex, 0x+hex]`.
`select_utxos_multi(adapter, &formes, target, asset)` essaie la forme canonique
seule via le fast-path `select_utxos` (perf inchangée dans le cas commun) ; si elle
ne couvre pas la cible, il **unit** les UTXOs de toutes les formes (full-scan,
largest-first, dédup par `OutputId`, exclusion des time-locks). Câblé dans
`wallet_send_simple`, `market_settle` (vendeur + acheteur), `wallet_burn_token` —
chacun construit ses candidats depuis **son propre** wallet (impossible de tirer les
UTXOs d'un tiers).

## 2. Validation de settlement par identité

`validate_settlement` ([[marketplace-royalty]]) comptait les flux nets par string
d'adresse brute → un item détenu sous la forme hex n'était pas attribué au `seller`
déclaré en bech32m (rejet « seller must net-relinquish »). Désormais les flux sont
comptés par **`address_identity(addr)`** :

| Forme | Identité |
|-------|----------|
| pubkey secp hex (33/65 octets, `0x` opt.) | `sha256(pubkey)[..20]` hex |
| bech32m | son hash20 (20 premiers octets du payload) |
| autre (multisig `msig1…`, inconnue) | le string minuscule (aucun collapse) |

C'est la **même** relation forme↔hash20 que `unlock_matches_address` : les deux
formes d'un wallet ont le même hash20, donc la même identité. **Loosening pur** —
l'enforcement exact du split royalty/prix est intact (un item d'un tiers ne compte
pas comme relinquish du vendeur). `DAG_VERSION 3.15.0 → 3.16.0`.

## 3. Garde-fou de rejet (fail-fast au mint)

`validate_recipient_address(to)` (module `api_fn::recipient`) refuse un
`to`/`owner_address` qui n'est **aucune** forme dépensable **avant** de forger le
bloc — sinon fonds piégés. Accepte : bech32m single-key, multisig (`msig1`+40 hex),
pubkey secp hex **minuscule** (33/65 octets, point valide via
`is_valid_secp_pubkey_hex`). Rejette : garbage, pubkey tronquée/hors-courbe, typo de
checksum bech32m, casse non canonique (`ApiError::InvalidAddress` / `2010`). Câblé
dans `admin_mint_sft`, `admin_mint_token`, `custodial_mint(_prepare)`, `mint_nft`,
`faucet_mint`, `wallet_send_simple`, `prepare_tx`, `onramp`.

## 4. Endpoint adresse canonique & dérivation x25519

Un client **ne peut pas** calculer localement la bech32m canonique du nœud : le
bech32m embarque la clé x25519 dérivée **côté nœud**
(`derive_x25519_pair_from_private_key_b64`, HKDF `salt=None, info="pms/x25519-sk/v1"`),
**différente** de la dérivation locale du SDK (HKDF `salt="pms-x25519",
info="encryption"`). Une bech32m calculée avec la x25519 du SDK re-piégerait les
fonds (forme non dérivée par le nœud). `POST /v1/wallet/canonical-address` renvoie la
forme **autoritaire** (adresse + `public_key_hex` + `x25519_pub_hex` nœud) depuis une
clé privée, **sans aucun secret** en réponse. Le SDK expose
`getCanonicalAddress(wallet)`.

## Solde unifié par forme d'adresse (P5)

La coin-selection unit les formes au **moment de la dépense**, mais l'index
d'adresse et les caches de solde restent keyés par **string brute** (rekeyage par
identité = coût sur le hot-path consensus, écarté). Pour que le **solde** reflète
ce qui est réellement dépensable, `POST /v1/balance` accepte un champ optionnel
**`public_key_hex`** :

- sans le champ → comportement historique (solde du seul string interrogé) ;
- avec le champ → le solde somme l'adresse interrogée + la forme hex de la pubkey
  (`balance_union_forms`), la pubkey devant correspondre à l'adresse (même hash20,
  sinon `400`). Interroger par le **bech32m canonique** + `public_key_hex` couvre
  les deux buckets réels (bech32m nœud + hex SDK) — sans secret, sans x25519 (la
  bech32m n'est pas reconstructible depuis la pubkey seule, mais l'adresse
  interrogée LA fournit).

Restent par-forme (pas de body pour porter la pubkey) : `GET /v1/wallet/{address}/utxos`
et les soldes par path-adresse → passer par `/v1/balance` pour l'union.

## Cohérence des dérivations

La relation **pubkey → hash20** (`SHA256(pubkey)[..20]`) dont dépendent l'adresse
bech32m, le binding d'autorisation et l'identité de comptabilité est centralisée
dans un unique `pms_wallet::pubkey_hash20` — `make_address`, `Wallet::get_address`,
`unlock_matches_address` et `address_identity` l'appellent tous, ce qui empêche
une divergence silencieuse qui re-piégerait les fonds.

## Configuration

Aucune. Actif par défaut. `[address].hrp` (config) contrôle le préfixe bech32m.

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-wallet` | `src/wallet.rs` | `spend_address_forms`, `is_valid_secp_pubkey_hex`, `decode_address`, `make_address` |
| `pms-server` | `src/api_fn/tx_helpers/coin_selection.rs` | `select_utxos_multi` (union multi-forme) |
| `pms-server` | `src/api_fn/recipient.rs` | `validate_recipient_address` (garde-fou) |
| `pms-server` | `src/api_fn/wallet_factory.rs` | `wallet_canonical_address` (endpoint), `wallet_send_simple`, `faucet_mint` |
| `pms-server` | `src/api_fn/market.rs` | `market_settle` (candidats seller/buyer) |
| `pms-server` | `src/api_fn/{sft,token,custodial,nft,onramp,transaction,token_burn}.rs` | sites câblés (sélection + garde-fou) |
| `pms-core` | `src/validations/ownership.rs` | `address_identity` (identité forme↔hash20), `unlock_matches_address` |
| `pms-core` | `src/validations/market.rs` | `validate_settlement` (comptabilité par identité) |
| `pms-core` | `src/validations/transactions.rs` | `validate_token_burn_async` (owner par identité) |
| `pms-storage` | `src/migrations.rs` | `DAG_VERSION 3.16.0` |

## Fonctions Clés

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `spend_address_forms` | `pms-wallet/src/wallet.rs` | formes-propriétaire ordonnées canonique-d'abord |
| `select_utxos_multi` | `coin_selection.rs` | sélection unifiée multi-forme, fast-path préservé |
| `address_identity` | `pms-core/.../ownership.rs` | forme d'adresse → identité stable (hash20) ; partagée par settlement + burn |
| `validate_recipient_address` | `api_fn/recipient.rs` | rejet fail-fast des `to` non canoniques |
| `is_valid_secp_pubkey_hex` | `pms-wallet/src/wallet.rs` | pubkey secp valide (longueur + point courbe) |
| `wallet_canonical_address` | `wallet_factory.rs` | endpoint adresse canonique (sans secret) |

## Endpoints API

| Méthode | Path | Description |
|---------|------|-------------|
| POST | `/v1/wallet/canonical-address` | adresse bech32m canonique + pubkeys depuis une clé privée (API-key, aucun secret en réponse) |

## Interactions

Lié à : [[utxo-system]] (index d'adresse, coin-selection), [[marketplace-royalty]]
(validation de settlement), [[wallet-factory]] (endpoints wallet, send-simple,
faucet), [[wallet-encryption]] (dérivation x25519, ownership binding), [[nft-system]]
et [[token-system]] (garde-fou au mint).

## Versioning

`software 0.31.0 → 0.32.0` (MINOR) · `DAG_VERSION 3.15.0 → 3.16.0` (loosening,
auto-migrating, pas de wipe) · `API_VERSION 39 → 40` · SDK `0.11.0 → 0.12.0`.

> ⚠️ **Mixed-version P2P** : un nœud < 3.16.0 REJETTE un `market/settle` dont un
> input est détenu sous la forme pubkey-hex, qu'un nœud ≥ 3.16.0 accepte →
> divergence consensus. Upgrade coordonné requis avant d'émettre de tels
> settlements.
