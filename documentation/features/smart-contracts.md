---
tags: [feature]
created: 2026-03-13
updated: 2026-03-16
version: v0.5.3
---

# Smart Contracts (Contrats Déclaratifs)

## Résumé

Le système de Smart Contracts de PMS est un moteur de règles déclaratives, exécuté nativement par le nœud coordinateur. Contrairement aux smart contracts Turing-complets (Solidity/EVM), les contrats PMS sont des règles pré-définies qui réagissent à des événements spécifiques (burn de NFT, burn de tokens) et déclenchent des actions automatiques (refunds, émissions d'événements). Ce design garantit la prévisibilité, la sécurité, et la performance, sans risque d'exécution arbitraire de code. Les contrats sont stockés dans le RocksDB du **main ledger** et évalués en temps réel par le `ContractEngine` dans le crate dédié `pms-contracts`, déclenché via l'[[event-system|EventBus]] lors de chaque burn NFT, y compris sur les custom ledgers.

### Architecture : EventBus + `pms-contracts` (v0.5.2)

Depuis v0.5.2, l'évaluation des contrats est **découplée** des handlers NFT burn via l'[[event-system|EventBus]] :

1. Les handlers burn (`burn_nft`, `burn_nft_simple`, `burn_nft_batch_simple`) émettent un événement `NftBurnProcessed` sur l'EventBus après un burn réussi.
2. Le `ContractListener` (dans `pms-contracts`) écoute ces événements et évalue les contrats matchants.
3. Les refunds sont accumulés via le trait `RefundSink`, implémenté côté `pms-server` par `FeePoolRefundSink` qui wrappe le `FeePoolRegistry`.

Ce design :
- **Isole** la logique contrat dans un crate dédié (`pms-contracts`), sans dépendance sur `pms-server` ou `AppState`.
- **Résout** le problème cross-ledger (v0.5.1) : le listener reçoit `Arc<dyn ContractStorage>` pointant vers le main RocksDB au démarrage.
- **Découple** temporellement : les handlers burn n'attendent plus l'évaluation des contrats.

**Fix v0.5.3 — EventBus routing** : Les burns sur les custom ledgers (eden, etc.) émettaient sur le bus **per-ledger** (via `state.srv.adapter_arc().event_bus()`), mais le `ContractListener` est abonné au bus **main** uniquement. Fix : `AppState.contract_event_bus` pointe toujours vers le bus du main adapter, et `emit_nft_burn_processed()` utilise ce bus partagé quel que soit le ledger.

## Dates

| | Date |
|---|---|
| Créée | 2026-03-13 |
| Dernière mise à jour | 2026-03-15 |
| Version d'introduction | v0.2.1 |

## Configuration

### Frais de déploiement de contrat

Le déploiement d'un contrat peut être soumis à des frais, configurables via :

- **Fichier de configuration statique** (`pms-config`) : champ `contract_deployment_fee` dans la section `Fees` (ex: `"10.0"` pour 10 PMS).
- **RuntimeConfig** (hot-swap) : via `ConfigUpdate::SetContractDeploymentFee { fee: Option<String> }`, applicable sans redémarrage du nœud.
- **Per-ledger override** : champ `contract_deployment_fee` dans `LedgerOverrides`.

**Priorité de résolution** : `RuntimeConfig` > `EffectiveFees` (per-ledger) > valeur par défaut (None = pas de frais).

Si configuré, les frais sont débités automatiquement lors de l'appel à `POST /admin/contracts` et enregistrés dans un bloc Reward dans le DAG.

### Exemple de configuration TOML

```toml
[fees]
contract_deployment_fee = "50.0"  # 50 PMS par déploiement de contrat
```

### Hot-swap via API

```json
{
  "SetContractDeploymentFee": {
    "fee": "25.0"
  }
}
```

Pour désactiver les frais de déploiement : `{ "SetContractDeploymentFee": { "fee": null } }`.

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-contracts` | `crates/pms-contracts/src/engine.rs` | Moteur d'évaluation des contrats (`evaluate_nft_burn`, `evaluate_formula`, `find_attribute`, `ContractResult`) |
| `pms-contracts` | `crates/pms-contracts/src/listener.rs` | Subscriber EventBus : écoute `NftBurnProcessed`, évalue les contrats, pousse les refunds via `RefundSink` |
| `pms-contracts` | `crates/pms-contracts/src/lib.rs` | Re-exports : `evaluate_nft_burn`, `ContractResult`, `RefundSink`, `spawn_contract_listener` |
| `pms-types-contract` | `crates/pms-types-contract/src/lib.rs` | Types de données : `Contract`, `ContractScope`, `ContractTrigger`, `ContractAction`, `MintFormula` |
| `pms-types-payload` | `crates/pms-types-payload/src/payload.rs` | Variantes `PlainPayload::ContractRegister` et `PlainPayload::ContractUpdate` |
| `pms-event` | `crates/pms-event/src/events.rs` | Variant `PmsEvent::NftBurnProcessed` + helper `nft_burn_processed()` |
| `pms-server` | `crates/pms-server/src/api_fn/contracts.rs` | Endpoints API admin CRUD pour les contrats |
| `pms-server` | `crates/pms-server/src/api_fn/nft.rs` | Émission `NftBurnProcessed` via `emit_nft_burn_processed()` dans les 3 handlers burn |
| `pms-server` | `crates/pms-server/src/api.rs` | `FeePoolRefundSink` impl + spawn du `ContractListener` au démarrage |
| `pms-server` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Chargement des frais de déploiement (`load_contract_deployment_fee`) |
| `pms-server` | `crates/pms-server/src/fee_pool.rs` | Accumulation des refunds de burn dans le `FeePool` (`add_burn_refund`) |
| `pms-storage` | `crates/pms-storage/src/contract_store.rs` | Trait `ContractStorage` + implémentation in-memory pour les tests |
| `pms-storage` | `crates/pms-storage/src/rocks_store/contract_storage.rs` | Implémentation RocksDB de `ContractStorage` |
| `pms-core` | `crates/pms-core/src/validations/check.rs` | Validation des blocs `ContractRegister` et `ContractUpdate` (signature coordinateur, champs obligatoires) |
| `pms-config` | `crates/pms-config/src/config.rs` | Champ `contract_deployment_fee` dans `Fees` et `LedgerOverrides` |
| `pms-config` | `crates/pms-config/src/runtime.rs` | `RuntimeConfig.contract_deployment_fee` + `ConfigUpdate::SetContractDeploymentFee` |

### Fichiers de tests

| Crate | Fichier | Couverture |
|-------|---------|------------|
| `pms-core` | `crates/pms-core/tests/contract_validation.rs` | Validation DAG : signature coordinateur, champs vides, scopes, triggers |
| `pms-storage` | `crates/pms-storage/tests/contract_store_test.rs` | CRUD RocksDB, filtrage par type/scope, toggle enable/disable, wildcard |
| `pms-contracts` | `crates/pms-contracts/src/engine.rs` (tests inline) | Évaluation des formules, batch burn, scope ledger, contrats désactivés, wildcard |
| `pms-types-contract` | `crates/pms-types-contract/src/lib.rs` (tests inline) | Sérialisation/désérialisation, scope matching |

## Fonctions Clés

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `evaluate_nft_burn()` | `crates/pms-contracts/src/engine.rs` | Point d'entrée principal : recherche les contrats matching et évalue les formules pour un burn NFT donné |
| `evaluate_formula()` | `crates/pms-contracts/src/engine.rs` | Évalue une `MintFormula` (`FixedRate`, `AttributeFormula`, `FixedAmount`) et retourne un `Decimal` |
| `find_attribute()` | `crates/pms-contracts/src/engine.rs` | Extrait un attribut du JSON `extra` des metadata NFT, avec support du dot-notation (ex: `"attributes.weight"`) |
| `spawn_contract_listener()` | `crates/pms-contracts/src/listener.rs` | Spawne la tâche Tokio qui écoute les `NftBurnProcessed` et évalue les contrats |
| `RefundSink::add_burn_refund()` | `crates/pms-contracts/src/listener.rs` | Trait pour l'accumulation des refunds (implémenté par `FeePoolRefundSink` dans pms-server) |
| `emit_nft_burn_processed()` | `crates/pms-server/src/api_fn/nft.rs` | Émet un événement `NftBurnProcessed` sur l'EventBus après un burn réussi |
| `decrypt_nft_metadata_from_dag()` | `crates/pms-server/src/api_fn/nft.rs` | Récupère les metadata NFT AVANT le burn (nécessaire car `apply_action` supprime le block_id) |
| `register_contract()` | `crates/pms-server/src/api_fn/contracts.rs` | Handler API pour l'enregistrement d'un nouveau contrat (génère l'ID SHA-256, charge les frais de déploiement) |
| `list_contracts()` | `crates/pms-server/src/api_fn/contracts.rs` | Handler API pour lister tous les contrats enregistrés |
| `get_contract()` | `crates/pms-server/src/api_fn/contracts.rs` | Handler API pour récupérer les détails d'un contrat par ID |
| `toggle_contract()` | `crates/pms-server/src/api_fn/contracts.rs` | Handler API pour activer/désactiver un contrat existant |
| `find_nft_burn_contracts()` | `crates/pms-storage/src/contract_store.rs` | Recherche les contrats actifs matching un trigger `OnNftBurn` + scope ledger |
| `set_enabled()` | `crates/pms-storage/src/contract_store.rs` | Active ou désactive un contrat dans le store |
| `load_contract_deployment_fee()` | `crates/pms-server/src/api_fn/tx_helpers.rs` | Charge le montant des frais de déploiement depuis `RuntimeConfig` ou `EffectiveFees` |
| `require_coordinator_signature()` | `crates/pms-core/src/validations/check.rs` | Valide que le bloc est signé par le coordinateur (utilisée pour `ContractRegister` et `ContractUpdate`) |
| `ContractScope::matches()` | `crates/pms-types-contract/src/lib.rs` | Vérifie si un contrat s'applique à un ledger donné (`Global` = tous, `Ledger(ids)` = liste spécifique) |

## Endpoints API

Tous les endpoints de gestion des contrats sont sous le préfixe `/admin/` et accessibles uniquement au coordinateur.

| Méthode | Path | Description |
|---------|------|-------------|
| `POST` | `/admin/contracts` | Enregistre un nouveau contrat. Génère un `contract_id` (SHA-256 de name+trigger+actions). Charge les frais de déploiement si configurés. Retourne `201 Created`. |
| `GET` | `/admin/contracts` | Liste tous les contrats enregistrés (actifs et inactifs). Retourne `{ "contracts": [...] }`. |
| `GET` | `/admin/contracts/{contract_id}` | Récupère les détails d'un contrat par son ID. Retourne `404` si inexistant. |
| `POST` | `/admin/contracts/{contract_id}/toggle` | Active ou désactive un contrat. Body : `{ "enabled": bool, "reason": "..." }`. |

### Exemple : enregistrer un contrat

**Requête :**

```json
POST /admin/contracts
{
  "name": "cube-burn-to-pms",
  "scope": "Global",
  "trigger": {
    "OnNftBurn": { "nft_type": "cube" }
  },
  "actions": [
    {
      "AccumulateRefund": {
        "asset_id": null,
        "formula": {
          "FixedRate": {
            "rate_numerator": 1,
            "rate_denominator": 10
          }
        }
      }
    }
  ]
}
```

**Réponse :**

```json
{
  "contract_id": "a1b2c3d4e5f6...",
  "name": "cube-burn-to-pms",
  "enabled": true,
  "fee_charged": "50.0",
  "fee_block_id": "block_abc123..."
}
```

### Exemple : désactiver un contrat

```json
POST /admin/contracts/{contract_id}/toggle
{
  "enabled": false,
  "reason": "Maintenance programmée"
}
```

## RocksDB Column Families

| Column Family | Clé | Valeur | Description |
|---------------|-----|--------|-------------|
| `contracts` | `contract_id` (bytes) | `Contract` (JSON sérialisé) | Stockage de tous les contrats déclaratifs. Scan complet pour la recherche (nombre de contrats supposé faible). |

Le CF `contracts` est déclaré dans deux listes dans `crates/pms-storage/src/rocks_store/store.rs` :
- `CF_NAMES` (ligne 163) : utilisé par `open_db_multi_prefix()`
- Tableau hardcodé dans `new()` (ligne 290) : utilisé pour la création initiale

**Important** : les deux listes doivent toujours être synchronisées.

## Types et Structures

### `Contract`

Structure principale représentant un contrat déclaratif enregistré.

```rust
pub struct Contract {
    pub contract_id: String,    // ID unique (hash SHA-256 du contenu)
    pub name: String,           // Nom lisible (ex: "cube-burn-to-pms")
    pub scope: ContractScope,   // Portée d'application
    pub trigger: ContractTrigger, // Événement déclencheur
    pub actions: Vec<ContractAction>, // Actions à exécuter
    pub enabled: bool,          // Actif ou non
    pub version: u32,           // Version (incrémentée lors de mises à jour)
}
```

### `ContractScope`

Portée d'application d'un contrat dans un engine [[multi-ledger]].

```rust
pub enum ContractScope {
    Global,             // S'applique à tous les ledgers
    Ledger(Vec<String>), // S'applique uniquement aux ledgers listés
}
```

- `Global` : le contrat s'applique à tous les ledgers du nœud.
- `Ledger(vec)` : le contrat s'applique uniquement aux ledgers dont l'ID est présent dans la liste.
- `ContractScope::matches(ledger_id)` retourne `true` si le contrat s'applique au ledger donné.

### `ContractTrigger`

Événement qui déclenche l'évaluation d'un contrat.

```rust
pub enum ContractTrigger {
    OnNftBurn { nft_type: Option<String> },  // Burn de NFT (filtre optionnel par type)
    OnTokenBurn { asset_id: String },        // Burn de token fungible (UTXO)
}
```

- `OnNftBurn { nft_type: None }` : wildcard, matche tous les burns NFT quel que soit le type.
- `OnNftBurn { nft_type: Some("cube") }` : matche uniquement les burns de NFTs dont `metadata.nft_type == "cube"`.
- `OnTokenBurn` : prévu pour les burns de tokens fungibles (pas encore intégré dans les handlers).

### `ContractAction`

Action exécutée quand un contrat est déclenché.

```rust
pub enum ContractAction {
    AccumulateRefund {
        asset_id: Option<String>,  // Asset à créditer (None = PMS natif)
        formula: MintFormula,      // Formule de calcul du montant
    },
    EmitEvent {
        event_type: String,        // Type d'événement (pour traitement off-chain)
    },
}
```

- `AccumulateRefund` : accumule un montant dans le `FeePool`, distribué au prochain cycle de [[fee-distribution]] (Milestone/Reward).
- `EmitEvent` : émet un log/événement (pour webhooks, notifications off-chain).

### `MintFormula`

Formule pour calculer le montant d'un refund.

```rust
pub enum MintFormula {
    FixedRate {
        rate_numerator: u64,     // Numérateur du taux
        rate_denominator: u64,   // Dénominateur du taux
    },
    AttributeFormula {
        attribute_names: Vec<String>, // Noms des attributs NFT à multiplier
        divisor: u64,                 // Diviseur final
    },
    FixedAmount {
        amount: String,          // Montant fixe par item (ex: "5.5")
    },
}
```

**Calculs :**

| Formule | Calcul | Exemple |
|---------|--------|---------|
| `FixedRate` | `count * numerator / denominator` | 10 CUBE à taux 1/10 = 1.0 PMS |
| `AttributeFormula` | `(attr[0] * attr[1] * ... * attr[n]) * count / divisor` | weight=1000, size=50, density=80, divisor=1M = 4.0 |
| `FixedAmount` | `amount * count` | 5.5 PMS * 3 items = 16.5 PMS |

- Tous les calculs utilisent `rust_decimal::Decimal` avec arrondi à 8 décimales (`round_dp(8)`).
- Division par zéro rejetée avec une erreur (`rate_denominator == 0` ou `divisor == 0`).
- `AttributeFormula` supporte le dot-notation dans les noms d'attributs (ex: `"attributes.weight"` navigue dans `extra["attributes"]["weight"]`).

### `ContractResult`

Résultat de l'évaluation d'un contrat, retourné par `evaluate_nft_burn()`.

```rust
pub struct ContractResult {
    pub contract_id: String,      // ID du contrat déclenché
    pub contract_name: String,    // Nom du contrat
    pub refund_address: String,   // Adresse du wallet recevant le refund
    pub refund_amount: Decimal,   // Montant du refund
    pub asset_id: Option<String>, // Asset du refund (None = PMS natif)
    pub details: String,          // Détails d'exécution (audit/logging)
}
```

### Payload DAG

Les contrats sont également propagés via le DAG sous forme de blocs signés par le coordinateur :

```rust
// Dans PlainPayload (pms-types-payload)
ContractRegister(Contract),          // Enregistrement d'un nouveau contrat
ContractUpdate {                     // Activation/désactivation
    contract_id: String,
    enabled: bool,
    reason: String,
},
```

Ces blocs sont validés par `validate_block()` dans `pms-core` avec les règles suivantes :
- **Signature coordinateur obligatoire** : seul le nœud coordinateur peut enregistrer ou modifier des contrats.
- **Champs obligatoires** : `contract_id` non vide, `name` non vide (pas de whitespace-only), au moins une action pour `ContractRegister`.

## Interactions

### Burn NFT → EventBus → ContractListener (3 handlers)

Les contrats sont évalués automatiquement après chaque burn NFT réussi, via l'[[event-system|EventBus]]. Les 3 handlers burn du [[nft-system]] émettent un événement `NftBurnProcessed` :

1. **`burn_nft()`** : burn signé par le client via `WireBlock` (single ou batch).
2. **`burn_nft_simple()`** : burn simplifié (single NFT, le serveur construit le bloc).
3. **`burn_nft_batch_simple()`** : burn batch simplifié (multiple NFTs).

**Flux d'exécution :**
1. Le handler reçoit la requête de burn.
2. Les metadata du NFT sont récupérées AVANT le burn via `decrypt_nft_metadata_from_dag()` (car `apply_action()` supprime le `block_id` du NFT).
3. Le burn est exécuté (`apply_action()` + persistence du bloc dans le DAG).
4. `emit_nft_burn_processed()` émet un événement `NftBurnProcessed` sur l'EventBus avec les metadata pre-fetched.
5. Le `ContractListener` (dans `pms-contracts`) reçoit l'événement de manière asynchrone.
6. `evaluate_nft_burn()` recherche les contrats matching (type NFT + scope ledger).
7. Les formules sont évaluées et les refunds sont poussés via `RefundSink::add_burn_refund()`.
8. `FeePoolRefundSink` route le refund vers le bon `FeePool` via `FeePoolRegistry`.

### FeePool et [[fee-distribution|Fee Distribution]]

Les refunds de contrats sont accumulés dans le `FeePool` via `add_burn_refund(wallet_address, amount, asset_id)`. Ils sont distincts des fees de nœuds et sont distribués directement aux wallets des utilisateurs lors du prochain cycle de fee distribution (bloc Milestone ou Reward).

Le `FeePool` maintient un `HashMap<(String, Option<String>), Decimal>` pour les `burn_refunds` (clé = (adresse, asset_id)), accessible via `get_burn_refunds()` et `total_burn_refunds()`.

### Validation DAG (pms-core)

Les blocs `ContractRegister` et `ContractUpdate` sont validés au niveau du DAG par `validate_block()` dans `crates/pms-core/src/validations/check.rs`. La validation impose :
- Signature du coordinateur (`require_coordinator_signature()`).
- `contract_id` non vide.
- `name` non vide et non whitespace-only (pour `ContractRegister`).
- Au moins une action (pour `ContractRegister`).

### Configuration hot-swap (RuntimeConfig)

Le champ `contract_deployment_fee` peut être modifié à chaud via `ConfigUpdate::SetContractDeploymentFee { fee: Option<String> }` dans le système `RuntimeConfig` de `pms-config`. Cela permet d'ajuster les frais de déploiement sans redémarrage du nœud.

### [[economics|Gas Pool]] (ledgers custom)

Pour les ledgers custom (non-main), les handlers de burn vérifient les conditions du gas pool (`try_consume_gas()`) et de la subscription (`check_subscription_active()`) avant d'exécuter le burn et les contrats associés.
