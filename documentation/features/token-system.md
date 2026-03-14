---
tags: [feature]
created: 2025-12-28
updated: 2026-03-14
version: v0.3.0
---

# Token System (Multi-Asset)

## Resume

Le Token System permet la creation, l'enregistrement et la gestion de tokens custom (multi-asset) sur le DAG PMS, en complement du token natif PMS. Chaque token est identifie par un `asset_id` unique, possede ses propres metadonnees (symbole, decimales, supply max), et s'integre dans le modele UTXO existant via le champ optionnel `TxOutput.asset_id`.

Le systeme repose sur :
- Un **type `Amount`** avec precision garantie a 8 decimales et arrondi automatique apres chaque operation arithmetique.
- Un **registre de tokens** persiste dans RocksDB (column family `token_registry`).
- Une **politique de frais flexible** (`FeePolicy`) supportant le mode lineaire et le mode par paliers marginaux.
- Un **modele UTXO multi-asset** avec validation per-asset (conservation stricte par asset_id).
- Des **endpoints admin** pour la creation et le minting de tokens, avec verification de la `max_supply`.
- La **separation des frais** : les frais de transaction sont toujours payes en PMS natif, meme pour les transferts de tokens custom.

## Configuration

### Fichier de configuration (`config.toml` / `FeesSettings`)

| Parametre | Type | Default | Description |
|-----------|------|---------|-------------|
| `mint_fee_base` | `Option<String>` | `None` | Frais fixe en PMS sur chaque mint de token custom |
| `mint_fee_ratio` | `Option<String>` | `None` | Ratio proportionnel au montant minte (ex: `"0.02"` = 2%) |
| `token_creation_fee` | `Option<String>` | `None` | Fee one-time en PMS pour creer un nouveau token |
| `nft_mint_fee` | `Option<String>` | `None` | Fee sur le mint de NFT |

### Configuration dynamique (`RuntimeConfig` / hot-swap)

Les frais de token peuvent etre modifies a chaud via des blocs `ConfigUpdate` :

| Variante | Champs | Description |
|----------|--------|-------------|
| `ConfigUpdate::SetMintFee` | `base: Option<String>`, `ratio: Option<String>` | Modifie le fee de minting (fixe + ratio) |
| `ConfigUpdate::SetTokenCreationFee` | `fee: Option<String>` | Modifie le fee de creation de token |

### Configuration par ledger (`LedgerFeesOverride`)

Chaque ledger peut surcharger les frais globaux via `EffectiveFees`. La resolution suit la priorite : `RuntimeConfig` > `LedgerFeesOverride` > `FeesSettings` globale.

## Crates et Fichiers

| Crate | Fichier | Role |
|-------|---------|------|
| `pms-token` | `crates/pms-token/src/amount.rs` | Type `Amount` avec precision 8 decimales et arrondi automatique |
| `pms-token` | `crates/pms-token/src/fee.rs` | `FeePolicy` : calcul de frais lineaire et par paliers marginaux |
| `pms-token` | `crates/pms-token/src/token.rs` | Constante `PLANETARY_MONETARY_SYSTEM` (symbole PMS, 8 decimales) |
| `pms-types-transaction` | `crates/pms-types-transaction/src/transaction.rs` | `TxOutput` avec champ optionnel `asset_id` |
| `pms-types-payload` | `crates/pms-types-payload/src/payload.rs` | `TokenMetadata`, variantes `PlainPayload::TokenCreate` et `PlainPayload::Mint` |
| `pms-server` | `crates/pms-server/src/api_fn/token.rs` | Endpoints admin : creation et minting de tokens |
| `pms-server` | `crates/pms-server/src/api_fn/supply.rs` | Endpoint supply avec support multi-asset (`?asset_id=`) |
| `pms-server` | `crates/pms-server/src/api_fn/transaction.rs` | `prepare_tx` et `wallet_send_tx` avec coin selection multi-asset |
| `pms-server` | `crates/pms-server/src/api_fn/tx_helpers.rs` | `select_utxos` (coin selection par asset), `load_mint_fee_policy`, `load_token_creation_fee` |
| `pms-server` | `crates/pms-server/src/api.rs` | Routage des endpoints token (public + admin) |
| `pms-storage` | `crates/pms-storage/src/rocks_store/token_registry.rs` | Persistence RocksDB du registre de tokens (CF `token_registry`) |
| `pms-storage` | `crates/pms-storage/src/rocks_store/store.rs` | Declaration du CF `token_registry` dans `CF_NAMES` |
| `pms-core` | `crates/pms-core/src/utxo.rs` | `ShardedUtxoSet` : `circulating_supply_by_asset`, `balance_by_address_and_asset` |
| `pms-core` | `crates/pms-core/src/validations/transactions.rs` | `validate_transaction_async` : validation per-asset (conservation stricte) |
| `pms-interface` | `crates/pms-interface/src/net_adapter.rs` | Trait `NetDagAdapter` : `add_utxo(asset_id)`, `circulating_supply_by_asset` |
| `pms-config` | `crates/pms-config/src/config.rs` | `FeesSettings` : `mint_fee_base`, `mint_fee_ratio`, `token_creation_fee` |
| `pms-config` | `crates/pms-config/src/runtime.rs` | `RuntimeConfig` + `ConfigUpdate::SetMintFee`, `SetTokenCreationFee` |

