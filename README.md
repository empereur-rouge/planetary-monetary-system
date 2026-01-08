# PMS Node (DAG Protocol)

PMS (Planetary Monetary System) est un nœud blockchain implémentant un **DAG (Directed Acyclic Graph)** de blocs avec consensus basé sur la PoW (Proof of Work) et un modèle UTXO.

### ⚡ Performance
- **4000+ TPS** (transactions par seconde) en mode mainnet
- Architecture lock-free inspirée d'IOTA
- Stockage RocksDB optimisé SSD

## 🌟 Fonctionnement Global

Contrairement aux blockchains linéaires (comme Bitcoin), PMS utilise une structure de graphe où chaque bloc peut référencer **plusieurs parents**. Cela permet :
*   **Concurrence** : Plusieurs blocs peuvent être minés en parallèle sans créer de forks orphelins.
*   **Haut débit** : Le réseau "plie" le graphe pour accepter un débit de transactions plus élevé.

### Architecture

1.  **BlockDAG**
    *   Chaque bloc pointe vers `k` blocs précédents (parents).
    *   L'ensemble des blocs sans enfants est appelé les **Tips**.
    *   Un nouvel arrivant référence tous les "Tips" connus (ou un sous-ensemble) pour converger.

2.  **Consensus & Ordre**
    *   L'ordre total des événements est déterminé topologiquement.
    *   Un mécanisme de *Blue Set / Red Set* (inspiré de GhostDAG/SPECTRE) est utilisé pour résoudre les conflits et déterminer l'état final.

3.  **Modèle UTXO & Transactions**
    *   **Atomic UTXO** : Les transactions consomment des *Unspent Transaction Outputs*.
    *   **Frais** : Les frais de transaction sont implicites (Input - Output) ou explicites via un output vers l'adresse de frais du validateur.
    *   **Confidentialité** : Support natif pour des payloads chiffrés (ECIES / X25519) permettant d'envoyer des métadonnées privées on-chain.

4.  **Stockage & Performance**
    *   Base de données : **RocksDB** (paramétrée pour SSD).
    *   Validation : Signatures **ECDSA (k256)**, Proof of Work (Check bits).
    *   API : Serveur HTTP performant basé sur **Axum** (Rust).

---

## 🚀 Installation & Déploiement (Docker)

Le déploiement recommandé se fait via **Docker** pour garantir l'isolation et la portabilité.

### Prérequis
*   Docker & Docker Compose.

### 1. Démarrage Rapide

**Étape 0 : Générer l'identité Coordinateur (Optionnel mais recommandé)**
Si vous voulez devenir le coordinateur du réseau (Master Node), générez vos clés :

```bash
chmod +x setup_coordinator.sh
./setup_coordinator.sh
```
Cela crée `node1.key` (clé privée) et `coordinator_wallet.json` (backup), et met à jour automatiquement `config.docker-test.toml` avec votre clé publique.

**Étape 1 : Lancer le Cluster**
Utilisez le script unifié :

```bash
chmod +x pms.sh
./pms.sh test
```

Ce script va :
1.  Générer des certificats TLS auto-signés (pour le développement/test).
2.  Préparer les dossiers de configuration (`etc/pms`, `docker_data`).
3.  Builder l'image Docker optimisée (multi-stage).
4.  Lancer le nœud Node et le CLI.

### 2. Commandes Utiles

*   **Logs** : `docker compose logs -f node`
*   **CLI Interactif** : `docker compose exec -it node tools-cli`
*   **Arrêt** : `docker compose down`

### 3. Production

