---
tags: [feature]
created: 2026-02-13
updated: 2026-03-03
version: v0.1.0
---

# Wallet Factory (Gestion Custodiale des Wallets)

## Résumé

La Wallet Factory fournit un ensemble d'endpoints API permettant la création, la restauration et l'utilisation custodiale de wallets sur le réseau PMS. Elle permet aux clients (SDK, dashboard, applications tierces) de générer des paires de clés (ECDSA secp256k1 + X25519), de restaurer des wallets via phrase mnémonique BIP39 ou clé privée hexadécimale, et d'effectuer des transferts one-shot entièrement gérés côté serveur (sélection d'UTXOs, calcul de frais, signature, chiffrement et broadcast). Un endpoint faucet réservé aux environnements de développement/testnet permet également de minter du PMS natif vers une adresse arbitraire.

## Dates

| | Date |
|---|---|
| Créée | 2026-02-13 |
| Dernière mise à jour | 2026-03-03 |
| Version d'introduction | v0.1.0 |

## Configuration

La Wallet Factory n'a pas de section de configuration dédiée. Elle s'appuie sur la configuration globale du nœud :

| Paramètre | Fichier / Section | Rôle |
|-----------|-------------------|------|
| `address.hrp` | `settings.toml` > `[address]` | Préfixe HRP Bech32m pour la génération d'adresses (ex: `8e`) |
| `fees.ratio`, `fees.base_fee` | `settings.toml` > `[fees]` | Policy de frais appliquée par `wallet_send_simple` |
| `fees.treasury_addresses` | `settings.toml` > `[fees]` | Adresses treasury pour la réception des frais |
| `admin.wallet_addresses` | `settings.toml` > `[admin]` | Adresses admin (fallback pour les frais si treasury vide) |
| `network.mode` | `server.toml` > `[network]` | Mode réseau ; le faucet est interdit en mode `prod` |
| `RuntimeConfig` | RocksDB (hot-swap) | Surcharge dynamique des frais (`fee_rate_bps`, `base_fee`, `fee_tiers`) |
| `effective_fees.gas_per_tx` | Per-ledger | Coût gas par transaction pour les [[multi-ledger|ledgers custom]] (non-main) |

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-server` | `crates/pms-server/src/api_fn/wallet_factory.rs` | Handlers HTTP : `wallet_create`, `wallet_restore_mnemonic`, `wallet_restore_private_key`, `wallet_send_simple`, `faucet_mint`, `wallet_from_b64` |
| `pms-server` | `crates/pms-server/src/api_fn/mod.rs` | Déclaration du module `wallet_factory` |
| `pms-server` | `crates/pms-server/src/api.rs` | Enregistrement des routes dans le Router Axum (`build_ledger_scoped_routes`, `build_ledger_admin_routes`) |
| `pms-server` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Helpers partagés : `load_fee_policy`, `select_utxos`, `get_block_parents`, `forge_and_sign_block`, `persist_and_broadcast`, `apply_utxo_delta`, `create_reward_block`, `try_consume_gas` |
| `pms-server` | `crates/pms-server/src/api_fn/wallet.rs` | Endpoints de consultation : `wallet_balance`, `balance_by_address`, `get_utxos_by_address` |
| `pms-server` | `crates/pms-server/src/api_fn/nft.rs` | Utilise `wallet_from_b64` pour les burns [[nft-system|NFT]] custodials (`burn_nft_simple`, `burn_nft_batch_simple`) |
| `pms-server` | `crates/pms-server/src/api_fn/activity.rs` | Indexation d'[[activity-system|activité]] pour les transferts chiffrés émis par `wallet_send_simple` |
| `pms-wallet` | `crates/pms-wallet/src/wallet.rs` | Structure `Wallet` : génération, restauration, dérivation de clés, encodage d'adresses Bech32m |
| `pms-wallet` | `crates/pms-wallet/src/backends/k256.rs` | Implémentation `SignerBackend` pour secp256k1 (ECDSA via k256) |
| `pms-wallet` | `crates/pms-wallet/src/types.rs` | Trait `SignerBackend` : `sign`, `verify`, `from_seed`, `encoded_private_key`, `encoded_public_key` |
| `pms-wallet` | `crates/pms-wallet/src/helpers.rs` | Helpers de construction de transactions et sélection pondérée d'adresses admin |
| `pms-types-payload` | (crate externe) | `EncryptedPayload::encrypt_for_plain` : chiffrement multi-destinataire X25519 |

## Fonctions Clés

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `wallet_create` | `crates/pms-server/src/api_fn/wallet_factory.rs` | Génère un nouveau wallet (BIP39, 24 mots) ou importe une clé privée hex existante. Retourne adresse, clés privées (b64 + hex), clé publique, paire X25519 (pub + sk), et mots mnémoniques. |
| `wallet_restore_mnemonic` | `crates/pms-server/src/api_fn/wallet_factory.rs` | Restaure un wallet depuis 24 mots BIP39. Valide le dictionnaire anglais, re-dérive seed et paire de clés. |
| `wallet_restore_private_key` | `crates/pms-server/src/api_fn/wallet_factory.rs` | Restaure un wallet depuis une clé privée hexadécimale (64 chars / 32 bytes). Ne retourne pas de mnémonique. |
| `wallet_send_simple` | `crates/pms-server/src/api_fn/wallet_factory.rs` | Envoi custodial one-shot : reconstruit le wallet depuis `private_key_b64`, calcule les frais (FeePolicy + RuntimeConfig), sélectionne les UTXOs (largest-first), construit la transaction, signe, chiffre (X25519 multi-destinataire), forge le bloc, mine le PoW, persiste, met à jour le cache UTXO, indexe l'activité, crée le bloc de reward, et broadcast. Supporte les tokens custom (`asset_id`) avec frais en PMS natif. |
| `faucet_mint` | `crates/pms-server/src/api_fn/wallet_factory.rs` | Faucet dev/testnet : mint du PMS natif vers une adresse arbitraire. Rejeté en mode `prod`. Forge un bloc Mint signé par le node wallet. |
| `wallet_from_b64` | `crates/pms-server/src/api_fn/wallet_factory.rs` | Utilitaire : reconstruit un `Wallet` à partir d'une `private_key_b64` (decode base64 -> hex -> `Wallet::from_hex`). Réutilisé par `burn_nft_simple` et `burn_nft_batch_simple`. |
| `Wallet::generate` | `crates/pms-wallet/src/wallet.rs` | Génération de wallet avec entropie BIP39 (24 mots, `OsRng`). Dérive ECDSA secp256k1 + X25519 via HKDF-SHA256. |
| `Wallet::from_word_list` | `crates/pms-wallet/src/wallet.rs` | Restauration depuis 24 mots BIP39 (validation, normalisation, dérivation de seed). |
| `Wallet::from_hex` | `crates/pms-wallet/src/wallet.rs` | Restauration depuis une clé privée hexadécimale brute (32 bytes). Pas de mnémonique. |
| `Wallet::get_address` | `crates/pms-wallet/src/wallet.rs` | Calcule l'adresse Bech32m : `SHA256(ECDSA_pub)[0..20] || X25519_pub` (52 bytes payload). |
| `Wallet::derive_x25519_pair_from_private_key_b64` | `crates/pms-wallet/src/wallet.rs` | Dérive la paire X25519 depuis la clé privée ECDSA via HKDF-SHA256 (`pms/x25519-sk/v1`). Source unique de vérité pour la clé de chiffrement. |
| `Wallet::x25519_sk_hex` | `crates/pms-wallet/src/wallet.rs` | Retourne la clé secrète X25519 hex (dérivée à la volée, jamais stockée). |
| `select_utxos` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Sélection d'UTXOs largest-first depuis le cache RAM. Filtre par `asset_id`. |
| `try_consume_gas` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Consomme du gas depuis le [[economics|gas pool]] du ledger (ledgers custom uniquement). |
| `create_reward_block` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Crée un bloc Reward distribuant les frais au coordinateur et au treasury via la [[fee-distribution]]. |

## Endpoints API

| Méthode | Path | Auth | Description |
|---------|------|------|-------------|
| POST | `/v1/wallet/create` | [[api-key-authentication|API Key]] | Génère un nouveau wallet ou importe depuis une clé privée hex. Body optionnel `{ "import_hex": "..." }`. Retourne adresse, clés, X25519, et mnémonique (si généré). |
| POST | `/v1/wallet/restore/mnemonic` | [[api-key-authentication|API Key]] | Restaure un wallet depuis 24 mots BIP39. Body : `{ "mnemonic": "mot1 mot2 ... mot24" }`. |
| POST | `/v1/wallet/restore/private-key` | [[api-key-authentication|API Key]] | Restaure un wallet depuis une clé privée ECDSA hex (64 chars). Body : `{ "private_key_hex": "a1b2..." }`. |
| POST | `/v1/wallet/send-simple` | [[api-key-authentication|API Key]] | Envoi custodial one-shot. Body : `{ "private_key_b64", "to", "amount", "asset_id?" }`. Retourne `{ "block_id", "fee" }` (HTTP 201). |
| POST | `/admin/faucet` | Admin Token | Mint PMS natif (dev/testnet uniquement). Body : `{ "to", "amount" }`. Retourne `{ "block_id", "amount" }` (HTTP 201). Rejeté HTTP 403 en mode prod. |

Tous les endpoints wallet factory (sauf `/admin/faucet`) sont disponibles sur les sous-ledgers via le préfixe `/l/{ledger_id}/`.

## Interactions

### Cache UTXO en RAM

`wallet_send_simple` interagit directement avec le cache UTXO en mémoire via `NetDagAdapter` :
- **Lecture** : `select_utxos` appelle `adapter.utxos_by_address()` pour la coin selection (RAM, O(1) par adresse).
- **Écriture** : `apply_utxo_delta` supprime les UTXOs consommés (`remove_utxo`) et ajoute les nouveaux outputs (`add_utxo`) directement dans le cache RAM après persistance du bloc.

### Chiffrement X25519 multi-destinataire

`wallet_send_simple` chiffre automatiquement le payload de transaction pour plusieurs destinataires :
1. L'expéditeur (pour qu'il puisse relire ses propres transactions).
2. L'adresse de destination (X25519 pub extraite de l'adresse Bech32m via `pms_wallet::decode_address`).
3. Les adresses admin/treasury (si elles apparaissent dans les outputs de frais).

Le chiffrement utilise `EncryptedPayload::encrypt_for_plain` qui génère un secret éphémère et l'encapsule pour chaque clé publique X25519 destinataire.

### Calcul de frais

Le calcul de frais dans `wallet_send_simple` suit cette priorité :
1. **RuntimeConfig** (RocksDB, hot-swap via `/admin/config`) : `fee_rate_bps`, `base_fee`, `fee_tiers`.
2. **EffectiveFees** (per-ledger) : surcharge par [[multi-ledger|ledger]] si applicable.
3. **FeePolicy** : calcul via ratio proportionnel ou tiers gradués.
4. **Frais de gas** (`try_consume_gas`) : pour les [[multi-ledger|ledgers custom]] (non-main), consomme du [[economics|gas pool]] avant la transaction.

Les frais sont toujours payés en PMS natif, même pour les transferts de tokens custom. Dans ce cas, des UTXOs PMS supplémentaires sont sélectionnés pour couvrir les frais, et le change PMS est retourné à l'expéditeur.

### Indexation d'activité

Après persistance d'un transfert chiffré, `wallet_send_simple` indexe l'[[activity-system|activité]] pour toutes les adresses impliquées :
- Utilise `extract_involved_addresses` et `extract_involved_with_category` sur le payload en clair (avant chiffrement).
- Ajoute l'adresse de l'expéditeur si absente (résolu avant la dépense des UTXOs pour éviter la perte du lien input->adresse).
- Pré-calcule les `ActivityItem` via `precompute_all_items` pour des requêtes O(1) sur `/v1/wallet/{address}/activity`.

### Bloc de reward

Après chaque transfert payant, `wallet_send_simple` appelle `create_reward_block` qui :
1. Vérifie que le nœud est le coordinateur.
2. Calcule la répartition des frais (coordinateur + treasury) via `FeeDistributionConfig`.
3. Forge un bloc `Reward` avec les outputs de [[fee-distribution|distribution]].
4. Enregistre les UTXOs de reward dans le cache RAM pour que les récipients puissent les dépenser.

### Réutilisation par d'autres modules

La fonction utilitaire `wallet_from_b64` est réutilisée par le module [[nft-system|NFT]] (`nft.rs`) pour les endpoints de burn custodial (`burn_nft_simple`, `burn_nft_batch_simple`), évitant la duplication de la logique de reconstruction de wallet.

## Cryptographie

| Algorithme | Usage | Bibliothèque |
|------------|-------|--------------|
| ECDSA secp256k1 | Signature de transactions et de blocs | `k256` (RustCrypto) |
| BIP39 | Génération de phrases mnémoniques (24 mots, anglais) | `bip39` |
| X25519 (Curve25519) | Chiffrement asymétrique des payloads | `x25519-dalek` |
| HKDF-SHA256 | Dérivation de la clé X25519 depuis la clé ECDSA | `hkdf` + `sha2` |
| Bech32m | Encodage des adresses (HRP + hash20 + X25519 pub) | `bech32` |

## Tests

| Test | Fichier | Description |
|------|---------|-------------|
| `wallet_create_returns_x25519_sk` | `crates/pms-server/tests/wallet_x25519_sk.rs` | Vérifie que `/v1/wallet/create` retourne `x25519_sk_hex` valide et cohérent avec `x25519_pub_hex`. |
| `wallet_restore_mnemonic_returns_x25519_sk` | `crates/pms-server/tests/wallet_x25519_sk.rs` | Vérifie que `/v1/wallet/restore/mnemonic` retourne les mêmes clés X25519 que la création originale. |
| `wallet_restore_private_key_returns_x25519_sk` | `crates/pms-server/tests/wallet_x25519_sk.rs` | Vérifie que `/v1/wallet/restore/private-key` retourne les mêmes clés X25519 que la création originale. |
| `wallet_send_tx_injects_fee_and_admin_can_decrypt_fee_utxo` | `crates/pms-server/tests/wallet_send_fees.rs` | Vérifie que le serveur ajoute automatiquement la clé X25519 admin aux destinataires du chiffrement pour que l'admin puisse décrypter les frais. |
| `wallet_balance_returns_correct_balance_from_ram` | `crates/pms-server/tests/wallet_balance_fast.rs` | Vérifie que le solde retourné par `/wallet/balance` correspond au montant minté dans le cache RAM. |
| `wallet_balance_returns_zero_for_unknown_address` | `crates/pms-server/tests/wallet_balance_fast.rs` | Vérifie qu'une adresse inconnue retourne un solde de 0. |
| `wallet_balance_matches_v1_balance` | `crates/pms-server/tests/wallet_balance_fast.rs` | Vérifie la cohérence entre `/wallet/balance` et `/v1/balance` pour la même adresse. |
