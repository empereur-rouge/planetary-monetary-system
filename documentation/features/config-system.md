---
tags: [feature, config, governance]
created: 2026-01-08
updated: 2026-03-15
version: v0.4.3
---

# Config System

## Resume

Le systeme de configuration de PMS se compose de deux couches complementaires :

1. **Configuration statique (`Settings`)** : chargee au demarrage depuis des fichiers TOML et des variables d'environnement. Elle definit les parametres structurels du noeud (chemins RocksDB, reseau, TLS, limites, validation, fees initiaux, multi-ledger).

2. **Configuration runtime (`RuntimeConfig`)** : modifiable a chaud par le Coordinator via des blocs signes contenant `PlainPayload::ConfigUpdate`. Persistee dans RocksDB et rechargee au demarrage. Elle controle les parametres economiques du reseau (taux de fees, distribution, minting, burn rate, fees dynamiques).

Le systeme Hot-Swap permet au Coordinator de modifier les parametres reseau sans redemarrage des noeuds, via des transactions signees qui sont validees et appliquees de maniere deterministe par chaque noeud du reseau.

## Architecture

```
+------------------------------------------------------------------+
|                    Configuration Statique                         |
|                                                                  |
|  load_config() / load_config_with(path)                          |
|    1. Fichier $PMS_CONFIG (si defini)                             |
|    2. Fallbacks: ./config.dev.toml, etc/config/config.dev.toml,  |
|       /etc/pms/config.dev.toml                                   |
|    3. Variables d'env: PMS__SECTION__KEY                          |
|                                                                  |
|  -> Settings { rocks, network, address, admin, client, tls,      |
|                limits, auth, secrets, validation, fees, p2p,      |
|                ledgers }                                          |
+------------------------------------------------------------------+
                              |
                              v
+------------------------------------------------------------------+
|                  Configuration Runtime (Hot-Swap)                 |
|                                                                  |
|  RuntimeConfig (persiste dans RocksDB)                           |
|    - Chargee au demarrage depuis RocksDB                         |
|    - Modifiee via PlainPayload::ConfigUpdate dans un bloc signe  |
|    - Validee (fee split, tiers, distribution) avant application  |
|    - Historique complet via ConfigHistoryEntry                    |
|                                                                  |
|  ConfigUpdate (enum, 18 variantes + BatchUpdate)                 |
|    - Chaque variante modifie un parametre specifique             |
|    - BatchUpdate pour modifications atomiques multi-champs       |
|    - Validation eager (donnees malformees) + globale (coherence) |
+------------------------------------------------------------------+
```

## Configuration Statique (Settings)

### Sections Principales

| Section | Struct | Description |
|---|---|---|
| `[rocks]` | `Rocks` | Chemin RocksDB, prefix reseau, limites RAM (tip_limit, max_dag_blocks, max_utxos, max_spent_outpoints) |
| `[network]` | `Network` | Mode (dev/testnet/mainnet), network_id, protocol_version, symbole natif |
| `[address]` | `Address` | HRP Bech32m (ex: `"8e"`) |
| `[admin]` | `Admin` | Wallet addresses (obsolete), signer_pubkeys, treasury_wallets_file |
| `[client]` | `Client` | Adresses bind P2P et API, TLS insecure, `api_tls_enabled`, internal API |
| `[tls]` | `TlsConfig` | Chemins cert/key/CA PEM, whitelist empreintes SHA-256 |
| `[limits]` | `Limits` | max_body_bytes, request_timeout_ms, rate_limit_rps, burst |
| `[auth]` | `Auth` | Signature obligatoire, admin_api_token, IPs autorisees, fichier API keys |
| `[secrets]` | `SecretSettings` | Chemin cle identite noeud, fichier wallet admin |
| `[validation]` | `ValidationSettings` | PoW bits, limites tx, coordinator public keys, single_writer mode |
| `[fees]` | `FeesSettings` | Ratios, epsilon, mode selection, paliers, rewards, distribution, economics |
| `[p2p]` | `P2pConfig` | Peers connus, bind addr, IPs autorisees, strict whitelist |
| `[[ledgers]]` | `Vec<LedgerDef>` | Definitions multi-ledger avec overrides fees/validation par ledger |

### Chargement et Validation

La fonction `load_config()` suit un ordre de priorite :

