---
tags: [feature, infrastructure]
created: 2026-01-08
updated: 2026-03-16
version: v0.5.3
---

# Event System (EventBus)

## Resume

L'Event System est le bus d'evenements asynchrone central du moteur PMS. Il repose sur un canal `tokio::broadcast` et fournit un mecanisme de publication/souscription (pub/sub) in-process permettant a differents modules de reagir en temps reel aux evenements on-chain : validation NFT, persistance de blocs, distribution de recompenses de noeuds, execution de contrats, etc.

Le bus est thread-safe (clonable via `Arc` interne du `broadcast::Sender`), non-bloquant a l'emission, et supporte plusieurs subscribers en parallele. Il utilise une backpressure automatique : les subscribers trop lents recoivent une erreur `Lagged` et perdent les evenements les plus anciens du buffer. La capacite par defaut en production est de **4096 evenements** en buffer.

Les consommateurs principaux sont :
- Le systeme de streaming SSE (`GET /v1/wallet/{address}/activity/stream`) de l'[[activity-system|Activity System]], qui filtre les evenements `BlockPersisted` par adresse en memoire sans acces disque pour offrir des notifications temps reel aux clients.
- Le `ContractListener` (`pms-contracts`) qui ecoute les evenements `NftBurnProcessed` et evalue les [[smart-contracts|contrats declaratifs]] pour accumuler les refunds de burn.

## Dates

| | Date |
|---|---|
| Creee | 2026-01-08 |
| Derniere mise a jour | 2026-03-15 |
| Version d'introduction | v0.1.0 |

### Historique des commits

| Date | Commit | Description |
|------|--------|-------------|
| 2026-01-08 | `63411f3` | Creation initiale du crate `pms-event` avec `EventBus`, `PmsEvent` (Nft, ContractFulfilled, ContractFailed, MilestoneConfirmed, BlockAdded, NodeRewardDistributed) |
| 2026-01-23 | `e44113b` | Patch majeur : ajustements et stabilisation |
| 2026-02-27 | `f21a510` | Ajout du variant `BlockPersisted` avec adresses pre-calculees, integration SSE `stream_wallet_activity`, emission dans `persist_block()` |

## Crates et Fichiers

| Crate | Fichier | Role |
|-------|---------|------|
| `pms-event` | `crates/pms-event/src/lib.rs` | Re-export public : `EventBus`, `PmsEvent` |
| `pms-event` | `crates/pms-event/src/bus.rs` | Implementation du bus via `tokio::broadcast::Sender<PmsEvent>` |
| `pms-event` | `crates/pms-event/src/events.rs` | Definition de l'enum `PmsEvent` et de ses methodes utilitaires |
| `pms-event` | `crates/pms-event/Cargo.toml` | Dependances : `tokio` (sync), `serde`, `tracing`, `pms-types-nft` |
| `pms-event` | `crates/pms-event/tests/event_tests.rs` | Tests unitaires et d'integration (emit/receive, multi-subscribers, serialisation, clone) |
| `pms-core` | `crates/pms-core/src/core_adapter.rs` | Creation de l'`EventBus` (capacite 4096) dans `CoreAdapter::new()` |
| `pms-core` | `crates/pms-core/src/net_adapter.rs` | Emission des evenements `Nft`, `NodeRewardDistributed`, `BlockPersisted` dans `persist_block()` |
| `pms-interface` | `crates/pms-interface/src/net_adapter.rs` | Trait `NetDagAdapter` : methode `event_bus() -> Option<EventBus>` (default `None` pour les mocks) |
| `pms-contracts` | `crates/pms-contracts/src/listener.rs` | Consommateur : `ContractListener` ecoute `NftBurnProcessed`, evalue les contrats, pousse les refunds |
| `pms-server` | `crates/pms-server/src/api_fn/nft.rs` | Producteur : `emit_nft_burn_processed()` emet `NftBurnProcessed` depuis les 3 handlers burn |
| `pms-server` | `crates/pms-server/src/api.rs` | `FeePoolRefundSink` impl + spawn du `ContractListener` au demarrage |
| `pms-server` | `crates/pms-server/src/api_fn/activity.rs` | Consommateur SSE : `stream_wallet_activity()` s'abonne au bus et filtre `BlockPersisted` |
| `pms-wallet` | `crates/pms-wallet/src/history.rs` | `collect_involved_addresses()` : extraction des adresses impliquees depuis un `PlainPayload`, utilisee par le producteur `BlockPersisted` |

