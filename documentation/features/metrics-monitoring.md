---
tags: [feature, infrastructure]
created: 2025-12-28
updated: 2026-03-19
version: v0.5.16
---

# Metrics & Monitoring (Prometheus)

## Resume

Le systeme de metriques du moteur bancaire PMS expose des compteurs et jauges au format **Prometheus text exposition** via des endpoints HTTP dedies. Ces metriques permettent le suivi en temps reel de la sante du DAG, du debit de blocs, et des rejets. L'infrastructure inclut un serveur Prometheus containerise, un dashboard Grafana pre-configure, et une integration native avec le dashboard Svelte embarque.

Le module `metrics.rs` utilise la crate `prometheus 0.14` avec des metriques **multi-ledger** (labels `ledger_id`) enregistrees via `once_cell::Lazy`. Deux modes de rendu coexistent :
- **Per-ledger** (`render_for_ledger`) : format simplifie sans labels, compatible avec le parser `parseMetrics()` du dashboard Svelte.
- **Global** (`render`) : format Prometheus standard avec labels, destine au scraping par Prometheus/Grafana.

La couche interne (`internal_api.rs`) fournit un endpoint `/internal/metrics` utilise par le Gateway pour proxifier les metriques sans exposer l'Engine directement.

---

## Metriques Disponibles

### Metriques enregistrees dans le registre Prometheus

| Nom                           | Type            | Labels       | Description                                                        | Fichier source                          |
|-------------------------------|-----------------|--------------|--------------------------------------------------------------------|-----------------------------------------|
| `pms_blocks_persisted_total`  | `IntCounterVec` | `ledger_id`  | Nombre total de blocs valides et persistes (monotone croissant)     | `crates/pms-server/src/metrics.rs:14`   |
| `pms_blocks_rejected_total`   | `IntCounterVec` | `ledger_id`  | Nombre total de blocs rejetes lors de la persistance                | `crates/pms-server/src/metrics.rs:5`    |
| `pms_blocks_total`            | `IntGaugeVec`   | `ledger_id`  | Taille actuelle du DAG en memoire (nombre de blocs connus)          | `crates/pms-server/src/metrics.rs:23`   |

### Metriques referencees dans le dashboard Grafana (reservees / futures)

Le dashboard Grafana (`etc/grafana/pms-dashboard.json`) reference egalement les metriques suivantes, qui sont prevues pour des extensions futures ou des metriques applicatives externes :

| Nom                              | Type       | Description                                                      | Statut                |
|----------------------------------|------------|------------------------------------------------------------------|-----------------------|
| `pms_tips_count`                 | gauge      | Nombre de tips actifs dans le DAG                                | Reference Grafana     |
| `pms_blocks_broadcast_total`     | counter    | Nombre total de blocs diffuses en gossip P2P                     | Reference Grafana     |
| `persist_latency_seconds_bucket` | histogram  | Distribution de la latence de persistance (buckets P95)          | Reference Grafana     |
| `persist_err_total`              | counter    | Nombre total d'erreurs de persistance (par type)                 | Reference Grafana     |

### Metriques internes (non-Prometheus)

Le module `Stats` fournit des compteurs atomiques internes (non exposes au registre Prometheus) loggues periodiquement dans les traces :

| Champ             | Type        | Description                                        | Fichier source                        |
|-------------------|-------------|----------------------------------------------------|---------------------------------------|
| `persisted_ok`    | `AtomicU64` | Blocs persistes avec succes                        | `crates/pms-server/src/stats.rs:5`    |
| `persisted_dup`   | `AtomicU64` | Blocs deja existants (duplicatas)                  | `crates/pms-server/src/stats.rs:6`    |
| `persisted_err`   | `AtomicU64` | Erreurs de persistance                             | `crates/pms-server/src/stats.rs:7`    |
| `gossip_out_ok`   | `AtomicU64` | Messages gossip sortants envoyes                   | `crates/pms-server/src/stats.rs:8`    |
| `gossip_out_rej`  | `AtomicU64` | Messages gossip sortants rejetes                   | `crates/pms-server/src/stats.rs:9`    |
| `gossip_out_err`  | `AtomicU64` | Erreurs de transmission gossip                     | `crates/pms-server/src/stats.rs:10`   |

Ces compteurs sont snapshotes toutes les ~10 secondes dans le log (`pms_stats` target) par la boucle de fond du serveur P2P.

---

## Endpoints API

### Endpoint public : `/metrics`

| Route                      | Methode | Auth                   | Description                                                                    |
|----------------------------|---------|------------------------|--------------------------------------------------------------------------------|
| `/metrics`                 | GET     | Admin token ou localhost | Metriques du ledger par defaut, format simplifie (sans labels Prometheus)       |
| `/metrics/all`             | GET     | Admin token ou localhost | Toutes les metriques de tous les ledgers, format Prometheus standard avec labels |
| `/l/{ledger_id}/metrics`   | GET     | Admin token ou localhost | Metriques d'un ledger specifique, format simplifie                             |

