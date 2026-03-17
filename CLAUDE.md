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
- Quand un bug est corrigé dans `crates/pms-core/src/concurrent_dag.rs`, vérifier systématiquement `crates/pms-storage/src/rocks_store/store.rs` (et vice-versa).
- **Tests de boundary/edge-case obligatoires** : toujours tester les scénarios limites (dernier élément, liste vide, overflow) — pas seulement le cas nominal. Les bugs critiques se cachent dans les edge cases que les tests "happy path" ne couvrent pas.

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
