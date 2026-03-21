---
tags: [feature, api]
created: 2026-03-14
updated: 2026-03-21
version: v0.6.0
---

# Streaming API (Server-Sent Events)

## Resume

Le Streaming API fournit deux endpoints SSE (Server-Sent Events) permettant aux clients de recevoir des evenements en temps reel sans polling. Le systeme repose sur un `EventBus` interne base sur `tokio::broadcast` qui emet des evenements a chaque bloc persiste dans le DAG. Les handlers SSE d'Axum filtrent ces evenements par adresse wallet et type d'activite, puis les emettent vers le client sous forme de flux `text/event-stream`.

Le Gateway (`pms-gateway`) proxifie ces flux SSE de maniere transparente en mode streaming (non buffered), preservant la connexion longue entre le client et le Engine.

## Dates

| | Date |
|---|---|
| Creee | 2026-03-14 |
| Derniere mise a jour | 2026-03-14 |
| Version d'introduction | v0.2.0 |

## Endpoints SSE

| Endpoint | Methode | Auth | Description |
|----------|---------|------|-------------|
| `/blocks/stream` | `GET` | API Key | Retourne les N derniers blocs du DAG (polling JSON, pas SSE pur) |
| `/v1/wallet/{address}/activity/stream` | `GET` | API Key | Flux SSE temps reel des activites d'un wallet |
| `/l/{ledger_id}/v1/wallet/{address}/activity/stream` | `GET` | API Key | Idem, scope a un ledger specifique |
| `/l/{ledger_id}/blocks/stream` | `GET` | API Key | Idem `/blocks/stream`, scope a un ledger |

### Parametres de requete

#### `/blocks/stream`

| Parametre | Type | Defaut | Description |
|-----------|------|--------|-------------|
| `limit` | `usize` | 200 | Nombre de blocs a retourner (max 500) |

#### `/v1/wallet/{address}/activity/stream`

| Parametre | Type | Defaut | Description |
|-----------|------|--------|-------------|
| `type` | `String` | _(tous)_ | Filtre par type d'activite, virgule-separee (ex: `fee_received,transfer_in`) |
| `x25519_sk_hex` | `String` | _(none)_ | Cle privee X25519 (hex) pour dechiffrer les payloads chiffres en temps reel |

### Types d'activite emis

Les memes types que l'[[activity-system]] sont emis via SSE :

- `mint`, `transfer_in`, `transfer_out`, `fee_received`, `fee_paid`
- `nft_mint`, `nft_transfer_in`, `nft_transfer_out`, `nft_burn`, `nft_use`
- `reward`, `milestone`, `genesis`
- `freeze`, `unfreeze`, `seize`, `reverse`
- `encrypted` (payload chiffre sans cle de dechiffrement)

## Architecture

### Pipeline d'evenements

```
Bloc persiste (pms-core)
  |
  |  CoreAdapter::persist_block()
  |  -> Extrait payload_type + involved_addresses
  |  -> Emet PmsEvent::BlockPersisted sur l'EventBus
  |
  v
EventBus (tokio::broadcast, capacite 4096)
  |
  |  bus.subscribe() -> broadcast::Receiver<PmsEvent>
  |
  v
stream_wallet_activity() handler SSE
  |
  |  1. Filtre par involved_addresses (match adresse wallet)
  |  2. Parse PayloadEnvelope (Plain / Encrypted)
  |  3. Dechiffre si x25519_sk_hex fourni
  |  4. Classifie via classify_activity_sync()
  |  5. Filtre par type si ?type= present
  |  6. yield Event::default().event("activity").data(json)
  |
  v
Client (EventSource / fetch SSE)
```

### EventBus (`pms-event`)

Le bus d'evenements est base sur `tokio::sync::broadcast` :

- **Capacite** : 4096 evenements (configurable dans `CoreAdapter::new()`)
- **Semantique** : si un subscriber est trop lent, il recoit une erreur `Lagged(n)` et perd les `n` evenements les plus anciens
- **Thread-safe** : le `Sender` est `Clone` et `Send + Sync`, le bus est partage via `Arc` dans le `CoreAdapter`
- **Zero-subscriber** : si aucun subscriber n'ecoute, les evenements sont silencieusement ignores (pas de panic)

Le bus est accessible via le trait `NetDagAdapter::event_bus()` qui retourne un `Option<EventBus>`. Les mocks de test retournent `None` par defaut.

### Emission des evenements

L'emission se fait dans `CoreAdapter::persist_block()` (etape 9), apres la validation et la persistance du bloc :