**Middleware d'acces** : `require_local_or_admin` -- autorise les requetes localhost sans token, sinon exige un Bearer token admin valide + verification de l'IP allowlist.

#### Format de sortie `/metrics` (per-ledger)

```
# HELP pms_blocks_total Nombre total de blocs connus (DAG size)
# TYPE pms_blocks_total gauge
pms_blocks_total 42567
# HELP pms_blocks_persisted_total Blocs valides et persistes
# TYPE pms_blocks_persisted_total counter
pms_blocks_persisted_total 42350
# HELP pms_blocks_rejected_total Blocs rejetes lors de la persistance
# TYPE pms_blocks_rejected_total counter
pms_blocks_rejected_total 12
```

#### Format de sortie `/metrics/all` (global)

Format Prometheus standard avec labels :

```
# HELP pms_blocks_total Nombre total de blocs connus (DAG size)
# TYPE pms_blocks_total gauge
pms_blocks_total{ledger_id="main"} 42567
pms_blocks_total{ledger_id="nft"} 1200
# HELP pms_blocks_persisted_total Blocs valides et persistes
# TYPE pms_blocks_persisted_total counter
pms_blocks_persisted_total{ledger_id="main"} 42350
pms_blocks_persisted_total{ledger_id="nft"} 1198
...
```

### Endpoint interne : `/internal/metrics`

| Route               | Methode | Auth       | Description                                                                  |
|----------------------|---------|------------|------------------------------------------------------------------------------|
| `/internal/metrics`  | GET     | Aucune (reseau interne) | Metriques Prometheus completes, utilisees par le Gateway comme proxy  |

Cet endpoint est enregistre dans `internal_routes()` et ecoute sur le port interne (typiquement `:3000` en architecture VPS). Le Gateway proxifie les requetes `/metrics` vers cet endpoint via le fallback catch-all.

### Synchronisation du gauge `pms_blocks_total`

La jauge `pms_blocks_total` est synchronisee avec la taille reelle du DAG en memoire **a chaque appel** de l'endpoint `/metrics` :

- `sync_dag_size_metric()` : synchronise le ledger par defaut.
- `sync_dag_size_metric_for()` : synchronise un ledger specifique.
- `sync_all_dag_size_metrics()` : synchronise tous les ledgers (utilise par `/metrics/all`).

Cette synchronisation reflete les effets du pruning du DAG -- la jauge peut diminuer lorsque des blocs anciens sont elagages.

---

## Configuration

### Prometheus (`etc/prometheus/prometheus.yml`)

```yaml
global:
  scrape_interval: 15s
  evaluation_interval: 15s

scrape_configs:
  - job_name: 'pms-engine'
    scheme: https
    tls_config:
      insecure_skip_verify: true
    static_configs:
      - targets: ['pms-engine:8080']
    metrics_path: /metrics

  - job_name: 'pms-gateway'
    scheme: https
    tls_config:
      insecure_skip_verify: true
    static_configs:
      - targets: ['pms-gateway:8443']
    metrics_path: /metrics
```

- **Scrape interval** : 15 secondes.
- **TLS** : `insecure_skip_verify: true` car les certificats internes sont auto-signes.
- **Deux jobs** : `pms-engine` (port 8080 interne) et `pms-gateway` (port 8443 public).

### Docker Compose

Le service Prometheus est declare dans les fichiers Docker Compose :

| Fichier                       | Image                      | Port expose          | Retention       |
|-------------------------------|----------------------------|----------------------|-----------------|
| `docker-compose.yml`          | `prom/prometheus:v2.45.0`  | `127.0.0.1:9091:9090` | Defaut          |
| `docker-compose.vps.yml`      | `prom/prometheus:v2.51.2`  | `9090:9090`          | 7 jours         |

Le service est place sur le reseau interne `pms-internal` (bridge, `internal: true` en production) pour isoler Prometheus du reseau public.

---

## TPS Logger (v0.5.16)

Module de diagnostic qui enregistre les metriques de throughput toutes les 10 minutes dans un fichier JSONL append-only.

### Fichier de log

- **Chemin** : `{data_dir}/tps_log.jsonl` (ex: `/home/pms/data/tps_log.jsonl` en Docker)
- **Format** : Une ligne JSON par entree, auto-contenue

```json
{
  "ts": "2026-03-19T16:50:00Z",
  "epoch_ms": 1742403000000,
  "deployment_id": "6604a1b2-3c4d5e6f",
  "ledger": "main",
  "tps_60s": 1842.5,
  "block_count": 15234567,
  "circulating_supply": "1000000.00000000",
  "total_burned": "42000.12345678",
  "node_pk": "04a1b2c3d4e5f6g7",
  "uptime_min": 30
}
```