### Fichiers de tests

| Fichier | Couverture |
|---------|------------|
| `crates/pms-core/tests/multi_token_test.rs` | Validation multi-asset, conservation per-asset, supply/balance par asset, serialisation |
| `crates/pms-storage/tests/token_registry_test.rs` | Registre RocksDB : register, get, list, duplicates, serialisation |
| `crates/pms-token/src/amount.rs` (module `tests`) | Precision Amount, arrondi, operations arithmetiques |
| `crates/pms-token/src/fee.rs` (module `tests`) | FeePolicy lineaire, paliers, boundary, zero amount |

## Fonctions Cles

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `Amount::parse_pms(s)` | `crates/pms-token/src/amount.rs` | Parse une chaine en `Amount` avec validation (non-negatif, max 8 decimales) |
| `Amount::from_decimal(d)` | `crates/pms-token/src/amount.rs` | Cree un `Amount` a partir d'un `Decimal` avec arrondi automatique a 8 decimales |
| `FeePolicy::compute_fee(amount)` | `crates/pms-token/src/fee.rs` | Calcule les frais (lineaire ou paliers) avec precision `Amount` |
| `FeePolicy::tiered(base, tiers)` | `crates/pms-token/src/fee.rs` | Cree une politique de frais par paliers marginaux |
| `admin_create_token(state, headers, req)` | `crates/pms-server/src/api_fn/token.rs` | Cree un token : valide, enregistre dans RocksDB, emet un bloc `TokenCreate` on-chain |
| `admin_mint_token(state, headers, req)` | `crates/pms-server/src/api_fn/token.rs` | Mint des tokens : verifie `max_supply`, cree un bloc `Mint` avec `asset_id`, met a jour les UTXOs |
| `list_tokens(state)` | `crates/pms-server/src/api_fn/token.rs` | Liste tous les tokens enregistres dans le registre |
| `get_token(state, asset_id)` | `crates/pms-server/src/api_fn/token.rs` | Retourne les metadonnees d'un token specifique |
| `get_circulating_supply(state, query)` | `crates/pms-server/src/api_fn/supply.rs` | Supply circulant avec fallback auto sur le premier token custom si PMS = 0 |
| `prepare_tx(state, req)` | `crates/pms-server/src/api_fn/transaction.rs` | Prepare une TX multi-asset : coin selection separee (token + PMS pour fees) |
| `select_utxos(adapter, address, target, asset_id)` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Coin selection largest-first filtree par `asset_id` |
| `validate_transaction_async(utxos, tx)` | `crates/pms-core/src/validations/transactions.rs` | Validation per-asset : conservation `sum(inputs[asset]) == sum(outputs[asset])` |
| `RocksStore::register_token(metadata)` | `crates/pms-storage/src/rocks_store/token_registry.rs` | Persiste un token dans le CF `token_registry` avec validation des metadonnees |
| `RocksStore::get_token(asset_id)` | `crates/pms-storage/src/rocks_store/token_registry.rs` | Lecture d'un token par `asset_id` depuis RocksDB |
| `RocksStore::list_tokens()` | `crates/pms-storage/src/rocks_store/token_registry.rs` | Iteration sur tous les tokens du registre |
| `load_mint_fee_policy(store, eff)` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Charge la politique de mint fee (priorite : RuntimeConfig > EffectiveFees) |
| `load_token_creation_fee(store, eff)` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Charge le fee de creation de token |
| `resolve_effective_fees(global, override)` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Fusionne les fees globaux avec les overrides par ledger |
| `ShardedUtxoSet::circulating_supply_by_asset(asset_id)` | `crates/pms-core/src/utxo.rs` | Supply en circulation d'un token specifique depuis le cache |
| `ShardedUtxoSet::balance_by_address_and_asset(addr, asset_id)` | `crates/pms-core/src/utxo.rs` | Balance d'une adresse pour un asset specifique |
| `RocksStore::validate_token_metadata(metadata)` | `crates/pms-storage/src/rocks_store/token_registry.rs` | Validation exhaustive des metadonnees avant enregistrement |