1. Variable `$PMS_CONFIG` -> fichier specifique (required)
2. Fallbacks TOML dans l'ordre : `config/config.dev.toml`, `etc/config/config.dev.toml`, `./config.dev.toml`, `/etc/pms/config.dev.toml`
3. Variables d'environnement prefixees `PMS__` (double underscore pour sous-cles)

Exemple : `PMS__NETWORK__MODE=mainnet` -> `settings.network.mode = Mainnet`

La validation (`Settings::validate()`) verifie :
- Coherence prefix/mode (`pms:dev` pour Dev, `pms:test` pour Testnet, `pms:main` pour Mainnet)
- TLS obligatoire en mainnet, insecure interdit en prod
- Existence des fichiers secrets en prod
- PoW bits <= 32

### Separation TLS API / P2P (v0.4.3)

Le champ `api_tls_enabled` (defaut: `true`) dans `[client]` permet de desactiver TLS sur l'API HTTP sans affecter le P2P. Cas d'usage : deploiements ou l'Engine est derriere un reverse-proxy sur un reseau interne. Par defaut (`true`), l'API utilise le TLS de la section `[tls]` — le testnet simule la config prod avec HTTPS de bout en bout.

```toml
[client]
# api_tls_enabled = true   # defaut: API HTTPS (memes certs que P2P)
# api_tls_enabled = false   # optionnel: API HTTP sans TLS (P2P reste en TLS)
```

Voir [[deployment-operations]] et [[gateway]] pour l'impact sur l'architecture Docker.

### Multi-Ledger

Si aucun `[[ledgers]]` n'est defini, `effective_ledgers()` genere automatiquement un ledger `"main"` a partir de `[rocks]` et `[network]` (retrocompatibilite). Chaque `LedgerDef` peut avoir des overrides de fees (`LedgerFeesOverride`) et de validation (`LedgerValidationOverride`).

## Configuration Runtime (Hot-Swap)

### RuntimeConfig

La structure `RuntimeConfig` contient tous les parametres modifiables a chaud :

| Parametre | Type | Defaut | Description |
|---|---|---|---|
| `fee_rate_bps` | `u32` | `300` (3%) | Taux de commission sur les transactions |
| `base_fee` | `String` | `"0.0000001"` | Frais fixes par transaction |
| `coordinator_fee_bps` | `u32` | `6700` (67%) | Part du Coordinator dans les fees |
| `treasury_fee_bps` | `u32` | `3300` (33%) | Part du Treasury dans les fees |
| `min_pow_bits` | `u8` | `8` | Difficulte PoW minimale |
| `max_mint_per_block` | `u64` | `1_000_000` | Maximum tokens mintables par bloc |
| `mint_enabled` | `bool` | `true` | Kill switch minting |
| `fee_tiers` | `Vec<FeeTier>` | `[]` | Bareme progressif (remplace fee_rate_bps si non vide) |
| `fee_distribution` | `Option<FeeDistributionConfig>` | `None` | Distribution N-way (remplace coordinator/treasury split) |
| `mint_fee_base` | `Option<String>` | `None` | Fee fixe sur minting tokens custom |
| `mint_fee_ratio` | `Option<String>` | `None` | Ratio sur montant minte |
| `token_creation_fee` | `Option<String>` | `None` | Fee one-time creation de token |
| `nft_mint_fee` | `Option<String>` | `None` | Fee mint NFT |
| `nft_fee_exempt_types` | `Vec<String>` | `[]` | Types NFT exempts de fee |
| `burn_rate_bps` | `u32` | `0` | Pourcentage de fees brulees (3000 = 30%) |
| `contract_deployment_fee` | `Option<String>` | `None` | Fee deploiement contrat |
| `storage_fee_per_kb` | `Option<String>` | `None` | Fee stockage par KB de payload |
| `dynamic_fee_enabled` | `bool` | `false` | Multiplicateur de fee basee sur la congestion |
| `target_tps` | `u32` | `100` | TPS cible pour le calcul de fee dynamique |
| `max_fee_multiplier` | `f64` | `5.0` | Multiplicateur maximum sous congestion |

### ConfigUpdate (18 variantes)

L'enum `ConfigUpdate` definit toutes les modifications possibles :

