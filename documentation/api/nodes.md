# 🌐 Nodes API

> Endpoints pour le registre des nœuds distribués

---

## POST `/v1/register`

Enregistre un nouveau nœud dans le registre distribué.

> 💡 Utilisé pour le traitement distribué des transactions.

### Request Body

```json
{
  "node_id": "node_abc123",
  "public_key": "04abc...",
  "endpoint": "https://node1.example.com:8443",
  "capabilities": ["tx_processing", "block_validation"],
  "signature": "304..."
}
```

| Champ | Type | Description |
|-------|------|-------------|
| `node_id` | string | Identifiant unique du nœud |
| `public_key` | string | Clé publique ECDSA du nœud |
| `endpoint` | string | URL de l'API du nœud |
| `capabilities` | string[] | Capacités du nœud |
| `signature` | string | Signature de la demande |

### Response

```json
{
  "status": "ok",
  "node_id": "node_abc123",
  "registered_at": 1706000000000
}
```

---

## GET `/v1/nodes`

Liste tous les nœuds enregistrés.

### Response

```json
{
  "nodes": [
    {
      "node_id": "node_abc123",
      "endpoint": "https://node1.example.com:8443",
      "last_heartbeat": 1706000000000,
      "status": "active",
      "capabilities": ["tx_processing"]
    },
    {
      "node_id": "node_def456",
      "endpoint": "https://node2.example.com:8443",
      "last_heartbeat": 1705999000000,
      "status": "stale",
      "capabilities": ["block_validation"]
    }
  ],
  "count": 2
}
```

### Status des nœuds

| Status | Description |
|--------|-------------|
| `active` | Heartbeat récent (< 60s) |
| `stale` | Pas de heartbeat récent (60s - 5min) |
| `offline` | Pas de heartbeat (> 5min) |

---

## POST `/v1/heartbeat`

Envoie un heartbeat pour maintenir le nœud actif.

### Request Body

```json
{
  "node_id": "node_abc123",
  "timestamp": 1706000000000,
  "signature": "304..."
}
```

### Response

```json
{
  "status": "ok",
  "next_heartbeat_before": 1706000060000
}
```

### Intervalle recommandé

Envoyez un heartbeat toutes les **30 secondes** pour maintenir le status `active`.

---

## 🔄 Architecture distribuée

```
                    ┌─────────────┐
                    │ Coordinator │
                    └──────┬──────┘
                           │
           ┌───────────────┼───────────────┐
           │               │               │
    ┌──────┴──────┐ ┌──────┴──────┐ ┌──────┴──────┐
    │   Node 1    │ │   Node 2    │ │   Node 3    │
    │ (TX Proc)   │ │ (TX Proc)   │ │ (Validator) │
    └─────────────┘ └─────────────┘ └─────────────┘
```

### Rôles

| Rôle | Description |
|------|-------------|
| **Coordinator** | Signe les Milestones, distribue les récompenses |
| **TX Processor** | Traite les transactions entrantes |
| **Validator** | Valide les blocs et signatures |
