# PMS Node (DAG Protocol)

PMS (Planetary Monetary System) est un nœud blockchain implémentant un **DAG (Directed Acyclic Graph)** de blocs avec consensus basé sur la PoW (Proof of Work) et un modèle UTXO.

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

Utilisez le script de setup qui prépare les volumes et lance les conteneurs :

```bash
chmod +x setup_docker_node.sh
./setup_docker_node.sh
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
*   **Sécurité** : Requiert un header `Authorization: Bearer <PMS_ADMIN_TOKEN_DEV>`
*   **Exemple** :
    ```bash
    curl -k -H "Authorization: Bearer pms_admin_secret" https://127.0.0.1:8080/metrics
    ```
*   **Logs JSON** : Activés par défaut via `RUST_LOG=info`, parsables pour Elasticsearch/Loki.

## 🔒 Sécurité

*   **Signature Forcée** : En production (`config.prod.toml`), le nœud rejette tout bloc non signé (`require_signed_submit = true`).
*   **Protection API** :
    *   **Rate Limiting** : 1000 req/s (ajustable) pour prévenir le DDoS.
    *   **Admin Token** : Protège les routes sensibles et l'accès aux métriques.
*   **TLS** : HTTPS forcé via Caddy (Reverse Proxy) ou configuration native Rustls.
