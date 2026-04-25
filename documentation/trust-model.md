---
tags: [security, governance, trust-model]
created: 2026-04-25
updated: 2026-04-25
version: v0.7.4
---

# Trust Model — PMS Engine

> Ce document décrit le **modèle de confiance** sur lequel le réseau PMS opère
> aujourd'hui. Il est destiné aux opérateurs, aux utilisateurs finaux, et aux
> régulateurs. Il répond à une seule question : **à qui dois-tu faire confiance,
> et pour quoi exactement ?**

---

## Résumé en une phrase

PMS est un **moteur monétaire centralisé sur un seul opérateur (le Coordinator)**
dont les actes sont **publiquement vérifiables** dans un DAG signé. Ce n'est pas
une blockchain décentralisée : le Coordinator peut faire défaut, mais il ne peut
pas mentir sans laisser de trace.

---

## Ce que le Coordinator **peut** faire

Le Coordinator détient la clé privée ECDSA secp256k1 qui signe tous les blocs du
DAG. En conséquence il a **autorité unique** sur les opérations suivantes :

### Sur la monnaie native (PMS)

- **Minter** des PMS de manière arbitraire (`PlainPayload::Mint`) — il peut
  créer la quantité qu'il veut, à n'importe quelle adresse, à n'importe quel
  moment.
- **Inverser une transaction** (`PlainPayload::Reverse`) — déplacer rétroactivement
  des UTXOs déjà créés, par exemple en cas de fraude détectée ou d'erreur
  identifiée.
- **Geler une adresse** (`PlainPayload::Freeze`) — empêcher cette adresse
  d'envoyer ou de recevoir des fonds. Le freeze peut être levé via `Unfreeze`.
- **Saisir des fonds** (`PlainPayload::Seize`) — transférer des UTXOs d'une
  adresse vers une adresse de la trésorerie.
- **Modifier la politique de frais** (`PlainPayload::ConfigUpdate`) — changer
  `fee_rate_bps`, `treasury_fee_bps`, `coordinator_fee_bps`, le seuil PoW, etc.
  Hot-swap : prend effet immédiatement sur les blocs suivants.
- **Distribuer ou retenir les frais** — `perform_fee_distribution` est piloté
  par le Coordinator. Il peut, en théorie, ne jamais déclencher la distribution
  et accumuler les frais indéfiniment.
- **Émettre des récompenses** aux nœuds (`PlainPayload::Reward`) — le montant et
  les destinataires sont à sa discrétion (via `NodeRegistry`).

### Sur les ledgers custom (Edenite, etc.)

