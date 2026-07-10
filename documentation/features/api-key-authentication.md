---
tags: [feature]
created: 2026-02-20
updated: 2026-07-10
version: v0.30.3
---

# API Key Authentication

## Résumé

Le système d'authentification par clés API protège les routes publiques du PMS Engine (`/v1/*`, `/wallet/*`, `/submit/*`, etc.) contre les accès non autorisés. Il est destiné aux clients SDK et applications tierces qui consomment l'API.

Chaque clé API est :
- **Générée côté serveur** : 32 octets aléatoires (CSPRNG) encodés en hex avec préfixe `pk_` (ex: `pk_a1b2c3d4...`).
- **Stockée sous forme de hash SHA-256** : le fichier JSON sur disque ne contient jamais la clé en clair, uniquement son hash hexadécimal.
- **Vérifiée en temps constant** : la comparaison des hashes utilise `subtle::ConstantTimeEq` pour empêcher les timing attacks. Le middleware itère sur **toutes** les clés du store même après un match, garantissant un temps d'exécution constant.
- **Protégée par des scopes** : chaque clé possède une liste de permissions (scopes) qui déterminent les endpoints accessibles. La granularité va du groupe (`"wallet"`, `"nft"`) au path exact (`"/v1/nft/mint"`), avec un wildcard `"*"` pour l'accès total.

Le système est optionnel : si aucun fichier de clés n'est configuré (`api_keys_file` absent), le middleware laisse passer toutes les requêtes (mode dev, backward-compatible). Un admin token valide (`Authorization: Bearer ...`) bypass aussi la vérification API key.

## Dates

| | Date |
|---|---|
| Créée | 2026-02-20 |
| Dernière mise à jour | 2026-02-20 |
| Version d'introduction | v0.2.0 |

## Configuration

### Fichier TOML (`settings.toml`)

La section `[auth]` du fichier de configuration contrôle l'authentification API key :

```toml
[auth]
# Chemin vers le fichier JSON des clés API SDK.
# Si absent -> pas de vérification (mode dev, backward-compatible).
api_keys_file = "etc/pms/api-keys.json"
```

En production/testnet, le chemin est typiquement `/home/pms/config/pms/api-keys.json`.

### Format du fichier JSON (`api-keys.json`)

```json
{
  "keys": [
    {
      "id": "key_01",
      "key_hash": "a1b2c3d4e5f6...sha256hex...",
      "label": "Clicker Game Prod",
      "scopes": ["wallet", "nft"],
      "active": true,
      "created_at": "2026-02-20T19:00:00Z"
    }
  ]
}
```

| Champ | Type | Description |
|-------|------|-------------|
| `id` | `String` | Identifiant unique (ex: `key_01`, `key_02`) |
| `key_hash` | `String` | Hash SHA-256 hexadécimal de la clé secrète |
| `label` | `String` | Nom descriptif pour l'administrateur |
| `scopes` | `Vec<String>` | Permissions accordées (voir section Scopes) |
| `active` | `bool` | `true` = active, `false` = révoquée (soft-delete) |
| `created_at` | `String` | Date de création ISO 8601 |

Le fichier est écrit de façon atomique (écriture dans `.json.tmp` puis `rename`) pour éviter la corruption en cas de crash.

### Header HTTP client

Les clients SDK passent la clé via le header `X-API-Key` :

```http
X-API-Key: pk_a1b2c3d4e5f6...
```

## Crates et Fichiers

| Crate | Fichier | Rôle |
|-------|---------|------|
| `pms-server` | `crates/pms-server/src/api_keys.rs` | Module principal : types, `ApiKeyStore`, CRUD, vérification, scope resolution, tests |
| `pms-server` | `crates/pms-server/src/lib.rs` | Déclaration `pub mod api_keys` |
| `pms-server` | `crates/pms-server/src/api.rs` | Middleware `require_api_key`, handlers admin (`admin_create_api_key`, `admin_list_api_keys`, `admin_revoke_api_key`), enregistrement des routes, initialisation du store dans `AppState` |
| `pms-server` | `crates/pms-server/src/helper.rs` | `is_admin_authorized()` — bypass admin utilisé par le middleware API key |
| `pms-server` | `crates/pms-server/Cargo.toml` | Dépendances `sha2 = "0.10.8"`, `subtle = "2.5"` |
| `pms-config` | `crates/pms-config/src/config.rs` | Struct `Auth` avec champ `api_keys_file: Option<String>` |
| `pms-gateway` | `crates/pms-gateway/src/client.rs` | `EngineClient::proxy_request()` et `proxy_stream()` — forward du header `X-API-Key` vers le Engine |
| `pms-gateway` | `crates/pms-gateway/src/routes.rs` | `proxy_fallback()` et `proxy_stream()` — passent les headers au client |
| `pms-testkit` | `crates/pms-testkit/src/app.rs` | Initialise `api_key_store` avec `create_api_key_store(None)` (store vide, mode test) |
| -- | `etc/config/config.testnet.toml` | `api_keys_file = "/home/pms/config/pms/api-keys.json"` |
| -- | `etc/config/config.prod.template.toml` | `api_keys_file = "/home/pms/config/pms/api-keys.json"` |
| -- | `etc/config/config.docker-test.toml` | `api_keys_file = "/home/pms/config/api-keys.json"` |
| -- | `scripts/deploy.sh` | Crée `api-keys.json` vide + génère une clé SDK au déploiement |
| -- | `scripts/deploy-testnet.sh` | Idem + restart du [[simulator|simulateur]] avec la clé API |
| -- | `tools/simulator/src/client.rs` | Client HTTP avec `with_api_key()` — attache `X-API-Key` à chaque requête |
| -- | `tools/simulator/src/config.rs` | Config [[simulator|simulateur]] avec champ `api_key: Option<String>` |