## Endpoints API

### Endpoints publics

| Methode | Path | Description |
|---------|------|-------------|
| `GET` | `/v1/tokens` | Liste tous les tokens enregistres |
| `GET` | `/v1/tokens/{asset_id}` | Metadonnees d'un token specifique |
| `GET` | `/v1/supply?asset_id=edenite` | Supply circulant par asset (fallback auto sur le premier token custom si PMS natif = 0) |
| `POST` | `/v1/tx/prepare` | Prepare une TX (champ `asset_id` optionnel dans la requete) |
| `POST` | `/wallet/tx/send` | Envoie une TX signee (supporte les outputs multi-asset) |

### Endpoints admin (Coordinator, auth requise)

| Methode | Path | Description |
|---------|------|-------------|
| `POST` | `/admin/tokens/create` | Cree un nouveau token (enregistrement + bloc `TokenCreate` on-chain) |
| `POST` | `/admin/tokens/mint` | Mint des tokens custom vers une adresse (bloc `Mint` avec `asset_id`) |

### Formats de requete/reponse

**POST `/admin/tokens/create`** :
```json
// Request
{
  "asset_id": "edenite",
  "symbol": "EDEN",
  "name": "Edenite Token",
  "decimals": 8,
  "max_supply": "1000000.00000000"  // optionnel
}
// Response (201)
{
  "status": "ok",
  "token": { "asset_id": "edenite", "symbol": "EDEN", ... },
  "block_id": "abc123...",
  "creation_fee": "100"
}
```

**POST `/admin/tokens/mint`** :
```json
// Request
{
  "asset_id": "edenite",
  "to": "8exxxxxxxx...",
  "amount": "500.00000000"
}
// Response (200)
{
  "status": "ok",
  "block_id": "def456...",
  "asset_id": "edenite",
  "amount": "500",
  "to": "8exxxxxxxx...",
  "mint_fee": "11"
}
```

**POST `/v1/tx/prepare`** (transfert multi-asset) :
```json
// Request
{
  "from": "8eAlice...",
  "to": "8eBob...",
  "amount": "60.00000000",
  "asset_id": "edenite"  // optionnel, None = PMS natif
}
// Response (200)
{
  "unsigned_tx": { "inputs": [...], "outputs": [...], "fee": "0.3", "unlocks": [] },
  "tx_hash": "sha256hex...",
  "fee": "0.3",
  "inputs_detail": [...]
}
```

## Types Cles

### `Amount` (`pms-token`)

Type monetaire central avec precision garantie a 8 decimales. Encapsule un `rust_decimal::Decimal` et arrondit automatiquement apres chaque operation arithmetique (`Add`, `Sub`, `Mul`, `Div`).

```rust
pub struct Amount(pub Decimal);

impl Amount {
    pub const DECIMALS: u32 = 8;
    pub fn parse_pms(s: &str) -> Result<Self, AmountError>;
    pub fn from_decimal(d: Decimal) -> Self;  // arrondi auto
    pub fn zero() -> Self;
    pub fn inner(&self) -> Decimal;
}
```

**Pourquoi ce type existe** : avant `Amount`, les calculs de fees pouvaient produire des resultats a 11+ decimales (ex: `0.03549294 * 0.03 = 0.0010647882`), causant des erreurs de validation en cascade. `Amount` garantit que chaque resultat intermediaire respecte la precision du protocole.

### `FeePolicy` (`pms-token`)

