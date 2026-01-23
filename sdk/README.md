# @pms/sdk

SDK TypeScript officiel pour interagir avec le réseau **PMS** (Planetary Monetary System).

## Installation

```bash
npm install @pms/sdk
# ou
yarn add @pms/sdk
# ou  
pnpm add @pms/sdk
```

## Structure de l'API

Le SDK est organisé en deux niveaux :

| Import | Usage | Contenu |
|--------|-------|--------|
| `@pms/sdk` | 99% des cas | `PmsWallet`, `PmsClient`, utils simples |
| `@pms/sdk/src/advanced` | Power users | `computeBlockId`, crypto, types bas niveau |

## Démarrage Rapide

```typescript
import { PmsWallet, PmsClient } from "@pms/sdk";

// 1. Créer un wallet
const wallet = PmsWallet.generate();
console.log("Mnemonic:", wallet.mnemonic);
console.log("Address:", wallet.address);

// 2. Connecter au réseau
const client = new PmsClient({ 
  nodeUrl: "https://node.pms.network" 
});

// 3. Consulter la balance
const balance = await client.getBalance(wallet.address);
console.log("Balance:", balance);

// 4. Envoyer des tokens
const result = await client.send({
  to: "04abc123...",
  amount: "10.0",
  wallet,
});
console.log("Transaction:", result.block_id);
```

---

## API Reference

### `PmsWallet` - Gestion des Wallets

Le wallet gère les clés cryptographiques (secp256k1) et permet de signer des transactions.

#### Création de Wallet

```typescript
// Générer un nouveau wallet (24 mots)
const wallet = PmsWallet.generate();

// Restaurer depuis un mnemonic
const restored = PmsWallet.fromMnemonic("word1 word2 ... word24");

// Importer depuis une clé privée (hex)
const imported = PmsWallet.fromPrivateKey("abc123...");

// Créer depuis une seed (32 bytes)
const seeded = PmsWallet.fromSeed(new Uint8Array(32));
```

#### Propriétés

| Propriété | Type | Description |
|-----------|------|-------------|
| `address` | `string` | Adresse publique (clé publique hex non compressée, commence par `04`) |
| `publicKeyHex` | `string` | Clé publique en hexadécimal (identique à `address`) |
| `x25519PublicKeyHex` | `string` | Clé publique X25519 pour le chiffrement |
| `mnemonic` | `string \| undefined` | Phrase mnémonique (24 mots) si disponible |

#### Méthodes

```typescript
// Signer un message
const signature: string = wallet.sign(messageBytes);

// Exporter la clé privée (hex)
const privateKey: string = wallet.exportPrivateKey();

// Vérifier une signature (statique)
const isValid: boolean = PmsWallet.verify(message, signature, publicKeyHex);
```

#### Validation de Mnemonic

```typescript
import { isValidMnemonic } from "@pms/sdk";

if (isValidMnemonic(userInput)) {
  const wallet = PmsWallet.fromMnemonic(userInput);
}
```

---

### `PmsClient` - Client API

Le client permet d'interagir avec l'API REST des nœuds PMS.

#### Configuration

```typescript
const client = new PmsClient({
  nodeUrl: "https://node.pms.network",  // URL du nœud principal
  seedNodes: [                           // Optionnel: nœuds de secours
    "https://node2.pms.network",
    "https://node3.pms.network",
  ],
  networkId: "pms-mainnet",              // Défaut: "pms-mainnet"
  protocolVersion: 1,                    // Défaut: 1
  timeout: 30000,                        // Timeout en ms (défaut: 30s)
  enableRacing: true,                    // Racing pattern (défaut: true)
});
```

#### Méthodes de Lecture

```typescript
// Récupérer les tips du DAG (blocs les plus récents)
const tips: string[] = await client.getTips();
// Réponse: ["4aa21b8f570005b4088c53149c9afedc528d7a47127e915744a1811c11d5956c", "..."]
```

```typescript
// Récupérer un bloc par son ID
const block: Block = await client.getBlock(blockId);
// Réponse:
// {
//   id: "4aa21b8f570005b4088c53149c9afedc528d7a47127e915744a1811c11d5956c",
//   parents: ["abc123...", "def456..."],
//   payload: { Plain: { TxUtxo: { ... } } },
//   nonce: 12345
// }
```

```typescript
// Récupérer le supply total
const supply: SupplyInfo = await client.getSupply();
// Réponse:
// {
//   circulating: "1000000.00000000",
//   utxo_count: 42567
// }
```

