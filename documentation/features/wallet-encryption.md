---
tags: [feature, crypto, security]
created: 2025-12-28
updated: 2026-03-11
version: v0.3.0
---

# Wallet & Encryption

## Resume

Le systeme de wallet et chiffrement de PMS fournit une pile cryptographique complete pour la gestion d'identite, la signature de transactions, et le chiffrement de payloads. Il s'appuie sur des standards eprouves :

- **BIP-39** (24 mots) pour la generation deterministe de cles a partir d'une phrase mnemonique.
- **secp256k1 (ECDSA)** via la librairie `k256` pour la signature et la verification de blocs/transactions.
- **X25519 (Curve25519)** pour l'echange de cles Diffie-Hellman (chiffrement asymetrique).
- **AES-256-GCM** pour le chiffrement symetrique des payloads avec engagement (commitment SHA-256).
- **Bech32m** pour l'encodage d'adresses (20 bytes hash ECDSA + 32 bytes cle publique X25519).

Le design separe les responsabilites : la cle ECDSA signe, la cle X25519 chiffre. Les deux sont derivees de la meme source (seed BIP-39 ou cle privee ECDSA via HKDF), garantissant une source unique de verite cryptographique.

## Architecture Cryptographique

```
Mnemonic (24 mots BIP-39)
        |
        v
   Seed (32+ bytes)
        |
        +---> SigningKey (secp256k1/ECDSA)
        |         |
        |         +---> VerifyingKey (pub ECDSA, hex)
        |         |        |
        |         |        +---> SHA-256(pub)[0..20] = hash20
        |         |
        |         +---> private_key_b64 (base64 storage)
        |
        +---> HKDF-SHA256(private_key, "pms/x25519-sk/v1")
                  |
                  +---> StaticSecret (X25519)
                           |
                           +---> PublicKey (X25519, hex)

Adresse Bech32m = encode(hrp, hash20 || x25519_pub)  [52 bytes payload]
```

## Format d'Adresse Bech32m

L'adresse PMS utilise le format Bech32m (BIP-350) avec un payload de 52 bytes :

| Segment | Taille | Contenu |
|---|---|---|
| hash20 | 20 bytes | `SHA-256(pub_ecdsa_uncompressed)[0..20]` |
| x25519_pub | 32 bytes | Cle publique X25519 (Curve25519) |

L'adresse encode donc simultanement l'identite de signature (hash ECDSA) et la cle de chiffrement (X25519), permettant a quiconque connait l'adresse du destinataire d'envoyer des payloads chiffres sans echange prealable de cles.

Le HRP (Human-Readable Part) est configurable (defaut : `"8e"`), permettant de distinguer les reseaux (dev, testnet, mainnet).

## Chiffrement des Payloads (EncryptedPayload)

Le systeme de chiffrement utilise un schema hybride multi-destinataires :

```
          Expediteur                                    Destinataire(s)
             |                                               |
  1. Genere DEK (32 bytes aleatoires)                        |
  2. Genere nonce AES-GCM (12 bytes)                         |
  3. Chiffre payload avec AES-256-GCM(DEK, nonce, AAD)       |
  4. Calcule commitment = SHA-256(plaintext)                  |
  5. Genere cle ephemere X25519 (per-message)                 |
  6. Pour chaque destinataire :                               |
     a. ECDH(ephem_sk, recipient_pk) -> shared_secret         |
     b. HKDF(shared, "pms-dek-wrap") -> KEK + kid             |
     c. AES-GCM(KEK, DEK) -> wrapped_key                      |
                                                              |
     Dechiffrement :                                          |
     a. ECDH(my_sk, ephem_pub) -> shared_secret         <----|
     b. HKDF(shared) -> KEK + kid                             |
     c. Match kid -> unwrap DEK                                |
     d. AES-256-GCM(DEK, ciphertext) -> plaintext             |
     e. Verifie commitment SHA-256                             |
```

### Proprietes de Securite

| Propriete | Mecanisme |
|---|---|
| Confidentialite | AES-256-GCM avec DEK aleatoire par message |
| Authenticite/Integrite | AEAD (GCM tag) + AAD |
| Forward secrecy | Cle ephemere X25519 par message |
| Multi-destinataires | DEK enveloppee (wrapped) independamment pour chaque recipient |
| Anonymat destinataires | `kid` opaque (16 bytes HKDF) au lieu de la cle publique du recipient |
| Engagement | `commitment = SHA-256(plaintext)` verifie apres dechiffrement |
| Hygiene memoire | `zeroize` sur DEK et cles ephemeres apres usage |

