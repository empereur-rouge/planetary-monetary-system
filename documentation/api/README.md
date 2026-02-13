# API Reference - DAG PMS

> Documentation complete de l'API REST du serveur PMS (Planetary Monetary System)

## Vue d'ensemble

L'API PMS expose des endpoints REST pour interagir avec le **DAG centralise prive**.

> [!IMPORTANT]
> PMS est une infrastructure **custodiale** : le serveur est l'autorite finale de validation.
> Cette API est destinee a des usages internes (gaming, core banking, wallets custodial).

**Base URL**: `https://localhost:8443` (Gateway Public Port)

---

## Table des matieres

| Section | Description |
|---------|-------------|
| [Health & Status](./health.md) | Verification de l'etat du serveur |
| [Wallet](./wallet.md) | Gestion des portefeuilles, balances, creation et envoi simplifie |
| [Transactions](./transactions.md) | Envoi de tokens (multi-asset) et soumission de blocs |
| [Tokens](./tokens.md) | Registre des tokens custom (creation, mint, listing) |
| [NFT](./nft.md) | Mint, burn et query des NFTs |
| [Cube](./cube.md) | Systeme CUBE (claim et burn vers PMS) |
| [History](./history.md) | Historique des transactions |
| [Supply](./supply.md) | Statistiques de l'offre en circulation |
| [DAG](./dag.md) | Operations sur le graphe |
| [Nodes](./nodes.md) | Registre des noeuds distribues et peers P2P |
| [Ledgers](./ledgers.md) | Gestion multi-ledger (creation, listing, routage dynamique) |
| [Bridge](./bridge.md) | Pont cross-ledger (transferts entre ledgers) |
| [Compliance](./compliance.md) | Conformite reglementaire (gel, saisie, inversion) |
| [Admin](./admin.md) | Endpoints proteges d'administration (config, fees, faucet, metrics) |

---

## Authentification

### Endpoints publics
La plupart des endpoints sont publics et ne necessitent pas d'authentification.

### Endpoints Admin
Les endpoints `/admin/*` et `/metrics` sont proteges par :

1. **IP Localhost** : Toujours autorise
2. **IP Allowlist** : Si configure, l'IP doit etre dans la whitelist
3. **Bearer Token** : Header `Authorization: Bearer <token>`

```http
Authorization: Bearer votre_token_admin
```

---

## Rate Limiting (Gateway)

| Type | Limite |
|------|--------|
| **Global** | 10000 req/s, burst 20000 (Configurable) |
| **NFT Mint/Burn** | 5 req/s, burst 10 |

---

## Format des reponses

Toutes les reponses sont en **JSON**.

### Succes
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

## Quick Start

### Verifier que le serveur est actif
```bash
curl -k https://localhost:8443/livez
# Reponse: ok
```

### Creer un wallet
```bash
curl -k -X POST https://localhost:8443/v1/wallet/create
```

### Obtenir le solde d'une adresse
```bash
curl -k -X POST https://localhost:8443/v1/balance \
  -H "Content-Type: application/json" \
  -d '{"address": "pms1..."}'
```

### Envoyer des tokens (custodial)
```bash
curl -k -X POST https://localhost:8443/v1/wallet/send-simple \
  -H "Content-Type: application/json" \
  -d '{
    "private_key_b64": "MHQCAQEEIFm0...",
    "to": "pms1recipient...",
    "amount": "100.0"
  }'
```

### Lister les NFTs d'un wallet
```bash
curl -k https://localhost:8443/v1/wallet/{address}/nfts
```

---

## Endpoints par categorie

### Health & Status
| Methode | Endpoint | Description |
|---------|----------|-------------|
| GET | `/livez` | Check process UP |
| GET | `/healthz` | Check DB + Ready |
| GET | `/live` | Alias de /livez |
| GET | `/ready` | Alias de /healthz |

### Wallet
| Methode | Endpoint | Description |
|---------|----------|-------------|
| POST | `/v1/wallet/create` | Generer un nouveau wallet |
| POST | `/v1/wallet/send-simple` | Envoi custodial one-shot |
| POST | `/wallet/balance` | Solde avec UTXOs decryptes |
| POST | `/wallet/tx/send` | Envoyer une TX pre-signee |
| POST | `/wallet/history` | Historique du wallet |
| POST | `/v1/balance` | Solde simple par adresse |
| GET | `/v1/wallet/{address}/utxos` | UTXOs d'une adresse |

### Transactions
| Methode | Endpoint | Description |
|---------|----------|-------------|
| POST | `/v1/tx/prepare` | Preparer une TX non-signee (multi-asset) |
| POST | `/submit/block` | Soumettre un bloc signe |
| GET | `/blocks/stream` | Stream SSE des blocs |

