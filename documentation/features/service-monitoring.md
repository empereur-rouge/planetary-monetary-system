---
tags: [feature, infrastructure]
created: 2026-03-15
updated: 2026-03-15
version: v0.5.0
---

# Service Status Monitoring

## Resume

Monitoring en temps reel de l'etat des services d'infrastructure (Engine, Gateway, Prometheus, Simulator, Caddy). Le Gateway execute un health checker en background qui poll chaque service toutes les 20 secondes et cache le resultat. Les dashboards affichent des points colores (vert/rouge/orange) indiquant l'etat de chaque service.

## Architecture

```
Gateway (background task, poll 20s)
  |-- GET https://pms-engine:8080/internal/health  (reseau internal)
  |-- GET http://prometheus:9090/-/healthy           (reseau internal)
  |-- GET http://pms-simulator:9090/                 (reseau public)
  |-- GET http://caddy:80                            (reseau public)
  |-- Gateway = toujours "up" (implicite)
  |
  v
Cache RwLock<ServicesSnapshot>
  |
  v
GET /services/status  (JSON, pas de rate limit, pas d'auth)
  |
  +--> pms-dashboard (Svelte)     : header top-left, poll 30s
  +--> Heshima Network (React)    : footer bottom, poll 30s via /health/services proxy
```

## Configuration

### Variables d'environnement (Gateway)

| Variable | Type | Defaut | Description |
|----------|------|--------|-------------|
| `SERVICES_MONITOR` | String | (hardcoded defaults) | Liste des services a monitorer. Format : `Name\|url\|expect_body,...` |
| `SERVICES_CHECK_INTERVAL` | u64 | `20` | Intervalle entre les polls en secondes |

### Format `SERVICES_MONITOR`

```
Engine|https://pms-engine:8080/internal/health|ok,Prometheus|http://prometheus:9090/-/healthy|,Simulator|http://pms-simulator:9090/|,Caddy|http://caddy:80|
```

Chaque entree : `Name|URL|expect_body` (separees par virgule). Si `expect_body` est vide, tout status 2xx est considere "up". Si non vide, le body de la reponse doit contenir cette sous-chaine.

### Defaults hardcodes (si `SERVICES_MONITOR` absent)

| Service | URL | expect_body |
|---------|-----|-------------|
| Engine | `https://pms-engine:8080/internal/health` | `ok` |
| Prometheus | `http://prometheus:9090/-/healthy` | (vide) |
| Simulator | `http://pms-simulator:9090/` | (vide) |
| Caddy | `http://caddy:80` | (vide) |

## Crates et Fichiers

| Crate | Fichier | Role |
|-------|---------|------|
| `pms-gateway` | `src/health_checker.rs` | Background checker, types, cache |
| `pms-gateway` | `src/main.rs` | Initialisation cache + spawn + route |
| `pms-gateway` | `src/routes.rs` | Handler `services_status()` |
| `pms-dashboard` | `src/lib/ServiceStatusBar.svelte` | Composant Svelte (header top-left) |
| `pms-dashboard` | `src/lib/Dashboard.svelte` | Integration dans le header |

## Fonctions Cles

| Fonction | Fichier | Description |
|----------|---------|-------------|
| `spawn_health_checker()` | `health_checker.rs` | Spawn le background task tokio |
| `check_service()` | `health_checker.rs` | Poll un service individuel (3s connect, 5s total timeout) |
| `parse_monitored_services()` | `health_checker.rs` | Parse `SERVICES_MONITOR` ou retourne les defaults |
| `services_status()` | `routes.rs` | Handler GET, lit le cache et retourne le JSON |

## Endpoints API

| Methode | Path | Description |
|---------|------|-------------|
| GET | `/services/status` | Retourne le snapshot JSON de tous les services |

### Reponse `/services/status`

```json
{
  "services": [
    { "name": "Gateway", "status": "up", "latency_ms": 0 },
    { "name": "Engine", "status": "up", "detail": "12345 blocks", "latency_ms": 42 },
    { "name": "Prometheus", "status": "up", "latency_ms": 8 },
    { "name": "Simulator", "status": "down", "latency_ms": 3012 },
    { "name": "Caddy", "status": "up", "latency_ms": 3 }
  ],
  "checked_at": 1710504000
}
```

Status possibles : `up`, `down`, `degraded` (status 2xx mais body ne contient pas `expect_body`).

## Interactions

- [[gateway]] — Le health checker est un module du Gateway
- [[metrics-monitoring]] — Complementaire a Prometheus (monitoring infra vs metriques applicatives)
- [[simulator]] — Le simulateur est un des services monitores
- [[deployment-operations]] — Configuration Docker et env vars