### Champs

| Champ | Type | Description |
|-------|------|-------------|
| `ts` | string | ISO 8601 UTC timestamp |
| `epoch_ms` | u64 | Unix epoch en millisecondes |
| `deployment_id` | string | ID unique par instance (17 chars: `{timestamp_hex}-{random_hex}`) |
| `ledger` | string | ID du ledger (`"main"` ou custom) |
| `tps_60s` | f64 | TPS mesure sur les 60 dernieres secondes (via `TpsTracker`) |
| `block_count` | u64 | Estimation du nombre de blocs en RocksDB |
| `circulating_supply` | string | Supply en circulation (8 decimales) |
| `total_burned` | string | Total brule cumulatif |
| `node_pk` | string | 16 premiers caracteres de la cle publique du noeud |
| `uptime_min` | u64 | Minutes depuis le demarrage du logger |

### Usage

- Lance automatiquement au demarrage du serveur dans `api.rs` via `spawn_tps_logger()`
- Premiere entree apres 10 minutes (skip le tick immediat)
- Le `deployment_id` permet de distinguer les redemarrages des runs continus
- Aucune dependance externe (pas de chrono, pas de uuid)

---

## Crates et Fichiers

| Fichier                                                       | Role                                                                      |
|---------------------------------------------------------------|---------------------------------------------------------------------------|
| `crates/pms-server/src/metrics.rs`                            | Definition des metriques Prometheus (counters, gauges) et fonctions render |
| `crates/pms-server/src/internal_api.rs`                       | Endpoint `/internal/metrics` pour le proxy Gateway                        |
| `crates/pms-server/src/api.rs`                                | Routes `/metrics`, `/metrics/all`, `/l/{id}/metrics` + sync DAG size      |
| `crates/pms-server/src/stats.rs`                              | Compteurs atomiques internes (non-Prometheus) pour les logs               |
| `crates/pms-server/src/tps_logger.rs`                         | TPS logger JSONL — enregistrement periodique du throughput (v0.5.16)      |
| `crates/pms-server/src/server.rs`                             | Boucle de log des stats P2P (~10s) + incrementation metriques persist     |
| `crates/pms-server/src/api_fn/blocks.rs`                      | Incrementation `BLOCKS_PERSISTED` sur submit de bloc                      |
| `crates/pms-server/src/api_fn/tx_helpers.rs`                  | Incrementation `BLOCKS_PERSISTED` + `PMS_BLOCKS_TOTAL` sur transaction    |
| `crates/pms-server/src/api_fn/nft.rs`                         | Incrementation `BLOCKS_PERSISTED` sur mint/burn NFT                       |
| `crates/pms-server/src/api_fn/token.rs`                       | Incrementation `BLOCKS_PERSISTED` sur creation/mint de tokens             |
| `crates/pms-server/src/fee_distribution.rs`                   | Incrementation `BLOCKS_PERSISTED` sur distribution de frais               |
| `crates/pms-server/Cargo.toml`                                | Dependance `prometheus = "0.14.0"` et `once_cell = "1.21.3"`              |
| `etc/prometheus/prometheus.yml`                                | Configuration de scraping Prometheus                                      |
| `etc/grafana/pms-dashboard.json`                              | Dashboard Grafana pre-configure (7 panels)                                |
| `pms-dashboard/src/lib/Dashboard.svelte`                      | Dashboard Svelte embarque avec parsing des metriques                      |
| `tools/simulator/src/metrics/mod.rs`                           | Metriques internes du simulateur (TPS, latence, agents)                   |

---

## Fonctions Cles

### `crates/pms-server/src/metrics.rs`

| Fonction / Constante        | Signature                                        | Description                                                                   |
|-----------------------------|--------------------------------------------------|-------------------------------------------------------------------------------|
| `BLOCKS_REJECTED`           | `Lazy<IntCounterVec>`                            | Counter multi-ledger des blocs rejetes                                        |
| `BLOCKS_PERSISTED`          | `Lazy<IntCounterVec>`                            | Counter multi-ledger des blocs persistes                                      |
| `PMS_BLOCKS_TOTAL`          | `Lazy<IntGaugeVec>`                              | Gauge multi-ledger de la taille du DAG                                        |
| `render()`                  | `fn render() -> String`                          | Serialise toutes les metriques au format Prometheus text (avec labels)         |
| `render_for_ledger()`       | `fn render_for_ledger(ledger_id: &str) -> String` | Serialise les metriques d'un ledger specifique (format simplifie sans labels) |

### `crates/pms-server/src/api.rs`