### Tokens
| Methode | Endpoint | Description |
|---------|----------|-------------|
| GET | `/v1/tokens` | Lister tous les tokens |
| GET | `/v1/tokens/{asset_id}` | Info d'un token |
| POST | `/admin/tokens/create` | Creer un token (admin) |
| POST | `/admin/tokens/mint` | Minter des tokens (admin) |

### NFT
| Methode | Endpoint | Description |
|---------|----------|-------------|
| GET | `/v1/nft/{token_id}` | Info d'un NFT |
| GET | `/v1/wallet/{address}/nfts` | NFTs d'un wallet |
| POST | `/v1/nft/mint` | Minter un NFT |
| POST | `/v1/nft/burn` | Bruler un/des NFT(s) |
| POST | `/v1/nft/transfer/prepare` | Preparer un transfert NFT |

### Cube
| Methode | Endpoint | Description |
|---------|----------|-------------|
| POST | `/v1/cube/claim` | Claim 1000 CUBE |
| POST | `/v1/cube/burn` | Burn CUBE -> PMS (10:1) |

### History
| Methode | Endpoint | Description |
|---------|----------|-------------|
| GET | `/v1/history/encrypted` | Blocs chiffres pagines |
| GET | `/v1/history/plain` | Blocs plain pagines |

### Supply
| Methode | Endpoint | Description |
|---------|----------|-------------|
| GET | `/v1/supply` | Offre en circulation |
| GET | `/v1/fee_pool` | Status du pool de frais |

### DAG
| Methode | Endpoint | Description |
|---------|----------|-------------|
| POST | `/v1/dag/tips` | Tips actuels du DAG |
| GET | `/v1/config` | Configuration reseau |
| GET | `/v1/blocks/{id}` | Recuperer un bloc par ID |
| GET | `/v1/coordinator/info` | Cles publiques du coordinateur |

### Nodes & Peers
| Methode | Endpoint | Description |
|---------|----------|-------------|
| POST | `/v1/register` | Enregistrer un noeud |
| GET | `/v1/nodes` | Liste des noeuds |
| POST | `/v1/heartbeat` | Heartbeat d'un noeud |
| GET | `/v1/peers` | Liste des peers P2P |
| POST | `/v1/peers/connect` | Connecter a un peer |

### Ledgers (Multi-Ledger)
| Methode | Endpoint | Description |
|---------|----------|-------------|
| GET | `/v1/ledgers` | Lister les ledgers actifs |
| GET | `/admin/ledgers` | Liste detaillee (admin) |
| GET | `/admin/ledgers/{id}` | Detail d'un ledger (admin) |
| POST | `/admin/ledgers/create` | Creer un ledger (admin) |

### Bridge (Cross-Ledger)
| Methode | Endpoint | Description |
|---------|----------|-------------|
| GET | `/v1/bridge/links` | Lister les ponts |
| GET | `/v1/bridge/status/{id}` | Statut d'un transfert |
| POST | `/admin/bridge/enable` | Activer un pont (admin) |
| POST | `/admin/bridge/disable` | Desactiver un pont (admin) |
| POST | `/admin/bridge/transfer` | Transfert cross-ledger (admin) |

### Compliance (Admin)
| Methode | Endpoint | Description |
|---------|----------|-------------|
| POST | `/admin/compliance/freeze` | Geler une adresse |
| POST | `/admin/compliance/unfreeze` | Degeler une adresse |
| POST | `/admin/compliance/seize` | Saisir des UTXOs |
| POST | `/admin/compliance/reverse` | Inverser une transaction |
| GET | `/admin/compliance/frozen` | Liste des adresses gelees |
| GET | `/admin/compliance/log` | Journal de compliance |
| GET | `/admin/compliance/shadow_balance` | Balances des comptes geles |

### Admin (Protege)
| Methode | Endpoint | Description |
|---------|----------|-------------|
| GET | `/admin/ping` | Test de connectivite |
| POST | `/admin/compact` | Compacter RocksDB |
| POST | `/admin/distribute_fees` | Distribuer les frais manuellement |
| POST | `/admin/faucet` | Faucet (dev/testnet) |
| GET/POST | `/admin/config` | Lire/modifier la config runtime |
| GET | `/metrics` | Metriques Prometheus |
| GET | `/metrics/all` | Metriques Prometheus (tous les ledgers) |
| GET | `/l/{id}/metrics` | Metriques d'un ledger specifique |

---

## Multi-Ledger Routing

Quand le multi-ledger est actif, toutes les routes ledger-scoped sont disponibles sous `/l/{ledger_id}/` :

```bash
# Balance sur le ledger "gaming"
curl -k -X POST https://localhost:8443/l/gaming/v1/balance \
  -H "Content-Type: application/json" \
  -d '{"address": "pms1..."}'
```

Les routes sans prefixe `/l/` pointent vers le ledger par defaut ("main").

---

## Voir aussi

- [README principal](../../README.md)
- [CHANGELOG](../../CHANGELOG.md)
- [SDK TypeScript](../../sdk/README.md)