- **Créer** un ledger custom et fixer son owner via `LedgerOwnershipTransfer`
  (chiffré X25519+AES-256-GCM, mais c'est lui qui forge le bloc).
- **Transférer** la propriété d'un ledger custom — l'owner courant signe le
  transfert, mais le bloc est inséré au DAG par le Coordinator, qui peut donc
  refuser de l'inclure.
- **Frapper** des tokens custom (Edenite, etc.) selon la politique du ledger.

### Sur les NFTs

- **Forcer** des transferts NFT (les actions `NftAction::*` sont autorisées si
  le signataire est le Coordinator OU le propriétaire actuel).
- **Brûler** des NFTs avec ou sans le consentement du propriétaire.

### Sur la disponibilité

- **Censurer** : refuser d'inclure un bloc soumis par un client. Le client
  reçoit alors `503` ou un timeout — il n'a aucun recours en réseau.
- **Arrêter** le service : éteindre le serveur, perdre la clé, ne plus signer.
  Conséquence : le réseau s'arrête. Pas de tolérance aux pannes.

---

## Ce que le Coordinator **ne peut pas** faire

Même avec la clé privée, certaines invariants sont **mathématiquement** hors de
portée du Coordinator :

- **Forger une signature utilisateur**. Un wallet utilisateur signe ses tx avec
  sa propre clé secp256k1. Le Coordinator ne peut pas signer à sa place — il
  peut juste refuser d'inclure ses tx.
- **Déchiffrer un payload chiffré sans la bonne clé X25519**. Les payloads
  `LedgerOwnershipTransfer`, les transferts NFT avec re-encryption, et les
  transactions encryptées utilisent une clé éphémère + DEK ; seul le destinataire
  prévu peut lire. (NB : le Coordinator a sa propre clé X25519 pour les flux où
  il est destinataire de routage, comme le wrap des frais — c'est documenté dans
  [[features/block-payloads]].)
- **Réécrire l'historique**. Une fois qu'un bloc `B` est diffusé et copié par
  les opérateurs / utilisateurs (via SDK / dashboard), changer son contenu
  romprait la signature et serait visible. Il peut ajouter un `Reverse(B)` qui
  annule économiquement `B`, mais `B` reste dans le DAG comme preuve écrite.
- **Falsifier rétroactivement** sans laisser de trace. Toute modification d'état
  passe par un bloc DAG signé et horodaté ; un audit du DAG complet permet de
  reconstruire la chaîne d'évènements.

---

## Single Point of Failure (SPOF)

PMS aujourd'hui tourne sur **un seul VPS** (`87.106.50.82`, IONOS). Toutes les
clés critiques y vivent. Cela définit nos points de rupture :

| Évènement | Impact | Recovery time objective (RTO) | Recovery point objective (RPO) |
|-----------|--------|-------------------------------|--------------------------------|
| **Crash process** | Service down | < 1 min (Docker restart) | 0 (RocksDB WAL atomique) |
| **Crash VPS (panne hardware)** | Service down jusqu'au redéploiement | ~30 min (restore depuis backup) | < 1 jour (snapshot quotidien) |
| **Perte de la clé Coordinator** | **Game over**. Plus aucun bloc signé possible. | Jamais — il faut redémarrer un nouveau réseau. | N/A |
| **Fuite de la clé Coordinator** | **Game over**. L'attaquant peut minter / saisir / geler. | Détection des actes anormaux + annonce publique. | N/A |
| **Corruption RocksDB** | Service down | ~1 h (restore + replay des blocs depuis backup) | < 1 jour |
| **Disque plein** | Persist queue stalle, `/healthz` passe en 503 | < 30 min (extension volume ou purge backups) | 0 (back-pressure préserve les blocs) |

### Pourquoi un seul VPS ?

Le projet est en phase de **lancement précommercial**. Un seul VPS = un seul
budget, un seul point de configuration, un seul plan d'incident à maintenir.
Ajouter un secondaire failover demande :

- Une réplication RocksDB (logical or physical replication) → non implémentée.
- Un protocole de bascule (qui devient maître ?) → non spécifié.
- Une gestion des clés hot/cold → délicat pour la clé Coordinator.

Une migration vers une architecture **2 VPS + Caddy round-robin + sync RocksDB**
est dans la roadmap post-lancement (voir « Hors scope » dans le plan de
production hardening).

---

## Backups et recovery

### Snapshots RocksDB

- Fréquence : **quotidienne** (cron job sur le VPS, à 03:00 UTC).
- Mécanisme : `RocksStore::create_checkpoint()` (atomic hard-link checkpoint
  via `rocksdb::checkpoint::Checkpoint`). Voir
  [[features/storage-rocksdb#Checkpoints]].
- Destination : volume séparé du VPS (à terme : S3 chiffré côté client).
- Rétention : 30 jours.

### Clé Coordinator — protection au repos (v0.7.4)

Depuis **v0.7.4**, la clé peut être stockée **chiffrée** sur disque
(`node.key.enc`) :

- Format : AES-256-GCM, clé dérivée par Argon2id depuis une passphrase.
- Passphrase fournie via la variable d'environnement
  `PMS_COORDINATOR_KEY_PASSPHRASE`.
- Outil pour chiffrer une clé existante :
  `tools-cli encrypt-coordinator-key <plain.key> <out.enc>`.

Voir [[features/wallet-encryption#Coordinator key at-rest]] pour les détails
cryptographiques. **Recommandation opérationnelle** : la passphrase ne doit
**jamais** être stockée en clair à côté du fichier chiffré. Source acceptable :
gestionnaire de mots de passe externe (1Password, Bitwarden) lu manuellement par
l'opérateur au démarrage du process, ou injection via systemd unit avec
`EnvironmentFile=` pointant sur un fichier `0600` non backupé.

### Procédure de restauration

En cas de perte du VPS (mais clé Coordinator préservée) :

1. Provisionner un nouveau VPS avec le même OS (Debian 12).
2. `scripts/deploy-testnet.sh <nouvelle_IP>` — restore depuis backup automatique.
3. Vérifier `GET /healthz` → tous les checks ok.
4. Vérifier la dernière transaction du DAG (`/v1/dag/recent`) correspond bien à
   ce que les utilisateurs ont vu juste avant la panne. Acceptable : perte des
   tx des dernières secondes (RPO < 1 jour, en pratique < 1 min via WAL).

En cas de perte de la clé Coordinator (catastrophe absolue) :

1. **Annoncer publiquement** que le réseau est arrêté définitivement.
2. Lancer un nouveau réseau (nouveau `network_id`, nouvelle clé) avec genesis.
3. Migrer les balances utilisateurs depuis le snapshot final, signer un Mint
   d'ouverture avec la nouvelle clé.

C'est inévitablement une **rupture de service** : il n'y a aucune autre clé
capable de continuer la chaîne signée.

---

## Différence avec Bitcoin / Ethereum

| Aspect | PMS | Bitcoin / Ethereum |
|--------|-----|--------------------|
| **Producteur de blocs** | Coordinator unique | Mineurs / validators (milliers) |
| **Consensus** | Signature du Coordinator | PoW (Bitcoin) / PoS (Ethereum) |
| **Tolérance aux pannes** | 0 (single writer) | Byzantine Fault Tolerance |
| **Censure** | Possible par le Coordinator | Coûteuse (faut posséder la majorité) |
| **Mint arbitraire** | Possible par le Coordinator | Impossible (règle de consensus) |
| **Auditabilité** | Tout le DAG signé est public | Toute la chaîne est publique |
| **Compliance (freeze/seize)** | Native, signée par le Coordinator | Pas dans le protocole (USDC le fait au niveau du contrat) |
| **Frais transactions** | Configurables, distribués/burnés selon politique | Marché libre (gas / mempool) |
| **Performance** | 10K+ TPS prouvée en local | 7 TPS (Bitcoin) / 15-30 TPS (Ethereum L1) |
| **Consommation énergie** | Ordre du watt | Térawatts (Bitcoin) / kilowatt (Ethereum) |
| **Cas d'usage cible** | Backend de jeu, banking interne, programmes de fidélité | Réserve de valeur, smart contracts publics |

PMS est **plus proche d'un système bancaire fermé que d'une crypto publique**.
Le DAG sert à rendre les actes du Coordinator **vérifiables a posteriori**, pas
à le remplacer par un consensus distribué.

---

## Pour l'utilisateur final (résumé pour CGV)

> Le réseau PMS est opéré par un **opérateur unique** (l'éditeur du jeu, ci-après
> « le Coordinator »). Le Coordinator détient l'autorité de créer des EDN, de
> distribuer les frais, de geler ou saisir des comptes en cas de fraude
> détectée, et d'inverser une transaction erronée. Tous ses actes sont
> enregistrés dans un journal cryptographique (le DAG) public et signé : un
> utilisateur peut donc à tout moment **vérifier** ce qui a été fait sur son
> compte, mais doit **faire confiance** au Coordinator pour ne pas en abuser.
>
> Si le Coordinator perd l'accès à ses serveurs, **les transactions cessent**
> jusqu'à restauration. Si le Coordinator perd sa clé secrète, **le réseau
> s'arrête définitivement** et les soldes en EDN ne peuvent plus être
> déplacés. L'éditeur s'engage à conserver des sauvegardes chiffrées
> quotidiennes ; il ne s'engage **pas** à offrir une garantie de disponibilité
> au niveau d'une banque traditionnelle.

---

## Roadmap pour réduire la dépendance

Ces évolutions sont **possibles mais non implémentées** au moment de v0.7.4 :

- **Rotation de la clé Coordinator** (item 8 de la sprint hardening) : permettrait
  de changer la clé sans redémarrer le réseau, en signant un bloc
  `CoordinatorKeyRotate` avec l'ancienne clé qui autorise la nouvelle.
- **HSM** (YubiHSM / AWS KMS) : la clé ne quitte jamais le matériel sécurisé,
  même le process serveur ne la voit pas en clair.
- **Secondaire en lecture** : un VPS qui se synchronise sur le DAG en mode
  read-only, pour permettre les requêtes balance / history même quand le
  primary est down.
- **Multi-Coordinator avec quorum** : plusieurs clés, signature M-of-N requise
  pour qu'un bloc soit valide. Réduit le risque de fuite (un seul leak ne
  donne plus l'autorité unique). Demande une vraie refonte du protocole de
  consensus.
- **Byzantine Fault Tolerance** (HotStuff, Tendermint…) : on devient une vraie
  blockchain. C'est un projet à part entière (6+ mois) et change la nature du
  produit.

---

## Liens

- [[server-engine]] — boot et chargement de la clé Coordinator.
- [[features/wallet-encryption]] — détails cryptographiques (signatures, X25519, AES-GCM).
- [[features/block-payloads]] — chaque type de payload, qui peut le forger.
- [[features/storage-rocksdb]] — checkpoints, recovery, layout.
- [[features/compliance]] — freeze/seize/reverse en détail.
- [[features/multi-ledger]] — ledgers custom et leur ownership.
