# Project Rules

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

## Git Workflow
- At the start of each conversation, propose creating a new Git branch for the upcoming changes.
- When the task is complete, propose to commit the changes and merge the branch into main.

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
- **OBLIGATOIRE : Chaque commit/merge sur `main` DOIT incrémenter la version.** Cela permet d'identifier précisément quelle version du code tourne. Pas de commit sans bump de version.
- Règle de bump : PATCH pour bugfix/refactor, MINOR pour nouvelle feature, MAJOR pour breaking change.

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

## Code Quality
- Never use placeholder code, TODO stubs, or incomplete implementations. Always write the full, working code immediately.

## GitHub Integration and Version Control

**CRITICAL: All projects must use Git and GitHub for version control.**

### Initial Setup
- Initialize Git repository for ALL new projects immediately.
- Create `.gitignore` with common exclusions (`node_modules`, `__pycache__`, `.venv`, `.env`, etc.).
- NEVER commit secrets, API keys, or credentials.
- Always include `.env.example` for required environment variables.

### Branching Strategy

For solo projects:
- `main` branch for stable code.
- Feature branches: `feature/add-user-auth`
- Fix branches: `fix/memory-leak`
- Merge back to `main` when complete.

For collaborative projects:
- `main` — production-ready code.
- `develop` — integration branch.
- `feature/*` — new features.
- `hotfix/*` — urgent production fixes.

### Commit Best Practices
- Atomic commits (one logical change per commit).
- Clear messages: `"Add user authentication with JWT"`
- Include context: `"Fixes rate limiting issue causing 429 errors"`
- Reference issues when applicable: `"Closes #42"`
- Commit message format: brief description + context (1-2 sentences).

### When to Commit
- After completing a discrete feature/fix.
- Before risky refactoring (commit working state).
- After CodeRabbit review and fixes.
- Before ending work session.

### Push Frequency
- Make frequent, meaningful commits with clear messages.
- Push to GitHub regularly to maintain backup.
- After every completed and tested feature.
- At least once per work session.
- Before deployment.

### GitHub Operations
- Use `gh` CLI for GitHub operations when possible.
- Create branches for major features/experiments.
- Use GitHub Issues for tracking bugs and feature requests.

## Update PROJECT_LOG.md with Rebuild-Level Detail

**CRITICAL: `PROJECT_LOG.md` must contain sufficient detail to rebuild the entire project from the markdown alone.**

### Project Type Templates

**For Web Applications (Frontend/Backend):**
- Tech stack (framework, runtime versions).
- API endpoints with request/response schemas.
- Database schema and migrations.
- Authentication/authorization setup.
- Environment variables (`.env.example`).
- Deployment workflow (CI/CD, hosting platform).

**For CLI Tools:**
- Installation methods (`pip`, `cargo`, `go install`).
- Command-line arguments and flags.
- Configuration file formats.
- Build instructions for binaries.

**For Infrastructure/DevOps:**
- Terraform/CloudFormation configurations.
- Service topology diagrams.
- Secrets management approach.
- Monitoring and alerting setup.
