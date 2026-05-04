---
tags: [feature, security, wallet]
created: 2026-05-04
updated: 2026-05-04
version: v0.8.0
---

# HD Wallet (BIP32 / BIP39 / BIP44)

## Résumé

Génération déterministe de N wallets enfants depuis un seul secret maître (mnémonique BIP39 ou seed brute), via BIP32 secp256k1 + chemin BIP44. Permet à un SaaS d'émettre une adresse de dépôt par utilisateur sans stocker N clés privées — un seul `master_seed` en cold storage suffit.

Cas d'usage cible (PROTOCOL.md du SaaS streaming) : un viewer veut acheter des PMS, le serveur dérive `m/44'/PMS_COIN_TYPE'/0'/0/{user_id}` à la volée, montre l'adresse au viewer, le node watcher surveille les TX entrantes vers cette adresse.

## Configuration

Aucune config TOML. Le module est purement library-side. Pour le SaaS :

1. Générer un mnémonique BIP39 (12 ou 24 mots) hors-ligne (cold).
2. Stocker le mnémonique chiffré (HSM, fichier `.enc`, hardware wallet).
3. Au runtime, décrypter brièvement → `master_xprv_from_mnemonic(...)` → dériver le wallet enfant pour l'index voulu → laisser le master sortir de scope (zeroize sur Drop).

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-wallet` | [`src/hd.rs`](../../crates/pms-wallet/src/hd.rs) | Module BIP32/BIP39/BIP44, dérivation enfant, helpers |
| `pms-wallet` | [`src/wallet.rs`](../../crates/pms-wallet/src/wallet.rs) | `Wallet::from_hex` réutilisé par `derive_child_wallet` |
| `pms-wallet` | [`tests/hd_derivation_test.rs`](../../crates/pms-wallet/tests/hd_derivation_test.rs) | 7 tests d'intégration (déterminisme, unicité, signature) |

Dépendance externe : `bip32 = "0.5"` (RustCrypto, secp256k1 only, no_std-friendly).

## Fonctions Clés

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `master_xprv_from_mnemonic(mnemonic, passphrase)` | [hd.rs](../../crates/pms-wallet/src/hd.rs) | BIP39 → 64-byte seed → BIP32 master `XPrv`. Passphrase = "25e mot" optionnel |
| `master_xprv_from_seed(seed)` | [hd.rs](../../crates/pms-wallet/src/hd.rs) | Direct entry si la seed est déjà disponible (HSM) |
| `derive_child_wallet(master, "m/44'/...")` | [hd.rs](../../crates/pms-wallet/src/hd.rs) | Dérive un `Wallet` complet (secp + X25519) à un chemin BIP32 arbitraire |
| `derive_child_wallet_at_index(master, account, index)` | [hd.rs](../../crates/pms-wallet/src/hd.rs) | Convenience BIP44 `m/44'/PMS_COIN_TYPE'/{account}'/0/{index}` |
| `pms_bip44_path(account, change, index)` | [hd.rs](../../crates/pms-wallet/src/hd.rs) | Construit la string de chemin BIP44 |

## Constantes

| Nom | Valeur | Rationale |
|-----|--------|-----------|
| `PMS_COIN_TYPE` | `0x7FFF_FFFF` (2147483647) | Range "private use" SLIP-44 — temporaire jusqu'à enregistrement officiel. Migration prévue au moment du registration : ré-dérivation forcée des wallets utilisateurs. |

## Pattern d'usage SaaS

```rust
use pms_wallet::hd;

// Au boot du serveur — décrypter le master à partir du fichier .enc
let mnemonic = decrypt_master_mnemonic_from_hsm()?;
let master = hd::master_xprv_from_mnemonic(&mnemonic, "")?;

// Pour chaque nouvel utilisateur : dériver une adresse de dépôt
let user_index: u32 = next_index_from_db();
let user_wallet = hd::derive_child_wallet_at_index(&master, 0, user_index)?;
let deposit_addr = user_wallet.get_address("8e");

// Stocker en DB : (user_id, user_index, deposit_addr) — pas la clé privée.
// La clé privée sera re-dérivée à la demande pour signer un payout.
```

## Tests

Run : `cargo test -p pms-wallet --test hd_derivation_test -- --nocapture`

| Test | Vérifie |
|------|---------|
| `known_mnemonic_yields_stable_first_address` | "abandon × 11 + about" + path 0 → adresse stable + adresse différente pour index 1 |
| `one_thousand_derived_addresses_all_unique` | 1000 dérivations → 1000 adresses + secp pubkeys + X25519 pubkeys uniques. Pas de collision |
| `different_accounts_yield_different_addresses` | Multi-tenant : `account=0,1,99` au même `index=42` → 3 adresses différentes |
| `passphrase_produces_independent_chain` | BIP39 passphrase ("25e mot") → chaîne séparée, protège même si les 12 mots fuitent |
| `derived_wallet_can_sign_tx_with_network_binding` | Wallet HD signe une TX avec network_id Phase 1 ; cross-network msg differs |
| `arbitrary_path_works` | Chemins non-BIP44 (`m/0'/0/0`) acceptés pour usages spéciaux (treasury) |
| `invalid_inputs_error_cleanly` | Mauvais mnémonique / chemin → `Err`, jamais panic ou silent default |

## Limitations actuelles

### Pas de mode watch-only (Phase 2.5)

Le mode "vrai watch-only" (xpub sur le serveur, master jamais en ligne) n'est pas supporté. Raison technique : l'adresse PMS = `bech32m(secp_pubkey_hash || x25519_pubkey)`. La clé X25519 est dérivée de la clé privée secp via HKDF (cf. [[wallet-encryption]]) — un xpub seul ne permet pas de la reconstruire.

Deux designs envisagés pour Phase 2.5 :
1. **Dérivation parallèle SLIP-0010 X25519** : produire une seconde xpub pour la clé d'encryption ; le watch-only consumer combine les deux.
2. **Adresse "deposit-only"** sans composante X25519 : reçoit du PMS mais ne peut pas déchiffrer les payloads privés entrants (acceptable pour des adresses de dépôt qui ne reçoivent jamais d'encrypted TXs).

D'ici là, le pattern "master chiffré at-rest, déchiffré à la demande pour dériver" donne ~95% du bénéfice d'un cold/hot setup standard.

### SLIP-44 non enregistré

Le `PMS_COIN_TYPE` actuel est dans la range "private use" de SLIP-44. Au moment de l'enregistrement officiel, une migration tool devra ré-dériver les adresses des utilisateurs existants à partir du nouveau coin type — incompatible avec les anciennes dérivations.

## Interactions

- [[wallet-encryption]] — base `Wallet` (secp + X25519) et `derive_x25519_pair_from_private_key_b64` réutilisée par `derive_child_wallet`
- [[validation-consensus]] — Phase 1 cross-chain replay protection : les wallets HD signent en respectant le `network_id` de la chaîne (testé dans `derived_wallet_can_sign_tx_with_network_binding`)
- [[multi-ledger]] — un compte BIP44 par ledger custom (recommandé) : `account=0` pour main, `account=1+` pour customs

## Hors scope

- Mode watch-only complet (Phase 2.5)
- Plugin hardware wallet Ledger / Trezor (Phase ultérieure, gros chantier dédié)
- Recovery via Shamir SLIP-0039 (envisagé pour treasury wallets, pas pour user deposit addresses)
