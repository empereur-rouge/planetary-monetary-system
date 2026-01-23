# 🔷 DAG API

> Endpoints pour interagir avec le graphe acyclique dirigé

---

## POST `/v1/dag/tips`

Récupère les tips actuels du DAG (blocs sans enfants).

> 💡 Les tips sont nécessaires pour créer un nouveau bloc : il doit référencer 1-2 tips comme parents.

### Request Body

```json
{}
```

### Response

```json
{
  "tips": [
    "tip_block_id_1",
    "tip_block_id_2"
  ],
  "count": 2
}
```

### Exemple

```bash
curl -X POST http://localhost:3000/v1/dag/tips \
  -H "Content-Type: application/json" \
  -d '{}'
```

### Utilisation pour soumettre un bloc

```typescript
// 1. Récupérer les tips
const { tips } = await fetch('/v1/dag/tips', { method: 'POST' }).then(r => r.json());

// 2. Créer le bloc avec les tips comme parents
const block = {
  parents: tips.slice(0, 2), // Max 2 parents
  payload_json: JSON.stringify(payload),
  // ...
};

// 3. Soumettre
await fetch('/submit/block', {
  method: 'POST',
  body: JSON.stringify(block)
});
```

---

## GET `/v1/config`

Récupère la configuration réseau actuelle.

### Response

```json
{
  "network_id": "mainnet",
  "protocol_version": 1,
  "address_hrp": "pms",
  "pow_difficulty": 16,
  "block_time_target_ms": 1000,
  "max_parents": 2,
  "fees": {
    "transfer_rate": "0.001",
    "min_fee": "0.00000100"
  }
}
```

| Champ | Description |
|-------|-------------|
| `network_id` | Identifiant du réseau |
| `protocol_version` | Version du protocole |
| `address_hrp` | Préfixe HRP des adresses Bech32 |
| `pow_difficulty` | Bits de difficulté PoW |
| `block_time_target_ms` | Cible de temps entre blocs |
| `max_parents` | Nombre max de parents par bloc |
| `fees` | Configuration des frais |

### Exemple

```bash
curl http://localhost:3000/v1/config
```

---

## GET `/v1/coordinator/info`

Récupère les informations publiques du Coordinateur.

### Response

```json
{
  "coordinator_public_key": "04abc123...",
  "authority_public_keys": [
    "04def456...",
    "04ghi789..."
  ],
  "network_id": "mainnet"
}
```

| Champ | Description |
|-------|-------------|
| `coordinator_public_key` | Clé publique du coordinateur (signe les Milestones) |
| `authority_public_keys` | Clés des authorities (signent les Cubes) |
| `network_id` | Identifiant du réseau |

### Exemple

```bash
curl http://localhost:3000/v1/coordinator/info
```

---

## 🔄 Architecture DAG

```
     ┌─────────┐
     │ Genesis │
     └────┬────┘
          │
     ┌────┴────┐
     │ Block 1 │
     └────┬────┘
          │
    ┌─────┴─────┐
    │           │
┌───┴───┐   ┌───┴───┐
│Block 2│   │Block 3│  ← Tips (blocs sans enfants)
└───────┘   └───────┘
```

### Propriétés

- **Acyclique** : Pas de références circulaires
- **Convergent** : Les tips fusionnent vers un état commun
- **Append-only** : Les blocs ne peuvent pas être modifiés une fois ajoutés