## Fonctions Clés

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `ApiKeyStore::load()` | `crates/pms-server/src/api_keys.rs` | Charge le store depuis un fichier JSON (ou crée un store vide si le fichier n'existe pas) |
| `ApiKeyStore::empty()` | `crates/pms-server/src/api_keys.rs` | Crée un store vide sans fichier (mode dev/test) |
| `ApiKeyStore::save()` | `crates/pms-server/src/api_keys.rs` | Sauvegarde atomique (tmp + rename) du store sur disque |
| `ApiKeyStore::create_key()` | `crates/pms-server/src/api_keys.rs` | Génère une clé (32 octets CSPRNG, préfixe `pk_`), hash SHA-256, stocke l'entrée, retourne la clé en clair une seule fois |
| `ApiKeyStore::list_keys()` | `crates/pms-server/src/api_keys.rs` | Liste toutes les clés avec infos publiques (sans hash) |
| `ApiKeyStore::revoke_key()` | `crates/pms-server/src/api_keys.rs` | Soft-delete : met `active = false`, la clé reste dans le fichier mais est rejetée par le middleware |
| `ApiKeyStore::verify_key()` | `crates/pms-server/src/api_keys.rs` | Hash la clé reçue, compare avec toutes les clés en temps constant (`subtle::ConstantTimeEq`), retourne l'entrée si valide et active |
| `resolve_scope()` | `crates/pms-server/src/api_keys.rs` | Mappe un path HTTP vers son scope (`"/v1/balance"` -> `"wallet"`, `"/v1/nft/mint"` -> `"nft"`, etc.) |
| `has_permission()` | `crates/pms-server/src/api_keys.rs` | Vérifie si une clé a la permission d'accéder à un path (par scope, par path exact, ou par wildcard `"*"`) |
| `hash_api_key()` | `crates/pms-server/src/api_keys.rs` | Calcule le SHA-256 d'une clé et retourne le hash en hexadécimal |
| `create_api_key_store()` | `crates/pms-server/src/api_keys.rs` | Factory : crée un `SharedApiKeyStore` (`Arc<RwLock<ApiKeyStore>>`) à partir d'un chemin optionnel |
| `require_api_key()` | `crates/pms-server/src/api.rs` | Middleware Axum : vérifie `X-API-Key` sur les routes publiques authentifiées. Bypass si admin token valide ou store vide |
| `admin_create_api_key()` | `crates/pms-server/src/api.rs` | Handler `POST /admin/api-keys` — crée une clé et retourne la clé en clair |
| `admin_list_api_keys()` | `crates/pms-server/src/api.rs` | Handler `GET /admin/api-keys` — liste toutes les clés (sans hashes) |
| `admin_revoke_api_key()` | `crates/pms-server/src/api.rs` | Handler `DELETE /admin/api-keys/{key_id}` — révoque une clé |

## Endpoints API

### Routes Admin (protégées par `require_local_or_admin`)

| Méthode | Path | Description |
|---------|------|-------------|
| `POST` | `/admin/api-keys` | Crée une nouvelle clé API. Retourne la clé en clair **une seule fois**. Body: `{"label": "...", "scopes": ["wallet", "nft"]}` |
| `GET` | `/admin/api-keys` | Liste toutes les clés enregistrées (sans les hashes ni les secrets). Retourne `{"keys": [...]}` |
| `DELETE` | `/admin/api-keys/{key_id}` | Révoque une clé (soft-delete). Retourne `{"status": "revoked", "id": "key_01"}` |

### Réponses du middleware sur les routes publiques

| Code HTTP | Condition | Corps |
|-----------|-----------|-------|
| 400 | Header `X-API-Key` présent mais encodage invalide | `{"error": "Invalid X-API-Key header encoding"}` |
| 401 | Header `X-API-Key` absent (et store non vide) | `{"error": "Missing API Key", "hint": "Add header X-API-Key: pk_live_... to your request"}` |
| 403 | Clé invalide (hash ne correspond à aucune entrée) | `{"error": "Invalid API Key"}` |
| 403 | Clé révoquée (`active = false`) | `{"error": "Invalid API Key"}` |
| 403 | Scope insuffisant pour le path demandé | `{"error": "Insufficient permissions", "scope_required": "nft", "your_scopes": ["wallet"]}` |

## Scopes

Le système définit 8 scopes dans la constante `KNOWN_SCOPES` :

| Scope | Endpoints couverts | Description |
|-------|--------------------|-------------|
| `*` | Tous les endpoints publics | Accès total (wildcard) |
| `wallet` | `/wallet/*`, `/v1/balance`, `/v1/tx/*`, `/v1/wallet/create`, `/v1/wallet/send*` | Opérations de portefeuille : envoi, solde, historique, création |
| `nft` | `/v1/nft/*`, `/v1/wallet/{addr}/nfts`, `/v1/wallet/{addr}/utxos` | Minting, burn, listing des [[nft-system|NFTs]] et UTXOs d'un wallet |
| `dag` | `/v1/dag/*`, `/v1/blocks/*`, `/v1/config`, `/submit/*`, `/blocks/*` | Lecture du DAG, soumission de blocs, streaming, configuration |
| `supply` | `/v1/supply`, `/v1/fee_pool` | Consultation de l'offre totale et du fee pool |
| `tokens` | `/v1/tokens`, `/v1/tokens/{id}` | Listing et consultation des tokens custom |
| `history` | `/v1/history/*`, `/wallet/history` | Historique des transactions (chiffré et en clair) |
| `coordinator` | `/v1/coordinator/*` | Endpoints réservés au coordinateur |

En plus des scopes nommés, un **path exact** peut être utilisé comme scope pour une granularité individuelle (ex: `"/v1/nft/mint"` autorise uniquement le minting NFT, sans accès au reste du scope `nft`).

### Logique de résolution (`resolve_scope`)

La résolution suit un ordre du plus spécifique au plus général :
1. Routes `/wallet/*`, `/v1/balance`, `/v1/tx/*`, `/v1/wallet/create`, `/v1/wallet/send*` -> `"wallet"`
2. Routes `/v1/nft/*`, `/v1/wallet/{addr}/*` -> `"nft"`
3. Routes `/v1/dag/*`, `/v1/blocks/*`, `/v1/config`, `/submit/*`, `/blocks/*` -> `"dag"`
4. Routes `/v1/supply`, `/v1/fee_pool` -> `"supply"`
5. Routes `/v1/tokens*` -> `"tokens"`
6. Routes `/v1/history/*` -> `"history"`
7. Routes `/v1/coordinator/*` -> `"coordinator"`
8. Tout le reste -> `"unknown"` (refusé sauf si la clé a `"*"`)

### Logique de permission (`has_permission`)

Pour chaque requête, le middleware vérifie dans l'ordre :
1. La clé a-t-elle le scope `"*"` ? -> accès total
2. La clé a-t-elle le scope du groupe correspondant au path ? -> accès au groupe
3. Le path demandé commence-t-il par un des scopes de la clé ? -> accès par path exact

## Interactions

### Gateway (pms-gateway)

Le gateway agit comme reverse proxy entre les clients et le Engine. Il forward transparentement le header `X-API-Key` vers le Engine dans deux fonctions :
- `EngineClient::proxy_request()` : pour les requêtes standard (GET, POST, DELETE, etc.)
- `EngineClient::proxy_stream()` : pour les requêtes streaming (SSE, blocks stream)

Le gateway ne valide pas lui-même les clés API -- c'est le Engine qui applique le middleware `require_api_key`.

### Endpoints Admin

Les endpoints CRUD (`/admin/api-keys`) sont protégés par le middleware `require_local_or_admin` qui exige :
- Une connexion depuis localhost (`127.0.0.1` ou `::1`), **ou**
- Une IP dans la liste `allowed_ips`, **ou**
- Un Bearer token admin valide (`Authorization: Bearer <admin_token>`)

Le middleware `require_api_key` inclut un **bypass admin** : si le header `Authorization` contient un admin token valide (vérifié par `is_admin_authorized()`), la requête passe sans vérification API key. Cela permet à l'administrateur d'accéder à toutes les routes sans clé API.

### Scripts de déploiement

Les scripts `deploy.sh` et `deploy-testnet.sh` automatisent la gestion des clés API :
1. **Création du fichier** : si `etc/pms/api-keys.json` n'existe pas, un fichier vide `{"keys":[]}` est créé avec permissions `chmod 600`.
2. **Génération d'une clé SDK** : après le démarrage du Engine, le script appelle `POST /admin/api-keys` avec `{"label": "SDK Default", "scopes": ["*"]}` pour créer une clé par défaut.
3. **Sauvegarde sécurisée** : la réponse (contenant la clé en clair) est sauvegardée dans `etc/pms/sdk-api-key.json` (chmod 600), puis supprimée du serveur après récupération.
4. **Injection dans le simulateur** : la clé est passée via `PMS_API_KEY` au conteneur [[simulator|simulateur]].

### Simulateur (tools/simulator)

Le [[simulator|simulateur]] utilise le champ `api_key` dans sa configuration pour attacher automatiquement le header `X-API-Key` à chaque requête HTTP via la méthode `with_api_key()` de son client HTTP. Le champ supporte la syntaxe `"env:PMS_API_KEY"` pour lire la clé depuis une variable d'environnement.

### Testkit (pms-testkit)

En mode test, le testkit initialise un `api_key_store` vide via `create_api_key_store(None)`, ce qui désactive la vérification API key (mode dev). Les tests n'ont pas besoin de clés API.

## Sécurité

### Propriétés cryptographiques

- **Hashing** : SHA-256 via la crate `sha2` (même implémentation que le reste du projet pour les blocs et transactions).
- **Comparaison temps constant** : `subtle::ConstantTimeEq` empêche les timing attacks sur la vérification des hashes. Le middleware itère sur toutes les clés même après un match.
- **Génération aléatoire** : `rand::random::<[u8; 32]>()` utilise le CSPRNG du système d'exploitation.
- **Stockage sécurisé** : seul le hash est persisté sur disque, jamais la clé en clair.
- **Écriture atomique** : le fichier JSON est écrit via un fichier temporaire puis rename, évitant la corruption.

### Fail-open sur store vide — DEV LOCAL uniquement (v0.30.3)

`empty_key_store_allows` (`middleware.rs`) décide si un store de clés VIDE laisse
passer (fail-open) ou rejette (fail-closed). Depuis v0.30.3, **seul le mode
`Dev`** (sandbox local, jamais exposé) fail-open ; **`Testnet` ET `Mainnet`
fail-closed** — un `api_keys.json` vide/non provisionné fait renvoyer 401 à
toutes les routes write API-key-gated (avant : testnet restait permissif, un
fail-open exploitable, cf. durcissement DoS v0.30.2). Un diagnostic `error!`
loud est émis au boot (`serve.rs`) si le store est vide en mode networké, pour
que l'opérateur voie la cause immédiatement. Le store testnet étant provisionné
en pratique, zéro impact nominal.

### Quota de rate limit PAR API-key (v0.30.3)

En plus du rate limit **per-IP** (`SmartIpKeyExtractor`, global), un second
`GovernorLayer` (`ApiKeyKeyExtractor`, `routes.rs`) borne le débit d'écriture
**par clé** sur `auth_ledger_write_routes` :

- **Pourquoi** : le per-IP ne stoppe pas une clé valide abusée depuis plusieurs
  IPs (botnet). Le per-key borne l'identité, indépendamment de l'IP.
- **Clé de bucket** : hash `u64` (`DefaultHasher`/SipHash) du header `X-API-Key`
  — pas la valeur en clair. Header absent → sentinelle partagée (ces requêtes
  sont de toute façon 401 hors Dev).
- **Isolation** : une clé qui dépasse son burst prend 429 ; les autres clés
  gardent un bucket frais.
- **Plafond** : `[limits].api_key_rate_rps` / `api_key_burst` (optionnels).
  **Défaut = la limite per-IP** (`Limits::effective_api_key_limits`) : prod hérite
  1000/2000, bench/e2e héritent leur per-IP desserré (pas de bottleneck).

### Limites connues

- Pas de rate limiting spécifique sur les tentatives de vérification de clé (le rate limiting global du serveur s'applique) — atténué par le quota per-key (v0.30.3) sur les routes write.
- Pas d'expiration automatique des clés (la révocation est manuelle).
- L'ID de clé est basé sur un compteur séquentiel (`key_01`, `key_02`), pas un UUID.
- Le quota per-key ne couvre que les routes **write** (pas les reads ni l'admin) et un attaquant faisant tourner de FAUSSES clés obtient un bucket frais par clé (borné par le per-IP, et rejeté 401 par l'auth).
