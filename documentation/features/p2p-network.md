---
tags: [feature, infrastructure, networking]
created: 2025-12-28
updated: 2026-03-14
version: v0.3.0
---

# P2P Network (Reseau Pair-a-Pair)

## Resume

Le systeme P2P de DAG-PMS implemente un protocole de gossip pour la diffusion et la synchronisation de blocs entre noeuds. Il repose sur :

- **TLS mutuel (mTLS)** via `rustls` / `tokio-rustls` pour chiffrer et authentifier les connexions inter-noeuds.
- **Protocole JSONL** (JSON Lines) : chaque message est une ligne JSON serialisee (`NetMsg`), lue/ecrite de facon asynchrone via Tokio.
- **Whitelist stricte** (`strict_whitelist` + `allowed_peer_ips`) pour restreindre les pairs autorises sur un reseau prive.
- **Handshake avec anti-rejeu** : echange `Hello` / `HelloAck` avec nonce, `node_id` (cle publique du wallet), et verification du `proto` version.
- **Gossip par Inv/GetBlock** : diffusion legere d'inventaires (`Inv`) avec batching (lots de 100 IDs, flush toutes les 10ms), suivi de demandes ciblees (`GetBlock` / `GetBlocks`).
- **Synchronisation active** : requetes periodiques `GetTips` toutes les ~10 secondes, resolution recursive d'orphelins toutes les 2 secondes.
- **Anti-abus** : rate limiting par token bucket par pair, taille max de message (10 MiB), limite d'erreurs de parsing, idle timeout (60s), cache LRU d'inventaires deja vus.
- **Multi-ledger** : routage automatique des blocs vers le bon ledger via le `network_id` du `WireBlock`.

## Architecture

```
+-------------+        TLS/TCP         +-------------+
|   Node A    |<======================>|   Node B    |
|  (Server)   |    JSONL (NetMsg)      |  (Server)   |
|             |                        |             |
| +---------+ |                        | +---------+ |
| | Adapter | |  Inv {ids}             | | Adapter | |
| | (Core)  | |<-----------------------| | (Core)  | |
| +---------+ |  GetBlock {id}         | +---------+ |
|      |      |----------------------->|      |      |
|      v      |  Blocks {blocks}       |      v      |
| +---------+ |<-----------------------| +---------+ |
| | RocksDB | |                        | | RocksDB | |
| +---------+ |                        | +---------+ |
+-------------+                        +-------------+
```

### Flux de connexion

1. **Inbound** : le listener TCP/TLS accepte la connexion, le serveur envoie `Hello`.
2. **Outbound** : `connect_to_peer()` resout le hostname (DNS), etablit TCP puis TLS, envoie `Hello`.
3. **Handshake** : le pair repond `HelloAck { ok: true }`. Si `proto != 1` ou `node_id` identique (loopback), la connexion est refusee.
4. **Post-handshake** : le serveur envoie immediatement `GetTips { limit: 64 }` pour demarrer la synchronisation.
5. **Idle timeout** : un pair silencieux pendant 60 secondes est deconnecte.

## Configuration

### Section `[p2p]` (TOML)

| Champ | Type | Defaut | Description |
|-------|------|--------|-------------|
| `known_peers` | `String` | `""` | Liste de pairs a connecter au demarrage (comma-separated) |
| `bind_addr` | `Option<String>` | `None` | Adresse d'ecoute P2P (ex: `"0.0.0.0:8050"`) |
| `allowed_peer_ips` | `Vec<String>` | `[]` | IPs autorisees pour les connexions P2P entrantes |
| `strict_whitelist` | `bool` | `false` | Si `true`, rejette toute connexion entrante hors `allowed_peer_ips` |

### Section `[network]` (TOML)

| Champ | Type | Defaut | Description |
|-------|------|--------|-------------|
| `mode` | `NetworkMode` | - | `dev`, `testnet`, ou `mainnet` |
| `network_id` | `String` | `"pms-dev"` | Identifiant du reseau (utilise dans le handshake et les blocs) |
| `protocol_version` | `u32` | `1` | Version du protocole P2P (verifie dans `Hello` et `Block`) |
| `symbol` | `Option<String>` | `None` | Symbole du token natif |