Politique de calcul des frais de transaction.

```rust
pub struct FeePolicy {
    pub base_fee: String,       // Frais fixe (ex: "0.001")
    pub ratio: String,          // Ratio lineaire (ex: "0.03" = 3%)
    pub tiers: Vec<FeeTier>,    // Paliers marginaux (prioritaire sur ratio)
}
```

- **Mode lineaire** : `fee = base_fee + (amount * ratio)`
- **Mode paliers marginaux** : `fee = base_fee + somme(tranche_i * ratio_i)`

Exemple paliers :
- Palier 1 : 0..100 PMS a 3%
- Palier 2 : 100..10000 PMS a 1.5%
- Palier 3 : 10000+ PMS a 0.5%
- Pour 200 PMS : `fee = (100 * 0.03) + (100 * 0.015) = 3.0 + 1.5 = 4.5 PMS`

### `TokenMetadata` (`pms-types-payload`)

Metadonnees d'un token enregistre dans le DAG.

```rust
pub struct TokenMetadata {
    pub asset_id: String,           // Identifiant unique (ex: "edenite")
    pub symbol: String,             // Symbole court (ex: "EDEN")
    pub name: String,               // Nom complet (ex: "Edenite Token")
    pub decimals: u8,               // Nombre de decimales (0-18)
    pub max_supply: Option<String>, // Supply max (None = illimite)
    pub creator: String,            // Cle publique du createur
    pub mint_authority: String,     // Cle publique autorisee a mint
}
```

**Contraintes de validation** (appliquees par `validate_token_metadata`) :
- `asset_id` : 1-64 caracteres, alphanumerique + `_` + `-`
- `symbol` : 1-10 caracteres
- `name` : 1-128 caracteres
- `decimals` : 0-18
- `max_supply` : decimal positif si present
- `creator` et `mint_authority` : non-vides

### `TxOutput` (`pms-types-transaction`)

Output d'une transaction avec support multi-asset.

```rust
pub struct TxOutput {
    pub address: String,
    pub amount: String,
    pub asset_id: Option<String>,  // None = PMS natif, Some("edenite") = token custom
}
```

Le champ `asset_id` est decore avec `#[serde(default, skip_serializing_if = "Option::is_none")]` pour la retrocompatibilite : les anciens blocs sans ce champ sont deserialises avec `asset_id = None` (PMS natif).

### `Token` (`pms-token`)

Constante definissant le token natif du protocole.

```rust
pub const PLANETARY_MONETARY_SYSTEM: Token = Token::new("PMS", 8);
```

## Modele UTXO Multi-Asset

### Principe de conservation per-asset

La validation des transactions (`validate_transaction_async`) verifie la conservation stricte par asset :

```
Pour chaque asset_id present dans les inputs ou outputs :
    sum(inputs[asset_id]) == sum(outputs[asset_id])
```

Regles :
1. Un asset ne peut pas etre cree a partir de rien dans une transaction (`AssetBalanceMismatch` si un output reference un `asset_id` absent des inputs).
2. Un asset ne peut pas etre converti en un autre (pas de cross-asset mixing).
3. Les frais sont toujours payes en PMS natif (`asset_id: None`).
4. Les inputs dupliques dans la meme transaction sont rejetes (`DoubleSpend`).

### Coin selection multi-asset

La fonction `select_utxos` filtre les UTXOs par `asset_id` et utilise une strategie **largest-first** :

```
1. Filtrer utxos_by_address par asset_id
2. Trier par montant decroissant
3. Accumuler jusqu'a atteindre le montant cible
```

Pour un transfert de token custom, `prepare_tx` effectue **deux selections separees** :
1. Selection des UTXOs du token (pour le montant a transferer)
2. Selection des UTXOs PMS natif (pour payer les frais)

Cela produit une transaction avec 4 types d'outputs :
- **Output destination** : token transfere (avec `asset_id`)
- **Change token** : excedent de token retourne au sender (avec `asset_id`)
- **Output fees** : frais en PMS vers l'admin/treasury (`asset_id: None`)
- **Change PMS** : excedent PMS retourne au sender (`asset_id: None`)

### Supply tracking

