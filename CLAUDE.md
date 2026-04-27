# Project Rules

## Nature du Projet — Crypto Banking Engine

**CRITICAL: Ce projet est un moteur bancaire crypto dont l'objectif est l'indépendance vis-à-vis des banques traditionnelles. Toute contribution DOIT respecter les exigences suivantes :**

### Performance
- Le code doit être optimisé pour la production à grande échelle. Pas de compromis sur la performance.
- Profiler et benchmarker les chemins critiques (consensus, validation de transactions, propagation réseau).
- Préférer les structures de données et algorithmes les plus efficaces, même si plus complexes à implémenter.
- Éviter les allocations inutiles, les copies superflues, et les locks non nécessaires.

### Sécurité
- **Niveau de sécurité : bancaire.** Chaque ligne de code touchant la cryptographie, les transactions, les balances, ou le consensus doit être traitée comme critique.
- Jamais de raccourcis sur la validation des entrées, la vérification des signatures, ou la gestion des erreurs dans les chemins financiers.
- Toute opération sur les fonds (transferts, frais, mint, burn) doit être auditée, testée exhaustivement, et vérifiée pour les race conditions et les double-spend.
- Les dépendances cryptographiques doivent être des librairies éprouvées et maintenues. Pas de crypto "maison".
- Revue systématique des edge cases de sécurité : overflow/underflow, integer truncation, timing attacks, replay attacks.

### Traçabilité DAG — Mutations d'état
- **CRITICAL: Toute mutation d'état persistante (ownership, configuration, compliance, etc.) DOIT être enregistrée comme un bloc dans le DAG.** C'est la convention fondamentale d'un système blockchain/DAG : le DAG est la source de vérité pour l'audit et la traçabilité.
- Jamais de writes directs en base de données pour des changements d'état — toujours passer par un bloc DAG signé par le coordinateur.
- Les données sensibles dans les blocs DAG DOIVENT être chiffrées (X25519+AES-256-GCM) pour que seuls les destinataires autorisés puissent les déchiffrer.
- Le `PlainPayload` du bloc peut contenir un identifiant public (ex: `ledger_id`) pour le routage, mais les détails sensibles (ex: `new_owner_pubkey`) sont dans un `EncryptedPayload` embarqué.
- L'application de l'état (RocksDB + RAM) se fait APRÈS la persistance du bloc dans le DAG.
- Pattern de référence : `LedgerOwnershipTransfer` (ownership chiffré), `Freeze/Seize` (compliance en clair).

### Rigueur et Exhaustivité
- **Ne JAMAIS chercher à économiser des tokens ou prendre des raccourcis.** Ce projet gère de l'argent réel — la rigueur prime sur la rapidité.
- Jamais de placeholder code, TODO stubs, ou implémentations incomplètes. Toujours écrire le code complet et fonctionnel immédiatement.
- En cas de doute sur la sécurité, la performance, ou la correction d'une implémentation : **utiliser des sub-agents spécialisés** (Task tool) pour vérifier, auditer, ou valider. Ne pas hésiter à lancer une équipe d'agents en parallèle si la tâche le justifie.
- Exemples de cas où les agents doivent être mobilisés :
  - Revue de sécurité d'un changement touchant les transactions ou le consensus.
  - Validation de la cohérence entre les couches RAM et RocksDB.
  - Audit des dépendances pour des vulnérabilités connues.
  - Vérification de la correction d'algorithmes cryptographiques ou financiers.
- Chaque changement doit être complet, testé, et documenté. Pas de "on verra plus tard".
- **OBLIGATOIRE : Avant de conclure une conversation qui a produit des changements de code, lancer `/simplify` pour détecter et corriger les problèmes de réutilisation, qualité et efficacité.**

## VPS Testnet — Infrastructure de Production

### Serveur
- **Hébergeur** : IONOS VPS
- **IP** : `87.106.50.82`
- **User SSH** : `pms` (clé SSH : `~/.ssh/pms_vps`)
- **OS** : Debian 12 (kernel 6.1)
- **Specs** : 8 vCores, 16 Go RAM, 480 Go NVMe SSD
- **Pas d'accès sudo** — swap et opérations root impossibles depuis le user `pms`.

