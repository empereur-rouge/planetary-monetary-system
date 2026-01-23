# 📡 API Reference - DAG PMS

> Documentation complète de l'API REST du serveur PMS (Planetary Monetary System)

## Vue d'ensemble

L'API PMS expose des endpoints REST pour interagir avec le DAG (Directed Acyclic Graph) et gérer les transactions, NFTs, et portefeuilles.

**Base URL**: `http://localhost:3000` (ou votre domaine configuré)

---

## 📋 Table des matières

| Section | Description |
|---------|-------------|
| [Health & Status](./health.md) | Vérification de l'état du serveur |
| [Wallet](./wallet.md) | Gestion des portefeuilles et balances |
| [Transactions](./transactions.md) | Envoi de tokens et soumission de blocs |
| [NFT](./nft.md) | Mint, burn et query des NFTs |
| [History](./history.md) | Historique des transactions |
| [Supply](./supply.md) | Statistiques de l'offre en circulation |
| [DAG](./dag.md) | Opérations sur le graphe |
| [Nodes](./nodes.md) | Registre des nœuds distribués |
| [Admin](./admin.md) | Endpoints protégés d'administration |

---

## 🔐 Authentification

### Endpoints publics
La plupart des endpoints sont publics et ne nécessitent pas d'authentification.

### Endpoints Admin
Les endpoints `/admin/*` et `/metrics` sont protégés par :

1. **IP Localhost** : Toujours autorisé
2. **IP Allowlist** : Si configuré, l'IP doit être dans la whitelist
3. **Bearer Token** : Header `Authorization: Bearer <token>`

```http
Authorization: Bearer votre_token_admin
```

---

## ⚡ Rate Limiting

| Type | Limite |
|------|--------|
| **Global** | 50 req/s, burst 100 |
| **NFT Mint/Burn** | 5 req/s, burst 10 |

---

## 📦 Format des réponses

Toutes les réponses sont en **JSON**.

### Succès
```json
{
  "data": { ... }
}
```

### Erreur
```json
{
  "error": "Description de l'erreur"
}
```

---

## 🚀 Quick Start

### Vérifier que le serveur est actif
```bash
curl http://localhost:3000/livez
# Réponse: ok
```

### Obtenir le solde d'une adresse
```bash
curl -X POST http://localhost:3000/v1/balance \
  -H "Content-Type: application/json" \
  -d '{"address": "pms1..."}'
```

### Lister les NFTs d'un wallet
```bash
curl http://localhost:3000/v1/wallet/{address}/nfts
```

---

## 📑 Endpoints par catégorie

### Health & Status
| Méthode | Endpoint | Description |
|---------|----------|-------------|
| GET | `/livez` | Check process UP |
| GET | `/healthz` | Check DB + Ready |
| GET | `/live` | Alias de /livez |
| GET | `/ready` | Alias de /healthz |

### Wallet
| Méthode | Endpoint | Description |
|---------|----------|-------------|
| POST | `/wallet/balance` | Solde avec UTXOs décryptés |
| POST | `/wallet/tx/send` | Envoyer des tokens |
| POST | `/wallet/history` | Historique du wallet |
| POST | `/v1/balance` | Solde simple par adresse |
| GET | `/v1/wallet/{address}/utxos` | UTXOs d'une adresse |

### NFT
| Méthode | Endpoint | Description |
|---------|----------|-------------|
| GET | `/v1/nft/{token_id}` | Info d'un NFT |
| GET | `/v1/wallet/{address}/nfts` | NFTs d'un wallet |
| POST | `/v1/nft/mint` | Minter un NFT |
| POST | `/v1/nft/burn` | Brûler un/des NFT(s) |

### History
| Méthode | Endpoint | Description |
|---------|----------|-------------|
| GET | `/v1/history/encrypted` | Blocs chiffrés paginés |
| GET | `/v1/history/plain` | Blocs plain paginés |

### Supply
| Méthode | Endpoint | Description |
|---------|----------|-------------|
| GET | `/v1/supply` | Offre en circulation |
| GET | `/v1/fee_pool` | Status du pool de frais |

### DAG
| Méthode | Endpoint | Description |
|---------|----------|-------------|
| POST | `/v1/dag/tips` | Tips actuels du DAG |
| GET | `/v1/config` | Configuration réseau |
| POST | `/submit/block` | Soumettre un bloc |
| GET | `/blocks/stream` | Stream SSE des blocs |

### Coordinator
| Méthode | Endpoint | Description |
|---------|----------|-------------|
| GET | `/v1/coordinator/info` | Clés publiques du coordinateur |

### Nodes (Distributed TX)
| Méthode | Endpoint | Description |
|---------|----------|-------------|
| POST | `/v1/register` | Enregistrer un nœud |
| GET | `/v1/nodes` | Liste des nœuds |
| POST | `/v1/heartbeat` | Heartbeat d'un nœud |

### Admin (Protégé)
| Méthode | Endpoint | Description |
|---------|----------|-------------|
| GET | `/admin/ping` | Test de connectivité |
| POST | `/admin/compact` | Compacter RocksDB |
| POST | `/admin/distribute_fees` | Distribuer les frais manuellement |
| GET | `/metrics` | Métriques Prometheus |

---

## 📚 Voir aussi

- [README principal](../../README.md)
- [CHANGELOG](../../CHANGELOG.md)
- [SDK TypeScript](../../sdk/README.md)