Pour un déploiement sur serveur :
1.  Copiez le projet.
2.  Remplacez les certificats dans `secrets/tls/` par de vrais certificats (ex: Let's Encrypt).
3.  Lancez `./setup_docker_node.sh`.

---

## 🧪 Tests E2E (Cluster Local)

Pour exécuter les tests de stress et de synchronisation P2P (1000 transactions, 3 nœuds) :

```bash
# 1. Lancer le cluster de test (3 nodes)
./scripts/setup_test_cluster.sh

# 2. Exécuter le test de stress
cargo test --test docker_stress_sync -- --nocapture

# 3. Arrêter et nettoyer
docker compose down
rm -rf docker_data  # Optionnel: reset complet
```



## 📖 Documentation & Opérations

Pour une gestion complète en production (Backup, Restore, Maintenance), consultez le **[Runbook des Opérations](./runbook_ops.md)**.

### Commandes Rapides
| Action | Commande |
| :--- | :--- |
| **Démarrer** | `docker compose up -d` |
| **Logs (Live)** | `docker compose logs -f --tail 100 node` |
| **Santé** | `curl -k https://127.0.0.1:8080/livez` |
| **Arrêt** | `docker compose stop node` |

---

## 🛠 Observabilité

Le nœud est instrumenté pour Prometheus et Grafana.

*   **Endpoint Métriques** : `/metrics`
*   **Sécurité** : Requiert un header `Authorization: Bearer <PMS_ADMIN_TOKEN>`
*   **Exemple** :
    ```bash
    curl -k -H "Authorization: Bearer $PMS_ADMIN_TOKEN" https://127.0.0.1:8080/metrics
    ```
*   **Logs JSON** : Activés par défaut via `RUST_LOG=info`, parsables pour Elasticsearch/Loki.
*   **Grafana** : Un dashboard prêt à l'emploi est disponible dans `etc/grafana/pms-dashboard.json`. Importez-le dans Grafana pour visualiser :
    *   Taille du DAG & Tips.
    *   Débit d'ingestion (Blocks/s).
    *   Latence de persistance.
    *   Santé réseau et erreurs.

## 🔒 Sécurité

*   **Signature Forcée** : En production (`config.prod.toml`), le nœud rejette tout bloc non signé (`require_signed_submit = true`).
*   **Protection API** :
    *   **Rate Limiting** : 50 req/s par IP (configurable) pour prévenir le DDoS.
    *   **Admin Token** : `PMS_ADMIN_TOKEN` protège les routes sensibles et l'accès aux métriques.
*   **TLS** : HTTPS forcé via Caddy (Reverse Proxy) ou configuration native Rustls.

---

## 🌐 Déploiement VPS (Production)

### Déploiement Automatisé

```bash
# Rendre le script exécutable
chmod +x scripts/deploy.sh

# Déployer sur le VPS
./scripts/deploy.sh <VPS_IP> <USER>

# Exemple
./scripts/deploy.sh 45.67.89.123 ubuntu
```

### Déploiement Manuel

1. **Build l'image Docker :**
   ```bash
   docker build -t pms-node:latest .
   ```

2. **Copier sur le VPS :**
   ```bash
   scp -r docker-compose.yml etc/ secrets/ user@vps:/opt/pms/
   ```

3. **Configurer :**
   ```bash
   # Sur le VPS
   cd /opt/pms
   cp etc/config/config.prod.template.toml etc/config/config.prod.toml
   # Éditer config.prod.toml avec vos valeurs
   ```

4. **Lancer :**
   ```bash
   docker compose up -d
   ```

### Checklist Sécurité Production

| Check | Description |
|-------|-------------|
| ☐ | Changer `admin.token` (64+ caractères aléatoires) |
| ☐ | Configurer certificats TLS réels (Let's Encrypt) |
| ☐ | Configurer `p2p.known_peers` avec les autres nœuds |
| ☐ | Limiter `auth.allowed_ips` aux IPs admin |
| ☐ | Configurer firewall (ports 80, 443, 8443) |

---

## 📦 SDK TypeScript

Le SDK officiel permet d'intégrer PMS dans vos applications.

### Installation

```bash
npm install @pms/sdk
```

### Utilisation

```typescript
import { PmsWallet, PmsClient } from "@pms/sdk";

// Créer un wallet (24 mots)
const wallet = PmsWallet.generate();
console.log(wallet.mnemonic);
console.log(wallet.address);

// Connecter au réseau
const client = new PmsClient({ nodeUrl: "https://node.pms.network" });

// Consulter la balance
const balance = await client.getBalance(wallet.address);

// Envoyer des tokens
await client.send({
  to: "04abc...",
  amount: "10.0",
  wallet,
});
```

### Développement local du SDK

```bash
cd sdk
npm install
npm test      # 25 tests
npm run build
```