### Stack Docker (docker-compose.testnet.yml)
| Service | Container | Image | Ports | Mem Limit |
|---------|-----------|-------|-------|-----------|
| Engine | `pms-engine-testnet` | `pms-node:testnet` | 8080 (interne) | **14g** |
| Gateway | `pms-gateway-testnet` | `pms-gateway:testnet` | 8443 (interne) | 512m |
| Caddy | `pms-caddy-testnet` | `caddy:2-alpine` | 80, 443 (public) | — |
| Prometheus | `pms-prometheus-testnet` | `prom/prometheus:v2.45.0` | 9091 (localhost) | 512m |
| Simulator | `pms-simulator-testnet` | `pms-simulator:testnet` | 9090 (public) | 512m |

### URLs
- **API Testnet** : `https://testnet.pms-network.com`
- **Dashboard** : `https://testnet.pms-network.com/dashboard/`
- **Simulator** : `http://87.106.50.82:9090`

### Accès & Secrets
- **Backup local** : `/Volumes/Crutial X9 - Macbook Erwan/Misc/pms-key/pms-testnet-*.json` — contient admin token, coordinator keys, SDK API key, treasury keys.
- **NEVER hardcode secrets in code or config files committed to git.** Toujours utiliser `env:VAR` ou les fichiers secrets du VPS (`/opt/pms/etc/pms/`).

### Dimensionnement mémoire RocksDB (config.testnet.toml)
Les valeurs RocksDB DOIVENT être adaptées à la RAM du VPS. Avec 67 CFs (2 ledgers × 33 CFs + default), la formule memtable max est : `num_CFs × max_write_buffer_number × write_buffer_size_mb`.

| VPS RAM | `write_buffer_size_mb` | `max_write_buffer_number` | `block_cache_size_mb` | `db_write_buffer_size_mb` | `max_dag_blocks` | Docker `mem_limit` |
|---------|------------------------|---------------------------|-----------------------|---------------------------|-------------------|--------------------|
| 8 Go | 8 | 2 | 128 | 128 | 5000 | 7g |
| **16 Go** | **16** | **2** | **256** | **256** | **10000** | **14g** |
| 32 Go | 32 | 3 | 1024 | 512 | 50000 | 28g |

**Observation (v0.7.1, 16 Go)** : avec 20M blocs en DB, mémoire suit un pattern dent de scie (compaction RocksDB) : trough ~6-7 GiB, peak ~12.4 GiB. Stable à ~20 tx/s + game activity.

### Déploiement
- **Full deploy** (build + init) : `scripts/deploy-testnet.sh [--yes] <IP> [USER] [SSH_KEY]`
- **Upgrade** (code only, preserve data) : `scripts/upgrade-testnet.sh <IP> [USER] [SSH_KEY]`
- Les images Docker sont **cross-compilées localement** (linux/amd64 via buildx) puis transférées au VPS.
- Le deploy script sauvegarde/restaure automatiquement les clés VPS-specific (coordinator, treasury, signer) dans la config.

### Opérations courantes (sur le VPS)
```bash
# Logs
docker logs -f pms-engine-testnet
docker logs -f pms-simulator-testnet

# Status
cd /opt/pms && PMS_ADMIN_TOKEN=xxx docker compose -f docker-compose.testnet.yml ps

# Restart engine seul
PMS_ADMIN_TOKEN=xxx docker compose -f docker-compose.testnet.yml restart pms-engine

# Diagnostic OOM
docker events --since '1h' --filter container=pms-engine-testnet | grep oom
docker inspect pms-engine-testnet --format='RestartCount: {{.RestartCount}} | OOMKilled: {{.State.OOMKilled}}'
```

### Bug historique : OOM en boucle (v0.5.18, VPS 8 Go)
- **Symptôme** : Engine restart en boucle (16x), exit code 137 (SIGKILL par cgroup OOM).
- **Cause** : `mem_limit: 7g` sur un VPS 8 Go sans swap. Le simulator poussait des données en continu → mémoire montait jusqu'au kill.
- **Fix** : Upgrade VPS à 16 Go + `mem_limit: 14g` + tuning RocksDB pour 16 Go.
- **Diagnostic** : `docker events` montre l'événement `oom` juste avant le `die exitCode:137`. `docker inspect` peut montrer `OOMKilled: false` même si le cgroup a tué le process (c'est un bug connu de Docker).