1. **Extraction du type de payload** : via `plain_payload_type_str()` qui retourne `"Mint"`, `"TxUtxo"`, `"Milestone"`, etc.
2. **Collection des adresses impliquees** : via `pms_wallet::history::collect_involved_addresses()` qui parcourt le `PlainPayload` et extrait toutes les adresses (outputs, inputs, fee recipients, etc.)
3. **Emission** : `event_bus.emit(PmsEvent::BlockPersisted { ... })` avec le `block_id`, `ts_ms`, `payload_type`, `involved_addresses` et `payload_json`

Pour les payloads `Encrypted`, les `involved_addresses` sont vides (les adresses sont chiffrees). Le handler SSE gere ce cas en tentant le dechiffrement si une cle X25519 est fournie.

### Handler SSE : `stream_wallet_activity()`

Le handler SSE utilise `async_stream::stream!` pour creer un `Stream<Item = Result<Event, Infallible>>` qui est enveloppe dans `axum::response::sse::Sse` :

1. **Souscription** : `bus.subscribe()` cree un `broadcast::Receiver<PmsEvent>`
2. **Boucle de reception** : `rx.recv().await` attend le prochain evenement
3. **Filtrage par adresse** : verifie si `involved_addresses` contient l'adresse du wallet (case-insensitive)
4. **Gestion des payloads chiffres** :
   - `PayloadEnvelope::Encrypted` : tente le dechiffrement avec `x25519_sk_hex` si fourni
   - `PlainPayload::EncryptedReward` : tente le dechiffrement via `try_decrypt_encrypted_reward()`
   - Si le dechiffrement echoue ou si aucune cle n'est fournie, emet un `ActivityItem` avec `activity_type: "encrypted"`
5. **Classification** : `classify_activity_sync()` transforme le `PlainPayload` en `Vec<ActivityItem>`
6. **Filtrage par type** : si `?type=` est present, filtre les items par `activity_type`
7. **Emission SSE** : `yield Ok(Event::default().event("activity").data(json))`
8. **Keep-alive** : `Sse::new(stream).keep_alive(KeepAlive::default())` envoie des commentaires SSE periodiques pour maintenir la connexion

### Gestion du lag

Si un subscriber est trop lent (le buffer broadcast de 4096 evenements est plein), le handler :

1. Recoit `RecvError::Lagged(n)` au lieu d'un evenement
2. Emet un evenement SSE de type `warning` avec `{"warning": "lagged by N events"}`
3. Continue la boucle normalement (les evenements manques sont perdus)
4. Log un warning cote serveur pour monitoring

### Handler `/blocks/stream`

Ce handler est plus simple : il ne fait pas de streaming SSE mais retourne un JSON des N derniers blocs. Il utilise `adapter.recent_ids(limit)` suivi de `adapter.get_blocks_by_ids()` pour recuperer les blocs.

- Limite par defaut : 200 blocs
- Limite maximale : 500 blocs
- Retourne un `Json<Vec<WireBlock>>`

### Proxying Gateway

Le [[gateway]] (`pms-gateway`) gere les endpoints SSE de maniere speciale :

1. **Routes explicites** : les 2 endpoints SSE sont enregistres comme routes explicites (pas le fallback catch-all) pour utiliser `proxy_stream` au lieu de `proxy_fallback`
2. **`proxy_stream()`** : utilise `reqwest::Response::bytes_stream()` pour creer un `axum::body::Body::from_stream()` non-buffered qui maintient la connexion SSE ouverte
3. **Content-Type** : force `text/event-stream` si le Engine ne le specifie pas
4. **Forward d'authentification** : les headers `Authorization` et `X-API-Key` sont transmis au Engine
5. **Feature reqwest `stream`** : le crate `reqwest` est compile avec la feature `stream` pour supporter le streaming de bytes

Le proxy buffered standard (`proxy_fallback`) ne convient pas pour SSE car il attend la fin de la reponse avant de la relayer, ce qui bloquerait indefiniment.

## Format des Evenements

### Evenement `activity`

```
event: activity
data: {"block_id":"abc123...","ts_ms":1710432000000,"activity_type":"transfer_in","direction":"in","amount":"100.50","asset_id":null,"counterparty":"8e1f...","ledger_id":null,"payload":{...}}
```

Champs de l'`ActivityItem` :

| Champ | Type | Description |
|-------|------|-------------|
| `block_id` | `String` | ID du bloc contenant la transaction |
| `ts_ms` | `i64` | Timestamp Unix en millisecondes |
| `activity_type` | `String` | Type semantique (`mint`, `transfer_in`, `fee_received`, etc.) |
| `direction` | `String` | `"in"`, `"out"` ou `"info"` |
| `amount` | `Option<String>` | Montant en notation decimale (omis si non applicable) |
| `asset_id` | `Option<String>` | ID de l'asset si non-PMS natif (omis pour PMS) |
| `counterparty` | `Option<String>` | Adresse de la contrepartie (omis si non applicable) |
| `ledger_id` | `Option<String>` | ID du ledger (omis si `"main"` pour backward compat) |
| `payload` | `Value` | Payload JSON complet du bloc |