```typescript
// Récupérer les UTXOs d'une adresse
const utxos: Utxo[] = await client.getUtxos(address);
// Réponse:
// [
//   {
//     address: "04abc123...",
//     amount: "50.00000000",
//     outpoint: { txid: "tx123...", index: 0 }
//   },
//   {
//     address: "04abc123...",
//     amount: "25.50000000",
//     outpoint: { txid: "tx456...", index: 1 }
//   }
// ]
```

```typescript
// Récupérer la balance d'une adresse
const balance: string = await client.getBalance(address);
// Réponse: "75.50000000"
```

```typescript
// Récupérer balance + UTXOs détaillés
const info: BalanceInfo = await client.getBalanceInfo(address);
// Réponse:
// {
//   address: "04abc123...",
//   balance: "75.50000000",
//   utxos: [{ address: "...", amount: "...", outpoint: {...} }, ...]
// }
```

```typescript
// Récupérer l'historique (Transactions, Mints, Rewards)
const history = await client.getHistory(address, { limit: 50 });
// Réponse:
// {
//   address: "04abc123...",
//   count: 50,
//   items: [
//     { block_id: "...", ts_ms: 1700000000000, payload_type: "Reward", payload: { ... } },
//     { block_id: "...", ts_ms: 1700000050000, payload_type: "TxUtxo", payload: { ... } }
//   ]
// }

// Récupérer l'historique avec déchiffrement automatique des rewards
// (Nécessite le wallet pour déchiffrer les EncryptedRewards)
const historyWithDecrypt = await client.getHistory(address, {
    limit: 50,
    decryptionWallet: myWallet 
});
// Les items "EncryptedReward" déchiffrés apparaîtront comme des "Reward" standards
```

#### Méthodes d'Écriture (Transactions)

```typescript
// Envoyer des tokens
const result = await client.send({
  to: "04destinataire...",
  amount: "100.0",
  wallet: myWallet,
  memo: "Paiement optionnel", // Optionnel
});
// Réponse:
// {
//   status: "inserted",  // ou "already_exists"
//   block_id: "7f3a2b1c..."
// }
```

---

### NFT - Non-Fungible Tokens

Le SDK supporte les opérations NFT natives du réseau PMS.

#### Mint NFT (Standard)

La création de NFT est **sécurisée** par le Coordinateur.

```typescript
// Le server (Coordinateur) signe et chiffre le NFT pour vous.
const result = await client.mintNft({
  tokenId: "unique-nft-001",
  metadata: {
    name: "Mon NFT",
    description: "Description du NFT",
    uri: "ipfs://QmXxx...",
    nft_type: "art",
  },
  wallet: myWallet, // Votre wallet personnel
});
// Réponse:
// {
//   status: "inserted",
//   token_id: "unique-nft-001",
//   block_id: "a1b2c3d4e5f6..."
// }
```

> [!NOTE]
> Les métadonnées sont automatiquement **chiffrées** par le serveur pour le propriétaire et le Coordinateur.

#### Mint Cube (NFT avec Rareté)

Les Cubes sont des NFTs spéciaux avec rareté et attributs générés par un backend Authority.

```typescript
// Mint un cube pour un utilisateur
const result = await client.mintCube({
  wallet: myWallet,
  generatorUrl: "https://cube-generator.example.com", // Backend Authority
});
// Réponse (MintCubeResponse):
// {
//   status: "inserted",
//   block_id: "9f8e7d6c5b4a...",
//   token_id: "a1b2c3d4e5f6...64chars...",
//   rarity: "Legendary",
//   roll: 75,
//   attributes: {
//     weight: 50.5,
//     size: 30.2,
//     density: 2.5
//   }
// }
```

**Raretés possibles :**

| Rareté | Probabilité | Cubes sur 10M |
|--------|-------------|---------------|
| Unique | 1/1,000,000 | 10 |
| Legendary | 1/100,000 | 100 |
| Rare | 1/10,000 | 1,000 |
| Uncommon | 1/1,000 | 10,000 |
| Common | 1/100 | 100,000 |
| Basic | ~99% | ~9,888,890 |

#### Burn NFT

```typescript
// Détruit un NFT (seul le propriétaire peut brûler)
const result = await client.burnNft({
  tokenId: "nft-to-destroy",
  wallet: ownerWallet,
});
// Réponse:
// {
//   status: "inserted",
//   block_id: "1a2b3c4d5e6f...",
//   refund: {                       // Uniquement pour les Cubes authentiques
//     amount: "1.23456789",
//     recipient: "04abc123..."
//   }
// }
```