### Section `[tls]` (TOML)

| Champ | Type | Description |
|-------|------|-------------|
| `cert_pem` | `String` | Chemin vers le certificat PEM du serveur |
| `key_pem` | `String` | Chemin vers la cle privee PEM (PKCS#8, SEC1, ou RSA) |
| `ca_pem` | `Option<String>` | Chemin vers le CA racine (pour le client P2P mTLS) |
| `whitelist_fp256` | `Vec<String>` | Empreintes SHA-256 autorisees (optionnel) |

### Section `[client]` (TOML)

| Champ | Type | Description |
|-------|------|-------------|
| `bind_addr` | `String` | Adresse d'ecoute P2P (ex: `"0.0.0.0:8050"`) |
| `api_addr` | `String` | Adresse de l'API HTTP interne |
| `allow_insecure_tls` | `bool` | `true` en dev/testnet, force a `false` en mainnet |
| `internal_api_addr` | `Option<String>` | Adresse de l'API interne pour le Gateway |

### Regles TLS selon le mode

| Mode | TLS obligatoire | Comportement si fichiers absents |
|------|-----------------|----------------------------------|
| `mainnet` | Oui | Erreur fatale (`anyhow::bail!`) |
| `testnet` | Non | Fallback vers TCP clair avec warning |
| `dev` | Non | Fallback vers TCP clair avec warning |

## Crates et Fichiers

| Crate | Fichier | Role |
|-------|---------|------|
| `pms-wire` | `src/types.rs` | `WireBlock` (format reseau), `WireMeta`, `canonical_bytes()` |
| `pms-wire` | `src/lib.rs` | Re-exports publics |
| `pms-network` | `src/messages.rs` | Enum `NetMsg` (tous les types de messages P2P) |
| `pms-network` | `src/adapter.rs` | Trait `DagAdapter` (ancienne interface, remplace par `NetDagAdapter`) |
| `pms-network` | `src/peer.rs` | Module peer (actuellement vide) |
| `pms-network` | `src/lib.rs` | Re-exports (`adapter`, `messages`, `peer`) |
| `pms-interface` | `src/net_adapter.rs` | Trait `NetDagAdapter` (interface reseau-core active) |
| `pms-server` | `src/server.rs` | Struct `Server` : listener, handshake, gossip, orphan management |
| `pms-server` | `src/tls.rs` | `load_tls()`, `load_client_config()` (chargement PEM rustls) |
| `pms-server` | `src/limits.rs` | Constantes anti-abus (rate limits, tailles max, timeouts) |
| `pms-server` | `src/rate.rs` | `TokenBucket` (rate limiter par pair) |
| `pms-server` | `src/stats.rs` | `Stats` (compteurs atomiques : persist ok/dup/err, gossip ok/rej/err) |
| `pms-server` | `src/metrics.rs` | Metriques Prometheus (`pms_blocks_persisted_total`, `pms_blocks_rejected_total`) |
| `pms-server` | `src/node_registry.rs` | `NodeRegistry` (decouverte dynamique de noeuds via API REST) |
| `pms-server` | `src/api_fn/nodes.rs` | Endpoints REST : `POST /v1/register`, `GET /v1/nodes`, `GET /v1/peers`, `POST /v1/peers/connect` |
| `pms-server` | `src/internal_api.rs` | API interne Gateway (`/internal/health`, `/internal/tips`, `/internal/submit_block`) |
| `pms-core` | `src/net_adapter.rs` | Implementation de `NetDagAdapter` pour `CoreAdapter<S>` |
| `pms-config` | `src/config.rs` | Structs `P2pConfig`, `TlsConfig`, `Network`, `Client` |
| `pms-config` | `src/settings.rs` | `Settings` (configuration globale avec `p2p: P2pConfig`) |
| `pms-utils` | `src/handshake.rs` | `do_handshake()` (helper client pour les tests) |

## Types de Messages (`NetMsg`)

L'enum `NetMsg` definit tous les messages echanges sur le protocole P2P. La serialisation est en JSON (une ligne par message, separee par `\n`).

### Handshake

| Variante | Direction | Champs | Description |
|----------|-----------|--------|-------------|
| `Hello` | Bidirectionnel | `proto: u16`, `node_id: String`, `nonce: u64`, `ping_ms: u32` | Initiation du handshake. `proto` doit etre `1`. `node_id` = cle publique du noeud. `nonce` = anti-rejeu. |
| `HelloAck` | Reponse | `ok: bool`, `reason: Option<String>` | Acceptation ou refus. Raisons de refus : `"bad proto"`, `"loopback"`. |

### Sante de connexion

| Variante | Direction | Description |
|----------|-----------|-------------|
| `Ping` | Client -> Serveur | Requete de sante (rate-limited par le token bucket du pair) |
| `Pong` | Serveur -> Client | Reponse a `Ping` |

### Diffusion de blocs

| Variante | Direction | Champs | Description |
|----------|-----------|--------|-------------|
| `Block` | Broadcast | `id`, `parents`, `payload_json`, `nonce`, `network_id`, `protocol_version`, `signer_pk_hex`, `signature_hex`, `metadata` | Bloc complet (format reseau). Valide et persiste par le recepteur. |
| `Inv` | Broadcast | `ids: Vec<String>` | Annonce legere d'un inventaire de blocs (IDs uniquement). Declenche `GetBlock` / `GetBlocks` pour les blocs manquants. |

### Synchronisation

| Variante | Direction | Champs | Description |
|----------|-----------|--------|-------------|
| `GetTips` | Requete | `limit: usize` | Demande les tips connues du pair (bornees a `limit`). |
| `Tips` | Reponse | `ids: Vec<String>` | Liste d'IDs des tips. |
| `GetBlock` | Requete | `id: String` | Demande un bloc complet par ID. |
| `GetBlocks` | Requete | `ids: Vec<String>` | Demande un lot de blocs complets par IDs (batching). |
| `Blocks` | Reponse | `blocks: Vec<WireBlock>` | Lot de blocs complets (borne a `MAX_BLOCKS_BATCH = 512`). |

## Format Reseau (`WireBlock`)

Le `WireBlock` est le format de serialisation reseau, independant du stockage et du core.

```rust
pub struct WireBlock {
    pub id: String,              // Hash SHA-256 du bloc
    pub parents: Vec<String>,    // IDs des blocs parents
    pub payload_json: Option<String>, // Payload serialise en JSON
    pub nonce: u64,              // Nonce (PoW ou compteur)
    pub network_id: String,      // ex: "pms-dev", "pms-main"
    pub protocol_version: u16,   // ex: 1
    pub signer_pk_hex: String,   // Cle publique ECDSA (compressed, hex)
    pub signature_hex: String,   // Signature ECDSA (hex)
    pub metadata: Option<BlockMetadata>, // Metadata optionnelle
}
```

### `canonical_bytes()`

Octets canoniques utilises pour le hash et la signature. **Exclut** `signer_pk_hex`, `signature_hex`, et `metadata`. Inclut : `id`, `parents`, `payload_json`, `nonce`, `network_id`, `protocol_version`.

### `WireMeta`

Metadata reseau derivee de la configuration (`Settings`) :

```rust
pub struct WireMeta {
    pub network_id: String,       // ex: "pms-dev"
    pub protocol_version: u32,    // ex: 1
}
```

## Fonctions Cles

### Serveur P2P (`pms-server/src/server.rs`)

| Fonction | Description |
|----------|-------------|
| `Server::new()` | Construit le serveur P2P avec adapter, config reseau, wallet, et config P2P. Lance le `spawn_broadcast_worker`. |
| `Server::api_only()` | Construit un serveur sans listener P2P (pour les routes per-ledger en multi-ledger). |
| `Server::run()` | Point d'entree principal : lance l'API HTTP, le listener P2P (TLS ou TCP), la maintenance RocksDB, et les taches de sync en arriere-plan. |
| `Server::listen()` | Listener TCP clair. Boucle `accept()` + `handle_new_peer()`. |
| `Server::listen_tls()` | Listener TLS. Utilise `TlsAcceptor` (rustls). |
| `Server::handle_new_peer()` | Verifie la whitelist, split le stream, delegue a `handle_new_peer_from_io()`. |
| `Server::handle_new_peer_from_io()` | Coeur du protocol P2P : spawn 2 taches (lecture + ecriture), gere le handshake, traite les `NetMsg`. |
| `Server::connect_to_peer()` | Connexion sortante (outbound) avec resolution DNS. Supporte TCP et TLS. |
| `Server::broadcast()` | Diffuse un `NetMsg` a tous les pairs inbound (serialise une fois, partage `Arc<str>`). |
| `Server::broadcast_except()` | Diffuse a tous les pairs inbound sauf un (evite les echos). |
| `Server::unicast()` | Envoie un `NetMsg` a un pair specifique (best-effort). |
| `Server::process_incoming_blocks()` | Pipeline de traitement des blocs recus : verification parents, gestion orphelins, persistance, diffusion. |
| `Server::enqueue_broadcast()` | Ajoute un ID de bloc a la file de diffusion (batching). |
| `Server::trigger_sync()` | Nettoie les requetes en vol et broadcast `GetTips`. |
| `Server::is_peer_allowed()` | Verifie si l'IP du pair est dans `allowed_peer_ips`. |
| `Server::get_p2p_peers()` | Retourne les adresses socket des pairs connectes. |

### Broadcast Worker

Le worker d'agregation (`spawn_broadcast_worker`) optimise le reseau en groupant les diffusions :

- **Buffer** : accumule les IDs de blocs a diffuser.
- **Flush** : envoie un `Inv { ids }` tous les 10ms ou quand le buffer atteint 100 IDs.
- **Serialisation unique** : chaque `NetMsg` est serialise une seule fois en `Arc<str>`, puis clone avec un cout O(1) par pair.

### Gestion des orphelins

Quand un bloc arrive avec des parents manquants :

1. Le bloc est stocke dans le cache `orphans` (borne a `MAX_ORPHANS = 10_000`).
2. Les dependances parent-enfant sont enregistrees dans `parent_dependency` (borne a `MAX_PARENT_DEPS = 20_000`).
3. Les parents manquants sont demandes via `GetBlock` (avec tracking `inflight_fetch`).
4. Quand un parent arrive et est persiste, les orphelins dependants sont re-traites recursivement via une `VecDeque` (evite la recursion stack).
5. Un worker toutes les 2 secondes relance les demandes pour les parents non resolus.

### Synchronisation periodique

- **Toutes les ~10 secondes** : broadcast `GetTips { limit: 64 }` a tous les pairs.
- **Toutes les 2 secondes** : retry des parents manquants pour les orphelins (bounded a 100 demandes).
- **Toutes les ~10 secondes** : log des statistiques (persist ok/dup/err, gossip ok/rej/err).

### TLS (`pms-server/src/tls.rs`)

| Fonction | Description |
|----------|-------------|
| `load_tls()` | Charge la config TLS serveur depuis les fichiers PEM (cert + cle). ALPN : `h2`, `http/1.1`. |
| `load_client_config()` | Charge la config TLS client pour mTLS (cert + cle + CA optionnel). |

### Interface Reseau-Core (`pms-interface/src/net_adapter.rs`)

Le trait `NetDagAdapter` definit l'interface entre le serveur P2P et le moteur DAG :

| Methode | Description |
|---------|-------------|
| `have_block(id)` | Verifie si le bloc existe (RAM DAG + RocksDB fallback). |
| `persist_block(wb)` | Pipeline complet : validation wire-level, signature ECDSA, single-writer, UTXO, persistance. Retourne `PutResult::Inserted / AlreadyExists / Rejected(reason)`. |
| `broadcast_block(wb)` | Diffuse un bloc aux pairs (fire-and-forget). |
| `top_tips(limit)` | Retourne les tips les plus recents (RAM DAG + store). |
| `get_block(id)` | Recupere un bloc par ID depuis le store. |
| `get_blocks_by_ids(ids)` | Recupere un lot de blocs par IDs. |

### Multi-Ledger P2P

Le serveur supporte le routage multi-ledger transparent :

| Methode | Description |
|---------|-------------|
| `adapter_for_network(network_id)` | Resout l'adapter pour un `network_id` donne (via `LedgerManager`). |
| `all_adapters()` | Retourne tous les adapters (un par ledger). |
| `have_block_any(id)` | Verifie l'existence d'un bloc dans tous les ledgers. |
| `get_block_any(id)` | Recupere un bloc depuis n'importe quel ledger. |
| `all_tips(limit)` | Agrege les tips de tous les ledgers. |

## Protection Anti-Abus

### Constantes (`pms-server/src/limits.rs`)

| Constante | Valeur | Description |
|-----------|--------|-------------|
| `MAX_LINE_BYTES` | 10 MiB | Taille max d'un message JSONL. Depassement = deconnexion. |
| `PER_PEER_Q_CAP` | 10 000 | Capacite de la file de sortie par pair. |
| `RATE_MSGS_PER_SEC` | 10 000 | Token bucket : messages/seconde par pair. |
| `RATE_BURST` | 20 000 | Token bucket : burst max par pair. |
| `HANDSHAKE_TIMEOUT_MS` | 1 500 ms | Timeout du handshake. |
| `PING_EVERY_MS` | 1 000 ms | Intervalle de ping. |
| `MAX_PARSE_ERRORS` | 8 | Nombre max d'erreurs de parsing avant kick. |
| `SEEN_CAPACITY` | 10 000 | Taille du cache LRU des Inv deja vus. |
| `SEEN_TTL_MS` | 5 000 ms | TTL des entrees dans le cache LRU. |
| `MAX_BLOCKS_BATCH` | 512 | Nombre max de blocs dans un message `Blocks`. |
| `MAX_INFLIGHT_GETBLOCK` | 100 000 | Nombre max de requetes `GetBlock` en vol. |
| `INFLIGHT_TTL_MS` | 10 000 ms | TTL des requetes en vol. |
| `MAX_ORPHANS` | 10 000 | Taille max du cache d'orphelins en memoire. |
| `MAX_PARENT_DEPS` | 20 000 | Taille max de la table de dependances parent-enfant. |

### Token Bucket (`pms-server/src/rate.rs`)

Chaque pair a son propre `TokenBucket` pour limiter le debit des messages. Le bucket se recharge a `RATE_MSGS_PER_SEC` tokens/seconde avec un burst max de `RATE_BURST`. Les `Ping` sont rate-limites par ce mecanisme.

### Mecanismes de deconnexion

1. **Message trop long** (`> MAX_LINE_BYTES`) : deconnexion immediate.
2. **Trop d'erreurs de parsing** (`> MAX_PARSE_ERRORS`) : deconnexion.
3. **Handshake timeout** (`> HANDSHAKE_TIMEOUT_MS`) : deconnexion.
4. **Idle timeout** (60 secondes sans activite) : deconnexion.
5. **Loopback** (meme `node_id`) : refus via `HelloAck { ok: false, reason: "loopback" }`.
6. **Proto incompatible** (`proto != 1`) : refus via `HelloAck { ok: false, reason: "bad proto" }`.

## Node Registry (Decouverte de Noeuds)

Le `NodeRegistry` permet la decouverte dynamique de noeuds via l'API REST :

| Endpoint | Methode | Description |
|----------|---------|-------------|
| `/v1/register` | `POST` | Un noeud s'enregistre avec `node_pk`, `api_url`, et `wallet_address`. |
| `/v1/heartbeat` | `POST` | Heartbeat pour maintenir l'enregistrement. |
| `/v1/nodes` | `GET` | Liste des noeuds actifs (melangee aleatoirement pour la distribution). |
| `/v1/peers` | `GET` | Liste des pairs P2P connectes (adresses socket). |
| `/v1/peers/connect` | `POST` | Connexion manuelle a un pair P2P (hostname ou IP, supporte DNS). |

Le TTL d'un noeud est de 24 heures (`NODE_TTL_SECONDS = 86400`). Les noeuds inactifs sont nettoyes automatiquement.

## Metriques Prometheus

| Metrique | Type | Description |
|----------|------|-------------|
| `pms_blocks_persisted_total` | Counter (par ledger) | Blocs valides et persistes via P2P ou API. |
| `pms_blocks_rejected_total` | Counter (par ledger) | Blocs rejetes lors de la validation/persistance. |
| `pms_blocks_total` | Gauge (par ledger) | Nombre total de blocs connus (taille du DAG). |

Les statistiques internes (`Stats`) comptent en plus :
- `persisted_ok` / `persisted_dup` / `persisted_err`
- `gossip_out_ok` / `gossip_out_rej` / `gossip_out_err`

Ces statistiques sont logguees toutes les ~10 secondes par le worker de synchronisation.

## Optimisations de Performance

1. **Serialisation unique** : chaque message broadcast est serialise une seule fois en `Arc<str>` (clone O(1) par pair).
2. **BufWriter avec flush periodique** (5ms) : reduit le overhead des records TLS.
3. **Broadcast batching** : les Inv sont groupes par lots de 100 IDs avec flush toutes les 10ms.
4. **Inflight deduplication** : les requetes `GetBlock` en vol sont trackees pour eviter les doublons (TTL 10s).
5. **Orphan queue iterative** : la re-tentative des orphelins utilise une `VecDeque` au lieu de la recursion (pas de stack overflow).
6. **Broadcast selectif** : seuls les pairs inbound recoivent les broadcasts (les outbound sont des connexions ou le noeud lit, pas ecrit).

## Tests

| Fichier de test | Description |
|-----------------|-------------|
| `pms-network/tests/two_nodes_mvp.rs` | Deux noeuds RocksDB partagent des blocs via le protocole P2P. Verifie la convergence apres diffusion. |
| `pms-network/tests/anti_abuse.rs` | Tests anti-abus : ping/pong, oversize message, parse errors kick, rate limiting. |
| `pms-network/tests/validate_before_relay.rs` | Un bloc invalide (self-parent, signature vide) n'est ni persiste ni re-diffuse. |
| `pms-network/tests/rate_limit_and_size.rs` | Rate limiting + oversize message sur un noeud RocksDB ephemere. |
| `pms-network/tests/bootstrap_tips_then_fetch_chain.rs` | Un noeud B se connecte a A apres que A ait mine une chaine. B rattrape via GetTips + GetBlocks. |
| `pms-server/tests/dynamic_connection.rs` | Connexion dynamique entre deux noeuds via `connect_to_peer()`. Verifie le handshake et l'etat des peers. |
| `pms-server/tests/ip_allowlist.rs` | Tests unitaires du middleware IP allowlist (CIDR matching, localhost, IPv6). |
| `pms-server/tests/p2p_orphan_parallel.rs` | Tests de gestion des orphelins en parallele. |
| `pms-server/tests/submit_block_auth.rs` | Soumission de blocs authentifies. |
| `pms-wire/tests/wire_tests.rs` | Serialisation/deserialisation `WireBlock`, `canonical_bytes()` determinisme, conversion vers `Block`. |

## Interactions

- [[utxo-system]] -- La validation UTXO (double-spend, balances) est effectuee dans `persist_block()` avant l'insertion dans le DAG.
- [[fee-distribution]] -- Les fees sont accumulees dans le fee pool lors de la persistance des blocs recus via P2P.
- [[multi-ledger]] -- Le serveur P2P route les blocs vers le bon ledger via `adapter_for_network(network_id)`.
- [[dag-pruning]] -- Le DAG en RAM est borne (`max_dag_blocks`). Les blocs pruned restent accessibles via RocksDB pour les requetes P2P `GetBlock`.
- [[smart-contracts]] -- Les blocs `ContractRegister` et `ContractUpdate` sont valides dans le pipeline `persist_block()`.
- [[compliance]] -- Les adresses gelees (`Freeze`) sont verifiees avant la persistance des transactions recues via P2P.
- [[bridge]] -- Les blocs `BridgeLock` et `BridgeMint` sont traites dans le pipeline standard de persistance P2P.