### Bug historique : Prometheus tué par `upgrade-testnet.sh` (v0.7.6→v0.7.10, 2026-04-27)
- **Symptôme** : `pms-prometheus-testnet` absent de `docker ps -a` après chaque upgrade ; scrape Grafana muet jusqu'à ce qu'on relance Prometheus à la main. Les 4 autres containers tournent normalement.
- **Cause racine (le vrai !)** : deux bugs cumulés dans `scripts/upgrade-testnet.sh` qui supprimaient Prometheus à chaque upgrade :
  1. `docker ps -a --filter "label=com.docker.compose.project=pms" -q | xargs docker rm -f` — supprimait **tous** les containers du projet pour nettoyer les "ghost containers", mais `$SERVICES_TO_RECREATE = "pms-engine pms-gateway pms-simulator"` ne contient pas Prometheus, donc rien ne le rebrulait après.
  2. `docker compose up -d --force-recreate --remove-orphans $SERVICES_TO_RECREATE` — le `--remove-orphans` avec un sous-ensemble des services Compose marque Prometheus comme "orphelin" alors qu'il existe bien dans le YAML.
- **Hypothèse rejetée** : "arrêt manuel + `unless-stopped` inhibé". C'est ce que j'avais documenté la première fois, à tort — le lockfile résiduel + le timing collait. Mais l'observation du run de v0.7.10 (Prometheus disparu **immédiatement après l'upgrade**, sans `docker stop` ni opérateur) a démasqué le vrai script coupable.
- **Diagnostic** : grep dans le upgrade script pour `--remove-orphans` ou `docker rm` à blast-radius large. Le lockfile résiduel est en fait juste la conséquence du `docker rm -f` (SIGKILL).
- **Fix** : `scripts/upgrade-testnet.sh` réécrit pour scoper la cleanup et le `up -d` au seul `$SERVICES_TO_RECREATE` (pas de `--remove-orphans`, pas de `xargs docker rm` global). Dans v0.7.11.
- **Bonus découvert** : le scrape config `pms-gateway` était cassé en 401 Unauthorized — le job n'envoyait pas le Bearer token alors que le gateway protège son `/metrics` avec le même `ADMIN_TOKEN` que l'engine. Fixé dans `etc/prometheus/prometheus.yml` (réutilise `credentials_file: /etc/prometheus/admin_token`).

## Related Projects

### PMS SDK (TypeScript)
- **Chemin** : `/Volumes/Crutial X9 - Macbook Erwan/Documents/Programations/Rust/pms-sdk`
- SDK TypeScript officiel pour interagir avec le réseau PMS (npm : `@empereur-rouge/pms-sdk`).
- Gestion de wallets (BIP39, secp256k1), signature de transactions, communication réseau.
- Quand l'utilisateur parle du "SDK", il s'agit de ce projet.

### Dashboard Client — Heshima Network (React + Rust)
- **Chemin** : `/Volumes/Crutial X9 - Macbook Erwan/Documents/Programations/Web/Heshima Network`
- Frontend React 19 + Vite (`pms-network-client/`) et backend Rust/Axum (`pms-network-server/`).
- Dashboard multi-utilisateur : wallets, transactions, NFTs, admin console.
- Le backend sert de proxy entre le frontend et le PMS Engine (dag-pms).
- Quand l'utilisateur parle du "dashboard" ou du "client", il s'agit de ce projet.

### Règle d'exploration des projets externes
- **OBLIGATOIRE : Toujours utiliser des sub-agents (Task tool) pour explorer ou chercher dans les dossiers du SDK ou du Dashboard.** Ne jamais lire/grep ces dossiers directement depuis le contexte principal — cela évite de polluer la fenêtre de contexte avec du code hors-scope.

## Git & GitHub

- Au début de chaque conversation, proposer de créer une branche Git pour les changements à venir.
- À la fin de la tâche, proposer de commit et merger dans `main`.
- **NEVER commit secrets, API keys, or credentials.**
- Branching : `main` (stable), `feature/<nom>`, `fix/<nom>`. Merger dans `main` quand terminé.
- Commits atomiques avec messages clairs : description brève + contexte (1-2 phrases). Référencer les issues quand applicable.
- Utiliser `gh` CLI pour les opérations GitHub.