Le `ShardedUtxoSet` maintient un cache de supply par asset (`supply_cache: HashMap<Option<String>, (Decimal, usize)>`). Les requetes de supply sont O(1) via le cache :
- `circulating_supply()` : PMS natif uniquement
- `circulating_supply_by_asset(Some("edenite"))` : supply d'un token specifique
- `circulating_supply_by_asset(None)` : equivalent a `circulating_supply()`

## Stockage

### Column Family `token_registry`

| Cle | Valeur | Description |
|-----|--------|-------------|
| `{asset_id}` (bytes) | `TokenMetadata` (JSON) | Metadonnees du token |

- Declare dans `CF_NAMES` et dans le constructeur `new()` de `RocksStore`.
- Iteration sequentielle pour `list_tokens()`.
- Lookup O(1) par `asset_id` pour `get_token()`.

### UTXOs

Les UTXOs de tokens custom sont stockes de la meme maniere que les UTXOs PMS, avec le champ `asset_id` additionnel dans le `TxOutput` serialise. Le `ShardedUtxoSet` en memoire indexe tous les assets de maniere unifiee.

## Securite et Invariants

1. **Seul le Coordinator peut creer et minter des tokens** : les endpoints admin exigent l'authentification Bearer token et la verification de la cle publique du coordinateur.
2. **Conservation stricte per-asset** : `validate_transaction_async` verifie que chaque `asset_id` est conserve entre inputs et outputs. Impossible de creer des tokens a partir de rien dans une transaction utilisateur.
3. **Protection max_supply** : `admin_mint_token` verifie `current_supply + amount <= max_supply` via le `ShardedUtxoSet` en temps reel avant chaque mint.
4. **Precision garantie** : le type `Amount` empeche les erreurs d'arrondi qui pourraient causer des ecarts de balance. Toute operation arithmetique est automatiquement arrondie a 8 decimales.
5. **Retrocompatibilite** : les anciens blocs sans champ `asset_id` dans `TxOutput` sont traites comme PMS natif (`None`) grace a `#[serde(default)]`.
6. **Validation exhaustive des metadonnees** : `validate_token_metadata` verifie la longueur, le format, et la validite de chaque champ avant l'enregistrement.
7. **Unicite des tokens** : `register_token` verifie l'unicite de l'`asset_id` avant l'insertion dans RocksDB.

## Interactions

- [[fee-distribution]] : les frais de transaction et de minting sont distribues via le systeme de fee distribution existant. Les frais sont toujours en PMS natif.
- [[economics]] : les fees de token creation et de minting sont configurables via `RuntimeConfig` (hot-swap). Le fee burn (`burn_rate_bps`) s'applique aux fees collectes.
- [[nft-system]] : les NFTs utilisent un modele similaire (`PlainPayload::Nft`) et partagent la meme infrastructure de fees.
- [[wallet-factory]] : les wallets creees par le factory peuvent detenir et transferer des tokens custom.
- [[multi-ledger]] : chaque ledger peut avoir ses propres tokens avec des overrides de fees specifiques (`LedgerFeesOverride`).
- [[bridge]] : le bridge (`BridgeLock`/`BridgeMint`) supporte le champ `asset_id` pour les transferts cross-ledger de tokens custom.
- [[utxo-system]] : le `ShardedUtxoSet` gere les UTXOs multi-asset de maniere unifiee avec indexation per-asset pour les requetes de balance et supply.

## Historique

| Date | Evenement |
|------|-----------|
| 2025-12-28 | Creation initiale du crate `pms-token` (`Amount`, `FeePolicy`, constante `PLANETARY_MONETARY_SYSTEM`) |
| 2026-02-12 | Rework majeur : ajout du registre de tokens (`token_registry` RocksDB), endpoints admin (`admin_create_token`, `admin_mint_token`), support `asset_id` dans `TxOutput` et `PlainPayload::TokenCreate` |
| 2026-02-13 | Multi-token enhancements : coin selection multi-asset dans `prepare_tx`, support `?asset_id` dans `/v1/supply`, tests multi-token |
| 2026-02-16 | Renforcement du systeme de fees avec precision BPS et validation stricte |
| 2026-02-27 | Optimisation UTXO : fix deadlock + scaling pour 3M UTXOs (impacte le cache multi-asset) |
| 2026-03-01 | Ajout de `validate_token_metadata` pour validation exhaustive avant enregistrement |