> [!TIP]
> Les Cubes avec une signature Authority valide génèrent un **remboursement automatique** calculé selon leurs attributs.

#### Batch Burn (Destruction Multiple)

Pour détruire plusieurs NFTs en une seule transaction (économie de frais) :

```typescript
const result = await client.burnNfts({
  tokenIds: ["token-1", "token-2", "token-3"],
  wallet: ownerWallet,
});
// Réponse:
// {
//   status: "burned",
//   token_id: "batch",
//   token_ids: ["token-1", "token-2", "token-3"],
//   refund: { ... } // Remboursement cumulé
// }
```

---

### API Avancée

Pour les cas d'usage avancés (construction manuelle de blocs, chiffrement custom), importez depuis le module `advanced` :

```typescript
import { 
  computeBlockId, 
  checkPowBits,
  encryptPayload, 
  decryptPayload, 
  generateX25519Keypair,
  deriveX25519PublicKey,
} from "@pms/sdk/src/advanced";

import type {
  WireBlock,
  PayloadEnvelope,
  TxUtxo,
} from "@pms/sdk/src/advanced";
```

> [!WARNING]
> L'API avancée peut changer sans préavis. Préférez l'API publique pour la stabilité.

---

### Utilitaires (API Publique)

```typescript
import { 
  parseAmount, 
  formatAmount,
  toHex,
  fromHex,
} from "@pms/sdk";

// Convertir les montants
const sats = parseAmount("10.5");      // -> 1050000000n
const formatted = formatAmount(sats);   // -> "10.50000000"

// Conversions hex
const hex = toHex(new Uint8Array([1, 2, 3]));
const bytes = fromHex("010203");
```

---

## Types TypeScript

Tous les types sont exportés et documentés :

```typescript
import type {
  // Configuration
  PmsClientConfig,
  
  // Réponses API
  SubmitResponse,
  BalanceInfo,
  SupplyInfo,
  
  // NFT
  NftMetadata,
  MintCubeResponse,
  BurnNftResponse,
  CubeAttributes,
  
  // Blocs & Transactions (lecture)
  Block,
  Utxo,
  
  // Historique
  WalletHistoryResp,
  HistoryItem,
} from "@pms/sdk";

// Types bas niveau (API avancée)
import type {
  WireBlock,
  PayloadEnvelope,
  TxUtxo,
  OutputRef,
  TxOutput,
} from "@pms/sdk/src/advanced";
```

---

## Sécurité

### Coordinateur et Minting

En production, seul le **Coordinateur** peut créer de nouveaux NFTs (y compris les Cubes). Cette restriction est appliquée au niveau du backend via la clé `coordinator_pk`.

Les NFTs authentiques sont identifiés par le `creator` qui correspond à la clé publique du Coordinateur.

### Chiffrement

Le SDK utilise :
- **secp256k1** pour les signatures (ECDSA, DER-encoded)
- **X25519** pour l'échange de clés
- **AES-256-GCM** pour le chiffrement symétrique

### Bonnes Pratiques

```typescript
// ✅ Stocker le mnemonic de façon sécurisée
const wallet = PmsWallet.generate();
// Sauvegarder wallet.mnemonic dans un stockage sécurisé

// ✅ Ne jamais exposer la clé privée
const privateKey = wallet.exportPrivateKey();
// Ne pas logger ou transmettre cette valeur

// ✅ Vérifier les signatures avant d'accepter des données
const isValid = PmsWallet.verify(message, signature, senderPubKey);
```

---

## Racing Pattern

Le SDK utilise un "racing pattern" pour la soumission des transactions :

1. Le client maintient une liste de nœuds connus
2. Lors de `submitBlock()`, la transaction est envoyée à **tous** les nœuds en parallèle
3. La première réponse positive est retournée
4. Les autres requêtes sont annulées

Cela améliore :
- La **latence** (premier nœud qui répond)
- La **fiabilité** (tolérance aux pannes)
- La **propagation** (le bloc atteint plusieurs nœuds rapidement)

```typescript
// Désactiver si non souhaité
const client = new PmsClient({
  nodeUrl: "...",
  enableRacing: false,
});
```

---

## Tests

```bash
# Lancer les tests
npm test

# Mode watch
npm run test:watch

# Couverture
npm run test:coverage
```

---

## Licence

MIT © PMS Team