## Tests

- Always create new tests or update existing ones to cover the changes made.
- Tests must validate the expected behavior independently of the implementation. Do not write tests that simply mirror the code you wrote — tests should verify correctness from the user's perspective, not confirm that your implementation runs without error.
- **CRITICAL: Show test output before validation.** Every test MUST include `println!`/`eprintln!` statements that display key values (API responses, computed results, state changes). After writing a test, run it with `cargo test <test_name> -- --nocapture` and show the full output to the user. The user validates the test based on the printed output, NOT just on whether it passes. A test that passes but produces wrong output is a bug.
- Never remove debug prints from tests after validation — they serve as living documentation and help catch regressions.

### Dual-Layer Consistency (RAM + RocksDB)
- **CRITICAL: Tout fix appliqué sur une couche (RAM DAG) DOIT être vérifié et appliqué sur l'autre couche (RocksDB) si la même logique existe.**
  - Exemple historique : `prune_oldest()` (RAM) a été corrigé pour protéger le dernier tip (commit `9e2922f`), mais `trim_tips()` et `remove_tip()` (RocksDB) n'ont pas reçu la même protection → bug silencieux en production (frais bloqués pendant des heures).
- Quand un bug est corrigé dans `crates/pms-core/src/concurrent_dag/` (module directory), vérifier systématiquement `crates/pms-storage/src/rocks_store/` (et vice-versa).
- **Tests de boundary/edge-case obligatoires** : toujours tester les scénarios limites (dernier élément, liste vide, overflow) — pas seulement le cas nominal. Les bugs critiques se cachent dans les edge cases que les tests "happy path" ne couvrent pas.

### DAG Sandbox — Tests d'intégration production-like

Le fichier `crates/pms-server/tests/dag_sandbox.rs` fournit un **moteur PMS complet en in-process** (LedgerManager, EventBus, ContractListener, fee distribution) pour les tests d'intégration. C'est le lab de référence pour valider les fonctionnalités end-to-end.

**Commande d'exécution :**
```bash
# Tous les tests sandbox (release, ignored, avec output)
cargo test --release -p pms-server --test dag_sandbox -- --ignored --nocapture

# Un test spécifique
cargo test --release -p pms-server --test dag_sandbox test_edn_transfer_fee_flow -- --ignored --nocapture
```

**Quand utiliser la sandbox :**
- Validation de fee distribution (PMS et EDN) sur main et custom ledgers.
- Tests de smart contracts (burn → refund, transfer fees).
- Vérification des endpoints supply/balance avec filtrage d'assets.
- Tout scénario nécessitant un moteur complet (multi-ledger, gas pool, contracts).

**Benchmark TPS :**
- `crates/pms-server/tests/local_bench.rs` — mesure le TPS brut du moteur DAG (10K+ TPS prouvé).
- Commande : `cargo test --release -p pms-server --test local_bench -- --ignored --nocapture`

**Quand écrire un nouveau test sandbox :**
- Tout bug découvert en production doit d'abord être reproduit dans la sandbox avant d'être corrigé.
- Toute nouvelle fonctionnalité touchant les transactions, fees, contracts, ou ledgers doit avoir un test sandbox.
- Réutiliser les helpers existants de `Sandbox` (`create_ledger`, `faucet_mint`, `send_simple`, `burn_nft_simple`, `distribute_fees`, `get_balance`, `get_asset_balance`, `get_supply`, `send_asset`, `register_contract`).

## Edenite Game Engine (Simulator)

Le simulateur embarque un **game engine** qui gère un ledger custom "eden" avec un token "edenite" (EDN). Les agents mintent des cubes NFT, les burn pour recevoir de l'EDN, et s'échangent l'EDN entre eux.

### Fichiers clés
- Game engine : `tools/simulator/src/game.rs`
- Bootstrap funder : `tools/simulator/src/agent/funder.rs`
- Configs : `tools/simulator/simulator.{dev,docker,testnet}.toml`