### Structure EncryptedPayload

```rust
pub struct EncryptedPayload {
    pub scheme: String,           // "x25519+aes256gcm"
    pub key_version: u32,         // rotation de cle (actuellement 1)
    pub aad: AAD,                 // { len_hint: u32 }
    pub commitment: String,       // hex(sha256(plaintext))
    pub ciphertext_b64: String,   // AES-256-GCM(ct) en base64
    pub recipients: Vec<KeyWrap>, // DEK enveloppee par destinataire
    pub nonce_b64: String,        // nonce 12 bytes en base64
}

pub struct KeyWrap {
    pub kid: String,             // 16 bytes HKDF, hex (identifiant opaque)
    pub ephem_pub: String,       // pk ephemere X25519 (hex)
    pub wrapped_key_b64: String, // DEK chiffree via KEK
    pub kw_nonce_b64: String,    // nonce GCM du wrap
}
```

## Signature de Blocs

La verification de signature de blocs (`verify_block_signature`) assure :

1. **Authenticite** : le bloc a ete signe par le detenteur de la cle privee correspondante.
2. **Integrite** : aucune donnee du bloc (parents, payload, nonce, network_id, protocol_version) n'a ete modifiee.
3. **Non-repudiation** : le signataire ne peut pas nier avoir signe le bloc.

Le message canonique est construit via `canonical_wireblock_message()` qui serialise une vue deterministe du `WireBlock` en JSON, garantissant que le meme bloc produit toujours le meme message a signer.

## Crates et Fichiers

| Crate | Fichier | Role |
|---|---|---|
| `pms-wallet` | `crates/pms-wallet/src/wallet.rs` | `Wallet` struct : generation (BIP-39 24 mots), import (mnemonic, hex, seed, fichier), derivation X25519 via HKDF, encodage adresse Bech32m, balance UTXO |
| `pms-wallet` | `crates/pms-wallet/src/types.rs` | `SignerBackend` trait, `SignError`, `VerifyError` |
| `pms-wallet` | `crates/pms-wallet/src/backends/k256.rs` | Implementation `SignerBackend` pour secp256k1/ECDSA via `k256`. `sign()`, `verify()`, `from_seed()` |
| `pms-wallet` | `crates/pms-wallet/src/helpers.rs` | `build_utxo_tx_with_fee_checked()`, `pick_admin_address_weighted()`, `pick_admin_wallet_weighted()` |
| `pms-wallet` | `crates/pms-wallet/src/utils/utxo_store.rs` | `gather_wallet_utxos_dec()` avec dechiffrement, `select_utxos_dec()` |
| `pms-wallet` | `crates/pms-wallet/src/utils/signing_wire.rs` | `canonical_wireblock_message()` - message canonique pour signature de blocs |
| `pms-wallet` | `crates/pms-wallet/src/history.rs` | `HistoryEntry`, `try_decrypt_encrypted_reward()` - historique avec dechiffrement |
| `pms-core` | `crates/pms-core/src/crypto/crypto.rs` | `verify_block_signature()` - verification ECDSA secp256k1 des blocs |
| `pms-types-payload` | `crates/pms-types-payload/src/encrypted_payload.rs` | `EncryptedPayload` : chiffrement hybride X25519 + AES-256-GCM multi-destinataires |
| `pms-config` | `crates/pms-config/src/treasury_wallets.rs` | `TreasuryWallets`, `sign_treasury_wallets()`, `load_treasury_wallets()` - gestion signee des wallets treasury |

## Fonctions Cles