## Types d'Evenements (`PmsEvent`)

L'enum `PmsEvent` est defini dans `crates/pms-event/src/events.rs`. Chaque variant represente un type d'evenement distinct. Les variants sont organises en 4 categories.

### NFT Events

| Variant | `event_type()` | Champs | Description | Statut |
|---------|----------------|--------|-------------|--------|
| `Nft { block_id, action: NftAction::Mint }` | `"nft_minted"` | `block_id`, `action` (token_id, creator, metadata) | Un NFT a ete cree (mint) | **Actif** -- emis dans `persist_block()` |
| `Nft { block_id, action: NftAction::Transfer }` | `"nft_transferred"` | `block_id`, `action` (token_id, from, to, new_owner_x25519_pubkey) | Un NFT a ete transfere a un nouveau proprietaire | **Actif** |
| `Nft { block_id, action: NftAction::Use }` | `"nft_used"` | `block_id`, `action` (token_id, user, action_type, action_data) | Un NFT a ete utilise (ex: clicker_confirm) | **Actif** |
| `Nft { block_id, action: NftAction::Burn }` | `"nft_burned"` | `block_id`, `action` (token_id, burner) | Un NFT a ete detruit | **Actif** |
| `Nft { block_id, action: NftAction::BatchBurn }` | `"nft_batch_burned"` | `block_id`, `action` (token_ids, burner) | Plusieurs NFTs detruits en un seul bloc | **Actif** |

Le variant `Nft` encapsule un `NftAction` (defini dans `pms-types-nft`) et utilise le helper `PmsEvent::nft(block_id, action)` pour la construction.

### NFT Burn Processed (v0.5.2)

| Variant | `event_type()` | Champs | Description | Statut |
|---------|----------------|--------|-------------|--------|
| `NftBurnProcessed` | `"nft_burn_processed"` | `block_id`, `ledger_id`, `burner_address`, `token_ids: Vec<String>`, `metadata: Option<NftMetadata>` | Burn NFT traite avec succes, enrichi avec metadata pre-fetchees. Emis par les handlers burn AVANT `apply_action()`. | **Actif** -- emis dans `nft.rs`, consomme par `ContractListener` |

### Smart Contract Events

| Variant | `event_type()` | Champs | Description | Statut |
|---------|----------------|--------|-------------|--------|
| `ContractFulfilled` | `"contract_fulfilled"` | `block_id`, `contract_id`, `result` | Un smart contract a ete execute avec succes | **Actif** -- emis par `ContractListener` apres evaluation reussie |
| `ContractFailed` | `"contract_failed"` | `block_id`, `contract_id`, `error` | Un smart contract a echoue | **Reserve** -- defini mais pas encore emis |

Le variant `ContractFulfilled` est emis par le `ContractListener` (`pms-contracts`) pour chaque contrat evalue avec succes suite a un `NftBurnProcessed`.

### System Events

| Variant | `event_type()` | Champs | Description | Statut |
|---------|----------------|--------|-------------|--------|
| `MilestoneConfirmed` | `"milestone_confirmed"` | `block_id`, `approved_blocks: Vec<String>` | Un bloc Milestone a ete confirme, avec la liste des blocs approuves | **Reserve** -- defini mais pas encore emis |
| `BlockAdded` | `"block_added"` | `block_id` | Un nouveau bloc a ete ajoute au DAG (informatif, haut debit) | **Reserve** -- defini mais pas encore emis |
| `NodeRewardDistributed` | `"node_reward_distributed"` | `node_pk`, `address`, `amount_sats: u64`, `milestone_id` | Une recompense a ete distribuee a un noeud validateur | **Actif** -- emis dans `persist_block()` |

### Activity Stream Events

| Variant | `event_type()` | Champs | Description | Statut |
|---------|----------------|--------|-------------|--------|
| `BlockPersisted` | `"block_persisted"` | `block_id`, `ts_ms: i64`, `payload_type: String`, `involved_addresses: Vec<String>`, `payload_json: String` | Bloc persiste dans le DAG avec adresses pre-calculees et payload JSON serialise | **Actif** -- emis dans `persist_block()` |