### Evenement `warning`

```
event: warning
data: {"warning":"lagged by 42 events"}
```

Emis quand le client ne consomme pas assez vite et que des evenements sont perdus.

### Keep-alive

```
: keep-alive
```

Commentaire SSE periodique (configurable via `KeepAlive::default()`) pour empecher les proxies et les pare-feux de couper la connexion inactive.

## Crates et Fichiers

| Crate | Fichier | Role |
|-------|---------|------|
| `pms-event` | `crates/pms-event/src/lib.rs` | Re-export `EventBus` + `PmsEvent` |
| `pms-event` | `crates/pms-event/src/bus.rs` | Implementation `EventBus` (wrapper `tokio::broadcast`) |
| `pms-event` | `crates/pms-event/src/events.rs` | Enum `PmsEvent` (tous les types d'evenements) |
| `pms-interface` | `crates/pms-interface/src/net_adapter.rs` | Trait `NetDagAdapter` avec methode `event_bus()` |
| `pms-core` | `crates/pms-core/src/core_adapter.rs` | `CoreAdapter` : champ `event_bus: EventBus`, capacite 4096 |
| `pms-core` | `crates/pms-core/src/net_adapter/persist.rs` | `persist_block()` : emission `BlockPersisted` (etape 9) |
| `pms-server` | `crates/pms-server/src/api_fn/activity/stream.rs` | Handler SSE `stream_wallet_activity()` |
| `pms-server` | `crates/pms-server/src/api_fn/activity/cache.rs` | `ActivityCache` |
| `pms-server` | `crates/pms-server/src/api_fn/activity/classify.rs` | `classify_activity_sync()` |
| `pms-server` | `crates/pms-server/src/api_fn/stream_blocks.rs` | Handler `stream_blocks()` (polling JSON) |
| `pms-server` | `crates/pms-server/src/api/routes.rs` | Enregistrement des routes SSE dans le router Axum |
| `pms-gateway` | `crates/pms-gateway/src/routes.rs` | Handler `proxy_stream()` pour proxying SSE |
| `pms-gateway` | `crates/pms-gateway/src/client.rs` | `EngineClient::proxy_stream()` : streaming non-buffered |
| `pms-gateway` | `crates/pms-gateway/src/main.rs` | Routes SSE explicites dans le router Gateway |
| `pms-wallet` | `crates/pms-wallet/src/history.rs` | `collect_involved_addresses()`, `try_decrypt_encrypted_reward()` |

## Fonctions Cles

### `EventBus::new(capacity: usize) -> Self`
`crates/pms-event/src/bus.rs` -- Cree un bus d'evenements avec un buffer broadcast de `capacity` evenements.

### `EventBus::emit(event: PmsEvent)`
`crates/pms-event/src/bus.rs` -- Emet un evenement sur le bus. Non-bloquant, ignore silencieusement si aucun subscriber.

### `EventBus::subscribe() -> broadcast::Receiver<PmsEvent>`
`crates/pms-event/src/bus.rs` -- Cree un nouveau subscriber. Chaque subscriber recoit une copie (Clone) de chaque evenement emis apres la souscription.

### `NetDagAdapter::event_bus() -> Option<EventBus>`
`crates/pms-interface/src/net_adapter.rs` -- Methode du trait pour acceder au bus. Retourne `None` par defaut (mocks). `CoreAdapter` retourne `Some(self.event_bus.clone())`.

### `stream_wallet_activity(State, Path, Query) -> Result<Sse<...>>`
`crates/pms-server/src/api_fn/activity/stream.rs` -- Handler SSE principal. Souscrit au bus, filtre par adresse et type, gere le dechiffrement, emet les `ActivityItem` en JSON via SSE.

### `classify_activity_sync(plain: &PlainPayload, addr: &str) -> Vec<ActivityItem>`
`crates/pms-server/src/api_fn/activity/classify.rs` -- Version synchrone de la classification (pas de UTXO lookup). Utilisee par le SSE pour eviter les acces DB dans la boucle de streaming.

### `stream_blocks(State, Query) -> Result<Json<Vec<WireBlock>>>`
`crates/pms-server/src/api_fn/stream_blocks.rs` -- Retourne les N derniers blocs du DAG en JSON (polling, pas SSE).

### `proxy_stream(State, OriginalUri, HeaderMap) -> impl IntoResponse`
`crates/pms-gateway/src/routes.rs` -- Proxy streaming du Gateway. Utilise `reqwest::bytes_stream()` + `Body::from_stream()` pour relayer le flux SSE sans buffering.

### `EngineClient::proxy_stream(path, headers) -> Result<(StatusCode, Body, Option<String>)>`
`crates/pms-gateway/src/client.rs` -- Envoie une requete GET au Engine et retourne le body en streaming via `resp.bytes_stream()`.

### `collect_involved_addresses(plain: &PlainPayload) -> Vec<String>`
`crates/pms-wallet/src/history.rs` -- Extrait toutes les adresses impliquees dans un payload (outputs, inputs, fee recipients). Utilisee par le `CoreAdapter` pour pre-calculer les adresses au moment de l'emission.

### `parse_type_filter(filter: &Option<String>) -> Vec<&str>`
`crates/pms-server/src/api_fn/activity/handler.rs` -- Parse le parametre `?type=fee_received,mint` en liste de filtres.

## Dependencies

| Crate | Utilisation |
|-------|-------------|
| `tokio::sync::broadcast` | Canal broadcast multi-consumer pour l'EventBus |
| `async-stream` | Macro `stream!` pour creer des `Stream` async avec `yield` |
| `futures-util` | Trait `Stream` requis par Axum SSE |
| `tokio-stream` | Utilitaires de streaming |
| `axum::response::sse` | Types `Sse`, `Event`, `KeepAlive` pour les reponses SSE |
| `reqwest` (feature `stream`) | Streaming de bytes pour le proxy Gateway |

## Interactions

- [[activity-system]] : le SSE reutilise les memes types `ActivityItem` et la meme logique de classification (`classify_activity_sync`) que l'endpoint REST `/v1/wallet/{address}/activity`. La difference est que le REST utilise l'index RocksDB pre-calcule, tandis que le SSE classifie en memoire a la volee.
- [[event-system]] : le crate `pms-event` fournit le bus d'evenements sous-jacent. Le variant `PmsEvent::BlockPersisted` est specifiquement concu pour le SSE activity stream, avec les champs `involved_addresses` et `payload_json` pre-extraits pour eviter les acces DB dans le handler SSE.
- [[gateway]] : le Gateway proxifie les flux SSE via `proxy_stream()` en mode non-buffered. Les routes SSE sont explicitement declarees (pas dans le fallback catch-all) pour utiliser le handler streaming dedie.
- [[wallet-encryption]] : le SSE supporte le dechiffrement en temps reel des payloads chiffres via le parametre `x25519_sk_hex`. Les `EncryptedReward` et `Encrypted` envelopes sont dechiffrees a la volee si la cle est fournie.
- [[multi-ledger]] : les routes SSE sont disponibles en mode per-ledger via `/l/{ledger_id}/v1/wallet/{address}/activity/stream`. Le `ledger_id` est inclus dans chaque `ActivityItem` emis (sauf pour le ledger `"main"` pour backward compat).

## Securite

- **Pas de validation des droits sur l'adresse** : tout client authentifie (API Key valide) peut souscrire au flux d'activite de n'importe quelle adresse. Les adresses sont publiques sur la blockchain, donc ce n'est pas un risque de confidentialite pour les payloads en clair. Pour les payloads chiffres, la cle X25519 est necessaire pour dechiffrer.
- **Cle X25519 transmise en query parameter** : la cle de dechiffrement est transmise dans l'URL (`?x25519_sk_hex=...`). En production, TLS est obligatoire pour proteger cette cle en transit. Les logs serveur ne doivent jamais logger les query parameters des endpoints SSE.
- **Rate limiting** : les connexions SSE sont soumises au rate limiting global (GovernorLayer). Chaque connexion SSE occupe une place dans le semaphore de concurrence (256 max).
- **Backpressure** : le buffer broadcast de 4096 evenements protege contre les subscribers lents. Un subscriber trop lent perd les anciens evenements (lag) mais ne bloque pas le bus.

## Exemple d'utilisation client

### JavaScript (navigateur)

```javascript
const es = new EventSource(
  'https://node.example.com/v1/wallet/pms1abc.../activity/stream?type=transfer_in,transfer_out',
  { headers: { 'X-API-Key': 'pk_live_...' } }
);

es.addEventListener('activity', (e) => {
  const item = JSON.parse(e.data);
  console.log(`${item.activity_type}: ${item.amount} (${item.direction})`);
});

es.addEventListener('warning', (e) => {
  const msg = JSON.parse(e.data);
  console.warn('Stream warning:', msg.warning);
});
```

### curl

```bash
curl -N -H "X-API-Key: pk_live_..." \
  "https://node.example.com/v1/wallet/pms1abc.../activity/stream"
```

L'option `-N` desactive le buffering de curl pour voir les evenements en temps reel.