### Attributs de cube — Obfuscation SHA256
- Les attributs (`weight`, `size`, `density`) sont **obfusqués** dans le JSON `extra` et le contrat.
- Algorithme : `SHA256("pms-cube-attrs-v1" || attr_name)[..8]` → 16 hex chars.
- Clés obfusquées : `weight` → `e6c84244b96fe92d`, `size` → `7f41d7f9c843a618`, `density` → `c0d4a83995fb0edb`.
- Le contrat `edenite-cube-burn` utilise `CubeAttributes::obfuscated_attr_names()` pour matcher les clés.
- **CRITICAL** : Le contrat sur le testnet DOIT utiliser les mêmes clés obfusquées. Si le contrat est recréé, les anciennes clés en clair causeront un reward de 0.

### Ranges d'attributs
| Attribut | Range | Unité | Décimales |
|----------|-------|-------|-----------|
| weight | 1.0 – 30.0 | kg | 2 |
| size | 0.5 – 5.0 | cm | 2 |
| density | 0.1 – 1.0 | — | 2 |

### Formule de reward
```
EDN = (weight × size × density) / divisor
```
- **Divisor** : `13,700` (configuré dans `[simulation.game].divisor` du TOML).
- Produit moyen par cube : ~23.44 → ~0.00171 EDN/cube.

### Calibration économique (9 cubes/min, 10h/jour)
| Période | Cubes | EDN |
|---------|-------|-----|
| 1 minute | 9 | ~0.0154 |
| 1 heure | 540 | ~0.924 |
| 1 jour (10h) | 5,400 | ~9.24 |
| 1 mois (30j) | 162,000 | ~277 |

### Système de rareté — 6 tiers
Tirage sur 100,000,000. La rareté est **purement cosmétique** — elle n'affecte PAS la formule de reward.

| Tier | Probabilité | Leading zeros dans token_id |
|------|-------------|----------------------------|
| Basic | 99.9% | 0 |
| Common | 0.09% | 1 |
| Uncommon | 0.009% | 2 |
| Rare | 0.00099% | 3 |
| Legendary | 0.000009% | 4 |
| Unique | 0.000001% | 5 |