Le variant `BlockPersisted` est le plus riche : il contient le payload JSON complet et la liste des adresses impliquees, pre-calculees au moment de l'emission via `pms_wallet::history::collect_involved_addresses()`. Ce pre-calcul permet au consommateur SSE de filtrer en memoire sans acces a la base de donnees.

### Methodes utilitaires de `PmsEvent`

| Methode | Signature | Description |
|---------|-----------|-------------|
| `event_type()` | `fn event_type(&self) -> &'static str` | Retourne le type d'evenement sous forme de chaine statique (ex: `"nft_minted"`, `"block_persisted"`) |
| `block_id()` | `fn block_id(&self) -> &str` | Retourne le `block_id` associe a l'evenement (pour `NodeRewardDistributed`, retourne `milestone_id`) |
| `nft()` | `fn nft(block_id: String, action: NftAction) -> Self` | Helper constructeur pour creer un evenement NFT |
| `nft_burn_processed()` | `fn nft_burn_processed(block_id, ledger_id, burner_address, token_ids, metadata) -> Self` | Helper constructeur pour creer un evenement `NftBurnProcessed` enrichi avec metadata (v0.5.2) |

## Producteurs et Consommateurs

### Producteurs (Emission)

Les evenements sont emis depuis deux sources :

| Evenement | Localisation | Contexte d'emission |
|-----------|-------------|---------------------|
| `PmsEvent::Nft` | `pms-core/net_adapter.rs:267-268` | Apres application reussie d'une `NftAction` (Mint, Transfer, Burn, Use, BatchBurn) via `store.apply_action()` ou `store.set_owner()` |
| `PmsEvent::NftBurnProcessed` | `pms-server/api_fn/nft.rs` | Apres burn reussi, avec metadata pre-fetchees. Emis par `emit_nft_burn_processed()` dans les 3 handlers burn. |
| `PmsEvent::ContractFulfilled` | `pms-contracts/listener.rs` | Apres evaluation reussie d'un contrat par le `ContractListener`. |
| `PmsEvent::NodeRewardDistributed` | `pms-core/net_adapter.rs:1001-1006` | Apres creation des UTXOs de recompense pour chaque noeud validateur, declenchee par un bloc Milestone |
| `PmsEvent::BlockPersisted` | `pms-core/net_adapter.rs:1068-1074` | A la fin de `persist_block()`, pour tout bloc persiste dans le DAG. Inclut le type de payload, les adresses impliquees et le JSON serialise |

Le bus est cree dans `CoreAdapter::new()` (`core_adapter.rs:60`) avec une capacite de **4096 evenements** et stocke comme champ `pub event_bus: EventBus` de la struct `CoreAdapter`.

### Consommateurs (Souscription)

| Consommateur | Localisation | Evenement consomme | Description |
|-------------|-------------|-------------------|-------------|
| ContractListener | `crates/pms-contracts/src/listener.rs` | `NftBurnProcessed` | Evalue les contrats declaratifs matchants, accumule les refunds via `RefundSink`, emet `ContractFulfilled`. Spawne au demarrage du serveur dans `api.rs`. |
| SSE Activity Stream | `crates/pms-server/src/api_fn/activity.rs:430-575` | `BlockPersisted` | Endpoint `GET /v1/wallet/{address}/activity/stream`. Souscrit au bus, filtre par adresse via `involved_addresses`, classifie le payload en `ActivityItem`, et emet des evenements SSE `"activity"` ou `"warning"` (lagged). Supporte le dechiffrement en temps reel via `x25519_sk_hex` et le filtrage par type via `?type=`. |

Le bus est expose via le trait `NetDagAdapter::event_bus() -> Option<EventBus>` (`crates/pms-interface/src/net_adapter.rs:61`). L'implementation par defaut retourne `None` (pour les mocks de test). L'implementation reelle dans `CoreAdapter` (`net_adapter.rs:1232-1234`) retourne `Some(self.event_bus.clone())`.

## Fonctions Cles