| Variante | Parametres | Description |
|---|---|---|
| `SetFeeRate` | `{ bps: u32 }` | Modifier le taux de commission |
| `SetBaseFee` | `{ fee: String }` | Modifier les frais fixes |
| `SetCoordinatorFee` | `{ bps: u32 }` | Part du Coordinator |
| `SetTreasuryFee` | `{ bps: u32 }` | Part du Treasury |
| `SetMinPow` | `{ bits: u8 }` | Difficulte PoW |
| `SetMaxMint` | `{ amount: u64 }` | Max tokens par mint |
| `SetMintEnabled` | `{ enabled: bool }` | Kill switch minting |
| `SetFeeTiers` | `{ tiers: Vec<FeeTier> }` | Bareme progressif |
| `ClearFeeTiers` | _(aucun)_ | Retour au fee_rate_bps lineaire |
| `SetFeeDistribution` | `{ beneficiaries: Vec<FeeBeneficiary> }` | Distribution N-way |
| `SetMintFee` | `{ base, ratio }` | Fees de minting |
| `SetTokenCreationFee` | `{ fee }` | Fee creation token |
| `SetNftMintFee` | `{ fee }` | Fee mint NFT |
| `SetNftFeeExemptTypes` | `{ types: Vec<String> }` | Types NFT exempts |
| `SetBurnRate` | `{ bps: u32 }` | Taux de burn des fees |
| `SetContractDeploymentFee` | `{ fee }` | Fee deploiement contrat |
| `SetStorageFeePerKb` | `{ fee }` | Fee stockage par KB |
| `SetDynamicFee` | `{ enabled, target_tps, max_multiplier }` | Fees dynamiques |
| `BatchUpdate` | `Vec<ConfigUpdate>` | Modifications atomiques multiples |

### Mecanisme d'Application

```
Coordinator signe un bloc avec PlainPayload::ConfigUpdate(update)
    |
    v
Chaque noeud recoit le bloc et appelle:
    RuntimeConfig::apply_update(&self, update, block_id, timestamp)
        |
        v
    1. apply_update_inner() : applique les mutations
       - Validation eager des donnees malformees
       - BatchUpdate : recursion sur chaque sous-update
        |
        v
    2. validate_fee_config() : validation globale
       - fee_distribution.validate() OU validate_fee_split()
       - validate_fee_tiers() si non vide
        |
        v
    3. Si OK : nouvelle RuntimeConfig persistee dans RocksDB
       + ConfigHistoryEntry ajoute a l'historique
```

### Validation des Fee Tiers

Un bareme de fee tiers est valide si :
- Au moins un tier
- `up_to` strictement croissants (Decimal)
- Seul le dernier tier peut avoir `up_to: None` (catch-all)
- Tous les non-derniers doivent avoir `up_to: Some`
- Tous les `ratio` sont des Decimal >= 0

### Fee Distribution N-Way

La `FeeDistributionConfig` permet de distribuer les fees a N beneficiaires :

```rust
pub struct FeeBeneficiary {
    pub role: String,         // "coordinator", "treasury", "client", "partner"
    pub percent_bps: u16,     // 0-10000 (basis points)
    pub address: Option<String>, // None = resolu au runtime
}
```

Validation : la somme des `percent_bps` doit etre exactement `10000` (100%).

## Crates et Fichiers

| Crate | Fichier | Role |
|---|---|---|
| `pms-config` | `crates/pms-config/src/config.rs` | `Settings`, `load_config()`, `load_config_with()`, validation |
| `pms-config` | `crates/pms-config/src/runtime.rs` | `RuntimeConfig`, `ConfigUpdate`, `FeeTier`, `ConfigHistoryEntry`, `validate_fee_tiers()` |
| `pms-config` | `crates/pms-config/src/settings.rs` | Types statiques : `Rocks`, `Network`, `Client`, `TlsConfig`, `FeesSettings`, `LedgerDef`, `FeeBeneficiary`, `FeeDistributionConfig`, `ValidationSettings`, `Auth`, etc. |
| `pms-config` | `crates/pms-config/src/treasury_wallets.rs` | `TreasuryWallets`, `load_treasury_wallets()`, `sign_treasury_wallets()` |
| `pms-config` | `crates/pms-config/src/lib.rs` | Re-exports publics |
| `pms-config` | `crates/pms-config/tests/config_tests.rs` | Tests unitaires : NetworkMode, deserialization TOML, valeurs par defaut |
| `pms-storage` | `crates/pms-storage/src/rocks_store/store.rs` | Persistance RuntimeConfig dans RocksDB |

## Fonctions Cles