| Fonction                        | Description                                                               |
|---------------------------------|---------------------------------------------------------------------------|
| `sync_dag_size_metric()`        | Synchronise `PMS_BLOCKS_TOTAL` avec `dag.len()` du ledger par defaut      |
| `sync_dag_size_metric_for()`    | Synchronise `PMS_BLOCKS_TOTAL` pour un ledger specifique                  |
| `sync_all_dag_size_metrics()`   | Synchronise `PMS_BLOCKS_TOTAL` pour tous les ledgers via `LedgerManager`  |

### `crates/pms-server/src/internal_api.rs`

| Fonction               | Description                                                                       |
|-------------------------|-----------------------------------------------------------------------------------|
| `internal_metrics()`    | Handler GET `/internal/metrics` -- rassemble et encode toutes les metriques       |
| `internal_routes()`     | Construit le routeur interne incluant `/internal/metrics`                          |

### `crates/pms-server/src/stats.rs`

| Fonction          | Description                                                              |
|-------------------|--------------------------------------------------------------------------|
| `Stats::new()`    | Constructeur `const` -- tous les compteurs initialises a zero            |
| `Stats::snapshot()` | Retourne un tuple `(ok, dup, err, gossip_ok, gossip_rej, gossip_err)` |

---

## Integration Grafana

Le fichier `etc/grafana/pms-dashboard.json` definit un dashboard **"PMS Node Dashboard"** (`uid: pms-node-main`) avec 7 panels :

| Panel ID | Titre                         | Type        | Expression PromQL                                              | Description                                      |
|----------|-------------------------------|-------------|----------------------------------------------------------------|--------------------------------------------------|
| 1        | Total Blocks (DAG Size)       | stat        | `pms_blocks_total`                                             | Nombre total de blocs dans le DAG                |
| 2        | Active Tips                   | stat        | `pms_tips_count`                                               | Nombre de tips actifs                            |
| 3        | Total Persisted Blocks        | stat        | `pms_blocks_persisted_total`                                   | Cumul des blocs persistes                        |
| 4        | Ingestion Rate (Blocks/s)     | stat        | `rate(pms_blocks_persisted_total[1m])`                         | Debit d'ingestion en blocs par seconde           |
| 5        | Throughput (OPS)              | timeseries  | `rate(pms_blocks_persisted_total[1m])` + `rate(pms_blocks_broadcast_total[1m])` | Persistance vs diffusion P2P     |
| 6        | Persistence Latency (P95)     | timeseries  | `histogram_quantile(0.95, rate(persist_latency_seconds_bucket[5m]))` | Percentile 95 de la latence de persistance |
| 7        | Errors & Rejections           | timeseries  | `rate(persist_err_total[1m])` + `rate(pms_blocks_rejected_total[1m])` | Taux d'erreurs et de rejets          |

**Configuration** :
- Datasource : variable `${DS_PROMETHEUS}` (type prometheus).
- Plage temporelle par defaut : 6 heures.
- Tags : `pms`, `blockchain`, `dag`.
- Plugin version : `10.0.0`.
- Schema version : `38`.

---

## Integration Dashboard Svelte

Le dashboard embarque (`pms-dashboard/src/lib/Dashboard.svelte`) consomme les metriques de maniere differente de Grafana :

1. **Polling** : appel `GET /metrics` (ou `/l/{ledger_id}/metrics`) toutes les 2 secondes via `setInterval`.
2. **Parsing** : la fonction `parseMetrics()` parse le format simplifie (sans labels) ligne par ligne.
3. **Metriques utilisees** :
   - `pms_blocks_total` : affiche dans la carte "DAG Size".
   - `pms_blocks_persisted_total` : affiche dans "Blocks Persisted" + calcul du TPS par delta entre deux polls.
4. **Calcul TPS** : `deltaBlocks / deltaTime` entre deux polls successifs, avec historique glissant de 30 points pour le graphique `PerformanceChart`.

---

## Interactions

- [[server-engine]] : le serveur P2P incremente `BLOCKS_PERSISTED` et `BLOCKS_REJECTED` a chaque bloc recu via gossip.
- [[multi-ledger]] : les metriques sont labelisees par `ledger_id`, les fonctions `sync_*` iterent sur le `LedgerManager`.
- [[gateway]] : le Gateway proxifie `/metrics` vers l'Engine via le fallback catch-all ou `/internal/metrics`.
- [[storage-rocksdb]] : la jauge `PMS_BLOCKS_TOTAL` reflete la taille du DAG apres pruning.
- [[dag-pruning]] : la synchronisation `sync_dag_size_metric()` reflète les effets du pruning sur le gauge.
- [[fee-distribution]] : `BLOCKS_PERSISTED` est incremente lors de la creation des blocs de distribution de frais.
- [[simulator]] : le simulateur a ses propres metriques internes (`MetricsSnapshot`) distinctes du systeme Prometheus.
- [[config-system]] : l'endpoint `/metrics` est protege par la meme politique d'acces admin que les routes `/admin/*`.