| Fonction | Fichier | Description |
|---|---|---|
| `Wallet::generate()` | `wallet.rs` | Genere un wallet BIP-39 (24 mots, `OsRng`). Derive ECDSA + X25519. |
| `Wallet::generate_with_entropy(entropy)` | `wallet.rs` | Generation avec entropie fournie (32 bytes). Retourne `(Wallet, mnemonic_string)`. |
| `Wallet::from_word_list(words)` | `wallet.rs` | Restauration depuis 24 mots BIP-39. Valide la phrase avant derivation. |
| `Wallet::from_hex(priv_hex)` | `wallet.rs` | Creation depuis une cle privee hexadecimale (64 chars). Sans mnemonique. |
| `Wallet::from_seed(seed, mnemonic)` | `k256.rs` | Construction depuis seed brute. Derive `SigningKey` -> `VerifyingKey` -> X25519. |
| `Wallet::get_address(hrp)` | `wallet.rs` | Encode l'adresse Bech32m : `hash20(ECDSA_pub) \|\| x25519_pub` (52 bytes). |
| `Wallet::derive_x25519_pair_from_private_key_b64()` | `wallet.rs` | HKDF-SHA256 sur la cle ECDSA avec info `"pms/x25519-sk/v1"` -> paire X25519. |
| `Wallet::balance(store, hrp, scan_limit)` | `wallet.rs` | Calcul du solde via scan UTXO avec dechiffrement X25519. |
| `Wallet::load_from_node_key_file(path)` | `wallet.rs` | Charge un wallet node : cle hex 64 chars (keygen) ou seed binaire 32 bytes. |
| `SignerBackend::sign(message)` | `k256.rs` | Signature ECDSA DER encodee en base64. |
| `SignerBackend::verify(message, signature)` | `k256.rs` | Verification ECDSA. Supporte DER et raw 64-bytes. |
| `decode_address(addr)` | `wallet.rs` | Parse Bech32m -> `(hash20_hex, x25519_pub_hex)`. Valide le variant Bech32m. |
| `make_address(hrp, ecdsa_pub, x25519_pub)` | `wallet.rs` | Construit une adresse Bech32m depuis les composants bruts. |
| `address_candidates(hrp, ecdsa_pub, x25519_pub)` | `wallet.rs` | Retourne toutes les formes possibles d'une adresse (hex, 0x, bech32m) pour le matching. |
| `EncryptedPayload::encrypt_for(plaintext, recipients, len_hint)` | `encrypted_payload.rs` | Chiffrement hybride multi-destinataires. DEK aleatoire + ECDH par recipient. |
| `EncryptedPayload::decrypt_with(recipient_sk_hex)` | `encrypted_payload.rs` | Dechiffrement par le destinataire. Iteree sur les `KeyWrap` par `kid`. Verifie le commitment. |
| `EncryptedPayload::encrypt_for_plain(payload, recipients)` | `encrypted_payload.rs` | Chiffre un `PlainPayload` serialise en JSON pour une liste de recipients. |
| `verify_block_signature(wb)` | `crypto.rs` | Verification ECDSA secp256k1 d'un `WireBlock`. Message canonique -> SHA-256 -> verify. |
| `canonical_wireblock_message(wb)` | `signing_wire.rs` | Construit le message deterministe a signer (JSON canonique du WireBlock). |
| `sign_treasury_wallets(wallets, priv_key_hex)` | `treasury_wallets.rs` | Signe une liste de wallets treasury. Format : `PMS_TREASURY_v1:<w1>,<w2>,...` |
| `load_treasury_wallets(file, coordinator_pk)` | `treasury_wallets.rs` | Charge et verifie la signature Coordinator sur la liste treasury. |

## Interactions

- **[[utxo-system]]** : Le wallet scanne les UTXOs via `gather_wallet_utxos_dec()` en dechiffrant les payloads encrypted. La selection UTXO gloutonne (`select_utxos_dec`) sert a couvrir les montants de transaction.
- **[[config-system]]** : Le HRP d'adresse est configure dans `[address]`. Les cles publiques Coordinator (ECDSA + X25519) sont dans `[validation]`. Le fichier de wallets treasury est dans `[admin]`.
- Les rewards chiffres (`EncryptedRewardOutput`) utilisent `EncryptedPayload` pour masquer les montants distribues aux treasury wallets, ne revelant les details qu'aux destinataires legitimes.
- L'historique wallet (`history.rs`) tente de dechiffrer chaque `EncryptedPayload` rencontre avec la cle X25519 du wallet pour reconstituer les entrees d'historique.
- Le module `pick_admin_address_weighted()` selectionne les adresses treasury avec une probabilite proportionnelle au deficit de balance (reequilibrage automatique).

## Decisions Techniques

1. **Separation ECDSA / X25519** : secp256k1 n'est pas concu pour le chiffrement (pas de schema ECIES standard). X25519 (Curve25519) est le standard pour l'echange de cles. Les deux cles sont liees via HKDF, evitant la gestion de deux seeds independantes.
2. **BIP-39 24 mots** : Standard Bitcoin/Ethereum pour la portabilite. Compatible avec les hardware wallets.
3. **`kid` opaque** : L'identifiant de destinataire est un hash HKDF, pas la cle publique. Cela empeche un observateur de savoir qui sont les destinataires sans posseder la cle privee.
4. **Commitment SHA-256** : Protege contre la malleabilite du ciphertext (meme si AES-GCM fournit deja l'integrite, le commitment ajoute une couche de verification independante).
5. **Zeroize** : Les cles sensibles (DEK, ephemeral SK, KEK) sont mises a zero en memoire apres usage via le crate `zeroize`.
