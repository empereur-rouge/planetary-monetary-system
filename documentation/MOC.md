---
tags: [index]
updated: 2026-03-19
---

# PMS Engine — Map of Content

> Manuel d'instruction du projet DAG-PMS (Planetary Monetary System).
> Ce vault Obsidian sert de documentation fonctionnelle haut-niveau.
> Pour la documentation technique du code Rust, utiliser `cargo doc --no-deps --open`.

---

## Infrastructure

| Fiche | Description |
|-------|-------------|
| [[server-engine]] | Serveur Axum principal, AppState, middleware, background tasks |
| [[gateway]] | Proxy public (rate limiting, SSE streaming, catch-all fallback) |
| [[storage-rocksdb]] | Persistance RocksDB, column families, migrations, tuning |
| [[config-system]] | Configuration TOML, RuntimeConfig, hot-swap |
| [[p2p-network]] | Réseau P2P, TLS mutual auth, gossip, whitelist |
| [[event-system]] | Bus d'événements interne (tokio::broadcast), PmsEvent enum |
| [[metrics-monitoring]] | Métriques Prometheus, /metrics endpoints, observabilité |
| [[deployment-operations]] | Docker, scripts de déploiement, architecture TLS, topologie 4 processus |

## Données

| Fiche | Description |
|-------|-------------|
| [[utxo-system]] | UTXO shardé, LRU cache, balance tracking, supply cache |
| [[wallet-encryption]] | Wallets (BIP39, Ed25519, X25519), encryption (ChaCha20-Poly1305) |
| [[token-system]] | Tokens custom multi-asset, Amount (8 décimales), FeePolicy |
| [[block-payloads]] | 17 types de payload (PlainPayload), chiffrement X25519+AES-256-GCM |

## Protocole & Consensus

| Fiche | Description |
|-------|-------------|
| [[validation-consensus]] | Pipeline de validation, Single Writer, PoW, ValidatePolicy |
| [[milestones-finality]] | Finalité K-depth, milestones, checkpoints RocksDB |

## Fonctionnalités

| Fiche | Description |
|-------|-------------|
| [[compliance]] | Gel d'adresses, saisie de fonds, inversion de transactions |
| [[smart-contracts]] | Contrats déclaratifs (triggers: NFT burn, token burn) |
| [[economics]] | Fee burn, gas pool, dynamic fees, storage fees, cross-ledger fee |
| [[bridge]] | Transferts cross-ledger (lock/mint) |
| [[activity-system]] | Index par adresse/type, pré-calcul, cache LRU, SSE streaming |
| [[fee-distribution]] | Distribution automatique des frais, encrypted reward blocks |
| [[nft-system]] | Mint, burn, transfer, burn-to-refund, encrypted metadata |
| [[multi-ledger]] | Ledgers isolés, routage `/l/{id}/`, RocksDB multi-prefix |
| [[wallet-factory]] | Gestion custodiale des wallets, send-simple |
| [[api-key-authentication]] | Clés API SHA-256, scopes granulaires, CRUD admin |
| [[dag-pruning]] | Pruning insertion-order RAM + trim_tips RocksDB |
| [[simulator]] | Simulateur 252 agents, game engine Edenite, stress test |
| [[node-rewards]] | Distribution des récompenses aux nœuds, fee pool, block counts |
| [[streaming-api]] | Endpoints SSE, broadcast channels, streaming temps réel |
| [[service-monitoring]] | Health checker background, status bar dashboard, polling services |

## API

Voir le dossier [[api/README|documentation/api/]] pour la référence complète des endpoints REST.

| Doc | Description |
|-----|-------------|
| [API Overview](api/README.md) | Vue d'ensemble, authentification, rate limiting |
| [Admin](api/admin.md) | Endpoints admin protégés |
| [Wallet](api/wallet.md) | Gestion des wallets |
| [Transactions](api/transactions.md) | Envoi de tokens |
| [NFT](api/nft.md) | Opérations NFT |
| [Activity](api/activity.md) | Flux d'activité |
| [Tokens](api/tokens.md) | Registre des tokens |
| [Supply](api/supply.md) | Offre en circulation |
| [Bridge](api/bridge.md) | Pont cross-ledger |
| [Compliance](api/compliance.md) | Conformité réglementaire |
| [Ledgers](api/ledgers.md) | Multi-ledger |
| [Health](api/health.md) | Health checks |
| [DAG](api/dag.md) | Opérations DAG |
| [Nodes](api/nodes.md) | Registre des nœuds |
| [History](api/history.md) | Historique |

## Versions

| Système | Fichier | Valeur actuelle |
|---------|---------|-----------------|
| Software | `bin/Cargo.toml` | `0.5.17` |
| Schema DB | `crates/pms-storage/src/migrations.rs` → `CURRENT_VER` | `9` |
| DAG Protocol | `crates/pms-storage/src/migrations.rs` → `DAG_VERSION` | `1.2.0` |
| P2P Protocol | `crates/pms-config/src/config.rs` → `protocol_version` | `1` |
| API | `crates/pms-server/src/api_fn/version.rs` → `API_VERSION` | `7` |

Voir [[../../CHANGELOG|CHANGELOG.md]] pour l'historique complet des versions.

---

## Navigation

- **Par tag** : `#feature`, `#infrastructure`, `#reference`, `#api`
- **Par crate** : chercher le nom du crate dans les tableaux "Crates et Fichiers"
- **Par endpoint** : chercher le path dans les tableaux "Endpoints API"
- **Par fonction** : chercher le nom dans les tableaux "Fonctions Clés"