| Fonction | Fichier | Description |
|---|---|---|
| `load_config()` | `config.rs` | Charge depuis `$PMS_CONFIG` ou fallbacks TOML + env `PMS__*`. Valide. |
| `load_config_with(path)` | `config.rs` | Charge avec chemin optionnel `--config`. Defaults integres. |
| `Settings::validate()` | `config.rs` | Coherence prefix/mode, TLS obligatoire prod, secrets existants, PoW bounds. |
| `Settings::effective_ledgers()` | `config.rs` | Retourne les ledgers definis ou genere un ledger "main" par defaut. |
| `RuntimeConfig::new()` | `runtime.rs` | Valeurs par defaut (3% fee, 67/33 split, 8 PoW bits, minting actif). |
| `RuntimeConfig::apply_update(update, block_id, ts)` | `runtime.rs` | Applique une mise a jour. Validation eager + globale. Retourne `Result<Self, String>`. |
| `RuntimeConfig::apply_update_inner(update, block_id, ts)` | `runtime.rs` | Application sans validation globale (interne, pour BatchUpdate recursif). |
| `RuntimeConfig::validate_fee_split()` | `runtime.rs` | Verifie `coordinator_fee_bps + treasury_fee_bps == 10000`. |
| `RuntimeConfig::validate_fee_config()` | `runtime.rs` | Validation globale : distribution OU split + tiers. |
| `validate_fee_tiers(tiers)` | `runtime.rs` | Validation d'un bareme : ordre croissant, catch-all final, ratios >= 0. |
| `FeeDistributionConfig::validate()` | `settings.rs` | Somme des `percent_bps` doit etre 10000. |
| `FeeDistributionConfig::new(coord_bps, treasury_bps)` | `settings.rs` | Constructeur raccourci 2-way. |
| `ConfigUpdate::description()` | `runtime.rs` | Description humaine lisible de la mise a jour (pour logs/audit). |
| `load_treasury_wallets(file, coordinator_pk)` | `treasury_wallets.rs` | Charge et verifie la signature du fichier treasury wallets. |
| `sign_treasury_wallets(wallets, priv_key)` | `treasury_wallets.rs` | Signe une liste de wallets treasury (outil CLI). |

## Interactions

- **[[utxo-system]]** : `Settings.rocks.max_utxos` et `max_spent_outpoints` bornent le cache UTXO et le set de double-spend en RAM.
- **[[wallet-encryption]]** : `Settings.address.hrp` definit le prefix Bech32m. `Settings.validation.coordinator_public_key` et `coordinator_x25519_public_key` identifient le Coordinator. `Settings.admin.treasury_wallets_file` pointe vers la liste signee de wallets treasury.
- Le `RuntimeConfig` est lu par le pipeline de validation et de distribution des fees dans `pms-server`. Chaque modification via `ConfigUpdate` affecte le calcul des fees, la distribution, le minting, et les parametres economiques en temps reel.
- Le `BatchUpdate` permet des modifications atomiques (ex: changer `coordinator_fee_bps` et `treasury_fee_bps` ensemble sans etat intermediaire invalide).
- La configuration multi-ledger (`[[ledgers]]`) permet a chaque ledger d'avoir ses propres overrides de fees et de validation, heritant des valeurs globales pour les champs non specifies.

## Decisions Techniques

1. **Deux couches (statique + runtime)** : Les parametres structurels (chemins, TLS, reseau) ne changent pas en production. Les parametres economiques doivent pouvoir etre ajustes sans downtime.
2. **Validation a deux phases** : `apply_update_inner()` valide les donnees malformees (tiers invalides, distribution invalide) de maniere eager. `validate_fee_config()` valide la coherence globale une seule fois apres toutes les mutations d'un BatchUpdate.
3. **ConfigHistoryEntry** : Chaque changement est enregistre avec le block_id, timestamp, l'update appliquee, et la config resultante. Cela permet un audit complet de l'historique de gouvernance.
4. **Basis points (bps)** : Les pourcentages sont exprimes en basis points (10000 = 100%) pour eviter les erreurs de virgule flottante sur les montants financiers.
5. **`$PMS_CONFIG` prioritaire** : En production, le fichier de config est toujours specifie explicitement. Les fallbacks ne servent qu'au developpement local.
6. **Variables d'environnement `PMS__`** : Le double underscore comme separateur de section permet de surcharger n'importe quel parametre sans modifier le fichier TOML (utile pour les secrets en production, ex: `PMS__AUTH__ADMIN_API_TOKEN`).
