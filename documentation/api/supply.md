# 📊 Supply API

> Endpoints pour les statistiques économiques

---

## GET `/v1/supply`

Récupère l'offre en circulation de tokens PMS.

### Response

```json
{
  "circulating_supply": "1000000.00000000",
  "total_minted": "1500000.00000000",
  "total_burned": "500000.00000000",
  "last_update_ts": 1706000000000
}
```

| Champ | Description |
|-------|-------------|
| `circulating_supply` | Tokens actuellement en circulation |
| `total_minted` | Total des tokens émis depuis Genesis |
| `total_burned` | Total des tokens brûlés (frais, burns) |
| `last_update_ts` | Timestamp de la dernière mise à jour |

### Exemple

```bash
curl http://localhost:3000/v1/supply
```

---

## GET `/v1/fee_pool`

Récupère le status du pool de frais en attente de distribution.

> 💡 Les frais sont accumulés dans le pool et distribués lors des Milestones.

### Response

```json
{
  "accumulated_fees": "123.45678900",
  "pending_refunds": "5.67890000",
  "last_distribution_ts": 1706000000000,
  "blocks_since_distribution": 150,
  "distribution_threshold": 1000
}
```

| Champ | Description |
|-------|-------------|
| `accumulated_fees` | Frais accumulés depuis le dernier Milestone |
| `pending_refunds` | Remboursements de burn en attente |
| `last_distribution_ts` | Timestamp de la dernière distribution |
| `blocks_since_distribution` | Nombre de blocs depuis |
| `distribution_threshold` | Seuil en blocs avant distribution |

### Exemple

```bash
curl http://localhost:3000/v1/fee_pool
```

---

## 💰 Distribution des frais

Les frais sont distribués selon la configuration :

| Destinataire | Pourcentage |
|--------------|-------------|
| Nœuds validateurs | Variable (configuré) |
| Treasury | Variable (configuré) |
| Burn permanent | Variable (configuré) |

> 📝 La distribution est déclenchée automatiquement lors des Milestones ou manuellement via `/admin/distribute_fees`.