### `EventBus::new(capacity: usize) -> Self`
**Fichier** : `crates/pms-event/src/bus.rs:47-50`
Cree un nouveau bus d'evenements avec un buffer de `capacity` evenements. Utilise `tokio::broadcast::channel`. Capacite recommandee : 1024 (usage normal), 4096 (haut debit / production).

### `EventBus::emit(&self, event: PmsEvent)`
**Fichier** : `crates/pms-event/src/bus.rs:64-76`
Emet un evenement sur le bus. Non-bloquant. Si aucun subscriber n'ecoute, l'evenement est ignore silencieusement (log debug). Complexite O(n) ou n = nombre de subscribers (chaque subscriber recoit un `Clone` de l'evenement). Log structure via `tracing::debug!` avec `event_type` et `block_id`.

### `EventBus::subscribe(&self) -> broadcast::Receiver<PmsEvent>`
**Fichier** : `crates/pms-event/src/bus.rs:102-104`
Cree un nouveau receiver pour ecouter les evenements. Le receiver peut etre utilise avec `.recv().await` dans une boucle async. Les subscribers lents recoivent `RecvError::Lagged(n)` avec le nombre d'evenements perdus.

### `EventBus::subscriber_count(&self) -> usize`
**Fichier** : `crates/pms-event/src/bus.rs:109-111`
Retourne le nombre actuel de subscribers connectes. Utile pour le monitoring et les health checks.

### `EventBus::default() -> Self`
**Fichier** : `crates/pms-event/src/bus.rs:114-118`
Cree un bus avec la capacite par defaut de 1024.

### `PmsEvent::nft(block_id: String, action: NftAction) -> Self`
**Fichier** : `crates/pms-event/src/events.rs:125-127`
Helper constructeur pour creer un evenement `Nft` a partir d'un `block_id` et d'une `NftAction`.

### `PmsEvent::event_type(&self) -> &'static str`
**Fichier** : `crates/pms-event/src/events.rs:93-108`
Retourne le type de l'evenement sous forme de chaine statique. Les evenements NFT sont discrimines par le type de `NftAction` contenu (`nft_minted`, `nft_transferred`, `nft_used`, `nft_burned`, `nft_batch_burned`).

### `PmsEvent::block_id(&self) -> &str`
**Fichier** : `crates/pms-event/src/events.rs:112-122`
Retourne le `block_id` associe. Note : pour `NodeRewardDistributed`, retourne le champ `milestone_id` (l'ID du Milestone qui a declenche la distribution).

### `collect_involved_addresses(plain: &PlainPayload) -> Vec<String>`
**Fichier** : `crates/pms-wallet/src/history.rs:94`
Extrait toutes les adresses impliquees dans un `PlainPayload`. Appelee par le producteur `BlockPersisted` dans `persist_block()` pour pre-calculer les adresses sans acces DB cote consommateur SSE. Gere tous les types de payload : `Mint`, `TxUtxo`, `Reward`, `Nft` (Mint/Transfer/Use/Burn/BatchBurn), `TokenCreate`, `BridgeLock`, `BridgeMint`, `Freeze`, `Unfreeze`, `Seize`, `Reverse`, `ContractRegister`, `ContractUpdate`. Les payloads chiffres (`EncryptedReward`) ne retournent aucune adresse.

### `stream_wallet_activity()`
**Fichier** : `crates/pms-server/src/api_fn/activity.rs:430-575`
Handler Axum SSE. Souscrit au bus via `event_bus().subscribe()`, filtre les `BlockPersisted` par `involved_addresses`, parse le `payload_json`, classifie en `ActivityItem` via `classify_activity_sync()`, et emet des evenements SSE. Gere la backpressure Lagged en emettant un evenement `"warning"`.

## Architecture

```
 persist_block() [pms-core]       burn handlers [pms-server/nft.rs]
          |                                |
          |  .emit(Nft)                    |  .emit(NftBurnProcessed)
          |  .emit(NodeRewardDistributed)  |
          |  .emit(BlockPersisted)         |
          v                                v
  +--------------------------------------------+
  |              EventBus                      |  tokio::broadcast (cap=4096)
  |            (pms-event/bus.rs)               |
  +--------+---------------------+-------------+
           |                     |
           |  .subscribe()       |  .subscribe()
           v                     v
  +---------------------------+  +------------------------------+
  |  stream_wallet_activity() |  |  ContractListener            |
  |  (activity.rs)            |  |  (pms-contracts/listener.rs) |
  |  GET /v1/wallet/stream    |  +------------------------------+
  +---------------------------+            |
           |                               |  evaluate_nft_burn()
           |                               |  RefundSink::add_burn_refund()
           v                               v
     SSE -> Client                  FeePool -> fee_distribution
```

### Flux de donnees detaille pour `BlockPersisted`

1. `persist_block()` recoit un `WireBlock` valide
2. Le payload est extrait et classifie via `plain_payload_type_str()`
3. Les adresses sont pre-calculees via `collect_involved_addresses()`
4. Un `PmsEvent::BlockPersisted` est emis avec `block_id`, `ts_ms`, `payload_type`, `involved_addresses`, et `payload_json`
5. Le consommateur SSE recoit l'evenement via `rx.recv().await`
6. Filtrage rapide en memoire : `involved_addresses.iter().any(|a| a == addr)`
7. Si match, le `payload_json` est deserialisé en `PayloadEnvelope` puis classifie en `ActivityItem`
8. Les payloads chiffres (`Encrypted`, `EncryptedReward`) sont dechiffres en temps reel si `x25519_sk_hex` est fourni
9. L'`ActivityItem` est serialise en JSON et emis comme evenement SSE `"activity"`

## Interactions

- [[activity-system]] : Consommateur principal. Le SSE Activity Stream utilise l'EventBus pour recevoir les blocs persistes en temps reel et les diffuser aux clients connectes.
- [[nft-system]] : Les actions NFT (Mint, Transfer, Use, Burn, BatchBurn) sont emises comme evenements `PmsEvent::Nft` apres application dans le store NFT.
- [[fee-distribution]] : Les recompenses de noeuds generees par les Milestones sont emises comme `PmsEvent::NodeRewardDistributed`.
- [[smart-contracts]] : Le `ContractListener` (dans `pms-contracts`) ecoute les `NftBurnProcessed` et evalue les contrats declaratifs. Emet `ContractFulfilled` apres chaque evaluation reussie.
- [[server-engine]] : Le `CoreAdapter` cree et possede l'`EventBus`. L'interface `NetDagAdapter` expose `event_bus()` pour que le serveur API puisse s'abonner.
- [[storage-rocksdb]] : L'emission de `BlockPersisted` se produit apres la validation RAM/DAG mais avant la persistance asynchrone sur disque (fire-and-forget via `PersistJob`). Les evenements refletent donc l'etat RAM confirme, pas necessairement l'etat disque.

## Caracteristiques Techniques

| Propriete | Valeur |
|-----------|--------|
| Backend | `tokio::sync::broadcast` |
| Capacite (prod) | 4096 evenements |
| Capacite (defaut) | 1024 evenements |
| Thread-safety | Oui (Clone via Arc interne) |
| Emission bloquante | Non |
| Backpressure | `RecvError::Lagged(n)` -- subscribers lents perdent les anciens evenements |
| Serialisation | `serde::Serialize + Deserialize` (JSON-ready) |
| Trait Clone | Requis sur `PmsEvent` (chaque subscriber recoit un clone) |
| Logging | `tracing::debug!` a chaque emission |
| Dependances | `tokio`, `serde`, `tracing`, `pms-types-nft` |

## Tests

Les tests se trouvent dans `crates/pms-event/tests/event_tests.rs` :

| Test | Description |
|------|-------------|
| `test_nft_event_types` | Verifie que le helper `PmsEvent::nft()` et `event_type()` retournent les bonnes valeurs pour un NFT Mint |
| `test_event_serialization` | Verifie la serialisation/deserialisation JSON d'un evenement `NftAction::Use` |
| `test_bus_emit_and_receive` | Test async : emission d'un `BlockAdded` et reception via subscriber |
| `test_bus_multiple_subscribers` | Test async : un evenement est recu par deux subscribers independants |
| `test_bus_clone_shares_channel` | Verifie que deux clones de l'`EventBus` partagent le meme canal (meme `subscriber_count`) |