- Le `token_id` fait 64 hex chars. Le premier char non-zero est garanti `1-f` (pas d'ambiguïté entre tiers).
- Exemple Rare : `000e8c1d5f7a...` (3 leading zeros).

### Contrats smart
Le simulateur enregistre 2 contrats au setup :
1. **`edenite-cube-burn`** : `OnNftBurn{nft_type: "cube"}` → `AccumulateRefund{edenite, AttributeFormula{obfuscated_attrs, divisor}}`.
2. **`eden-transfer-fee`** : `OnTransfer` → 5% fee → coordinator wallet.

Les handlers 409 (already exists) sont idempotents. **Attention** : le `contract_id` est `SHA-256(name + trigger + actions)` — si les actions changent (ex: nouveau divisor), un NOUVEAU contrat est créé au lieu de mettre à jour l'ancien. Il faut alors désactiver l'ancien via `PUT /admin/contracts/{id}`.

## Critical Patterns

Règles impératives tirées de bugs production. Chaque pattern documente un piège récurrent.

### Supply/Balance API — Filtrage par asset

- **Quand un endpoint balance/supply reçoit un `asset_id`** (ex: `?asset_id=edenite`), toujours utiliser `balance_by_address_and_asset(address, asset_id)` et NON `balance_by_address(address)`.
- `balance_by_address()` retourne la balance PMS native, ignorant le filtre asset → le dashboard affiche "0 EDN" alors que la supply est > 0.
- **Fichier de référence** : `crates/pms-server/src/api_fn/supply.rs`.
- **Bug historique (v0.5.18)** : le dashboard montrait "0 EDN" pour tous les wallets car l'endpoint supply utilisait `balance_by_address()` au lieu de `balance_by_address_and_asset()`.

### Contract Store — Custom Ledgers

- **`AppState.contract_store` pointe TOUJOURS vers le RocksDB main.** Pour évaluer les transfer fees sur un custom ledger, utiliser `state.contract_store`, JAMAIS `state.store`.
- `state.store` peut être le store du custom ledger (via `LedgerInstance`), qui ne contient PAS les contrats.
- **Fichier de référence** : `crates/pms-contracts/src/engine.rs` → `evaluate_transfer()`, called from `crates/pms-server/src/api_fn/tx_helpers/` modules.
- **Bug historique (v0.5.10)** : les transfer fees étaient à 0 sur les custom ledgers car `state.store` (le store du ledger eden) était utilisé pour chercher les contrats, qui n'existent que dans le store main.

### UTXO Delta — Plain vs Encrypted Payloads

- `persist_block()` construit un `UtxoDelta` pour les payloads **plain** (transactions normales). Pour les payloads **encrypted**, le delta est `None`.
- Après `persist_block()`, appeler `apply_utxo_delta()` UNIQUEMENT pour les payloads encrypted. Les payloads plain ont déjà leur delta appliqué dans `persist_block` → double-comptage de la supply si appliqué deux fois.
- `UtxoFlatItem` DOIT inclure le champ `asset_id` — son absence cause un balance de 0 quand on filtre par asset.
- **Fichier de référence** : `crates/pms-storage/src/rocks_store/dag_storage_impl.rs` → `persist_block()`.
- **Bug historique (v0.5.15)** : la supply était doublée car `apply_utxo_delta()` était appelé pour les payloads plain ET dans `persist_block`.

## Versioning

**CRITICAL: Ne jamais oublier de mettre à jour les versions concernées lors d'une modification du code.**

Le projet utilise **5 systèmes de version** distincts. Lors de chaque changement, identifier lesquels sont impactés et les bumper :

### 1. Software Version (`Cargo.toml`)
- Fichier : `bin/Cargo.toml` et les workspace members concernés.
- Suit le **Semantic Versioning** : MAJOR (breaking) / MINOR (feature) / PATCH (bugfix).
- **OBLIGATOIRE : Chaque commit/merge sur `main` DOIT incrémenter la version.** Pas de commit sans bump de version.

### 2. DAG Protocol Version (`DAG_VERSION`)
- Fichier : `crates/pms-storage/src/migrations.rs` → constante `DAG_VERSION`.
- Suit le **Semantic Versioning**. Contrôle la compatibilité du protocole DAG.
- À incrémenter quand la structure des blocs, le format des transactions, ou la logique de consensus change.
- **MAJOR** = breaking (migration manuelle requise), **MINOR/PATCH** = auto-migrating.

### 3. Schema DB Version (`CURRENT_VER`)
- Fichier : `crates/pms-storage/src/migrations.rs` → constante `CURRENT_VER`.
- Entier incrémental (actuellement `5`). Contrôle les migrations RocksDB.
- À incrémenter **avec une nouvelle fonction `mig_X_to_Y()`** dans `crates/pms-storage/src/rocks_store/migration.rs` dès qu'un column family, un index, ou le schéma de stockage change.

### 4. P2P Protocol Version (`protocol_version`)
- Fichier : `crates/pms-config/src/config.rs` → champ `Network.protocol_version`.
- Utilisé dans les messages `Hello` et `Block` du réseau P2P.
- À incrémenter quand le format des messages réseau change.

### 5. API Version (`API_VERSION`)
- Fichier : `crates/pms-server/src/api_fn/version.rs` → constante `API_VERSION`.
- Entier incrémental (actuellement `1`). Contrôle la compatibilité de l'API REST.
- À incrémenter quand : un endpoint est ajouté/supprimé/modifié, le format d'une requête/réponse change, ou un comportement d'endpoint change.
- Exposé via `GET /v1/version` dans le champ `api_version`.
- **OBLIGATOIRE : Chaque modification touchant les routes, handlers, ou formats de l'API DOIT bumper `API_VERSION`.**

### Règles générales
- Le bump de version doit être inclus dans le **même commit** que les changements associés.
- En cas de doute, vérifier quel(s) système(s) de version sont impactés avant de commit.

## Changelog

**OBLIGATOIRE : À la fin de chaque conversation ayant produit des changements de code, mettre à jour `CHANGELOG.md`.**

- Ajouter les changements dans la section de la version en cours (la plus haute dans le fichier).
- Si la version est en développement (branche feature), utiliser `[X.Y.Z] - Unreleased`.
- Catégoriser les entrées : `### Added`, `### Fixed`, `### Performance`, `### Changed`, `### Removed`, `### Infrastructure`.
- Chaque entrée doit être concise mais suffisamment détaillée pour comprendre le changement sans lire le code.
- Inclure le scope entre parenthèses quand pertinent : `- **fix(storage)**: description`.
- Mettre à jour le tableau `Version History` en bas du fichier quand une nouvelle version est finalisée.

## Documentation — Manuel d'Instruction du Projet

**CRITIQUE : Ce projet est construit par IA. L'utilisateur ne peut pas suivre tous les changements. La documentation sert de manuel d'instruction et DOIT être maintenue à jour.**

Le projet utilise **deux systèmes de documentation complémentaires** :

### 1. Obsidian Vault — Documentation fonctionnelle (`documentation/`)

Le dossier `documentation/` est un **vault Obsidian**. Il contient la documentation haut-niveau : architecture, fonctionnalités, références, et API.

**Conventions Obsidian obligatoires :**
- Utiliser les **wikilinks** `[[nom-du-fichier]]` pour les liens internes entre fiches.
- Ajouter des **tags** en haut de chaque fiche : `#feature`, `#architecture`, `#reference`, `#api`.
- Le fichier `documentation/MOC.md` (Map of Content) est l'index principal — le mettre à jour quand une fiche est ajoutée.
- Les fiches de fonctionnalités vont dans `documentation/features/`.
- Les fiches d'architecture vont dans `documentation/architecture/`.
- Les références techniques vont dans `documentation/reference/`.
- La doc API reste dans `documentation/api/`.

**Structure obligatoire de chaque fiche fonctionnalité :**

```markdown
---
tags: [feature]
created: YYYY-MM-DD
updated: YYYY-MM-DD
version: vX.Y.Z
---

# Nom de la Fonctionnalité

## Résumé
Description concise : ce que fait la fonctionnalité et pourquoi elle existe.

## Configuration
Comment activer/configurer (TOML, env vars, runtime hot-swap).

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-server` | `src/api_fn/compliance.rs` | Endpoints API REST |
| ... | ... | ... |

## Fonctions Clés

| Fonction | Fichier | Description |
|----------|---------|-------------|
| ... | ... | ... |

## Endpoints API (si applicable)

| Méthode | Path | Description |
|---------|------|-------------|
| ... | ... | ... |

## Interactions
Liens vers les fonctionnalités liées : [[fee-distribution]], [[multi-ledger]], etc.
```

**Fiches obligatoires :**

| Catégorie | Fiches |
|-----------|--------|
| **Infrastructure** | Server/Engine, Gateway, Storage/RocksDB, Config System, P2P Network, Event System, Metrics & Monitoring, Deployment & Operations |
| **Données** | UTXO System, Wallet & Encryption, Token System, Block Payloads |
| **Protocole & Consensus** | Validation & Consensus, Milestones & Finality |
| **Fonctionnalités** | Compliance, Smart Contracts, Economics, Bridge, Activity System, Fee Distribution, NFT System, Multi-Ledger, Wallet Factory, API Key Authentication, DAG Pruning, Simulator/Game Engine, Node Rewards, Streaming API (SSE) |

### 2. rustdoc — Documentation technique du code

Chaque crate, module, struct, enum, trait, et fonction publique DOIT avoir une doc-comment (`///` ou `//!`).

**Règles rustdoc :**
- `//!` en haut de chaque `lib.rs` et `mod.rs` : description du crate/module, son rôle dans l'architecture.
- `///` sur chaque item public : structs, enums, traits, fonctions, constantes.
- Inclure des `# Examples` dans les doc-comments quand c'est pertinent.
- Les types financiers (Amount, FeePolicy, etc.) doivent documenter les invariants et précisions.
- Générer avec `cargo doc --no-deps --open` pour vérifier.

### Règle de mise à jour
- **OBLIGATOIRE : À la fin de chaque conversation ayant produit des changements de code :**
  1. Mettre à jour les fiches Obsidian des fonctionnalités impactées (date `updated`, crates, fonctions).
  2. Mettre à jour `[[MOC]]` si une nouvelle fiche a été créée.
  3. Ajouter/mettre à jour les doc-comments rustdoc sur le code modifié.
