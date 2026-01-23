# 🎨 NFT API - Privacy & Encryption

> Endpoints pour la gestion des NFTs (Non-Fungible Tokens) avec confidentialité.

## 🔒 Concepts Clés : Privacy & Re-encryption

Les NFTs dans PMS utilisent un modèle **Privacy-First** :

1.  **Métadonnées chiffrées** : Les métadonnées (nom, attributs, rareté) ne sont **jamais** stockées en clair.
2.  **Stockage DAG** : Les données chiffrées résident uniquement dans le bloc source (Mint) ou le bloc de transfert (Transfer).
3.  **Déchiffrement** : Seuls le **propriétaire actuel** et le **Coordinateur** peuvent déchiffrer les données.
4.  **Transfert avec Re-chiffrement** : Lors d'un transfert, les métadonnées sont déchiffrées puis **re-chiffrées** pour le nouveau propriétaire.

---

## POST `/v1/nft/transfer/prepare` (Nouveau)

Prépare une transaction de transfert en effectuant le re-chiffrement des métadonnées côté Coordinateur.

> **Pourquoi cet endpoint ?**
> Le client ne peut pas chiffrer directement pour le nouveau propriétaire sans exposer ses clés ou complexifier le flux. Le Coordinateur (qui a accès aux données chiffrées) agit comme tiers de confiance pour effectuer cette opération atomique.

### Request Body

```json
{
  "token_id": "cube_abc123",
  "from_address": "pms1currentowner...",
  "to_address": "pms1newowner...",
  "new_owner_x25519_pubkey": "abc123x25519key..."
}
```

| Champ | Type | Description |
|-------|------|-------------|
| `token_id` | string | ID du NFT à transférer |
| `from_address` | string | Adresse de l'expéditeur |
| `to_address` | string | Adresse du destinataire |
| `new_owner_x25519_pubkey` | string | Clé publique de chiffrement du destinataire |

### Response

Retourne une action `NftAction::Transfer` prête à être signée, contenant le blob `encrypted_metadata` mis à jour.

```json
{
  "action": {
    "Transfer": {
      "token_id": "cube_abc123",
      "from": "pms1currentowner...",
      "to": "pms1newowner...",
      "new_owner_x25519_pubkey": "...",
      "encrypted_metadata": "{\"scheme\":\"x25519+aes256gcm\",\"ciphertext_b64\":\"...\"}"
    }
  }
}
```

---

## GET `/v1/nft/{token_id}`

Récupère les informations publiques d'un NFT. **Ne retourne PAS les métadonnées en clair.**

### Response

```json
{
  "token_id": "cube_abc123",
  "exists": true,
  "owner": "pms1owner...",
  "mint_block_id": "block_xyz..." 
}
```

> **Note**: `mint_block_id` pointe vers le bloc contenant les métadonnées chiffrées les plus récentes (bloc de Mint original ou dernier bloc de Transfer).

---

## GET `/v1/wallet/{address}/nfts`

Récupère la liste des token IDs possédés. Le client doit ensuite récupérer chaque NFT individuellement pour déchiffrer les métadonnées.

### Response

```json
{
  "owner": "pms1abc123...",
  "token_ids": [
    "cube_001",
    "cube_002"
  ],
  "count": 2
}
```

---

## Workflow Client : Affichage des NFTs

Pour afficher les NFTs d'un utilisateur (ex: Cubes avec rareté) :

1.  Appeler `GET /v1/wallet/{address}/nfts` -> Reçoit liste d'IDs.
2.  Pour chaque ID :
    *   Appeler `GET /v1/nft/{id}` -> Obtient `mint_block_id`.
    *   Appeler `GET /v1/blocks/{mint_block_id}` -> Obtient le bloc brut.
    *   Extraire le payload chiffré (`EncryptedPayload`).
    *   Déchiffrer localement avec la clé privée X25519 du wallet.

```typescript
// Exemple SDK (conceptuel)
const myNfts = await client.getNfts(myAddress);
for (const id of myNfts) {
    const nft = await client.getNft(id);
    const block = await client.getBlock(nft.mint_block_id);
    const metadata = client.decrypt(block.payload, myPrivateKey);
    console.log(`Cube ${id}: ${metadata.rarity}`);
}
```

---

## POST `/v1/nft/mint` & `/v1/nft/burn`

(Ces endpoints restent inchangés dans leur signature externe, mais le backend traite maintenant les données de manière chiffrée).

- **Mint** : Les métadonnées envoyées sont chiffrées par le Coordinateur avant d'être incluses dans le bloc.
- **Burn** : Le Coordinateur déchiffre les métadonnées à la volée pour vérifier les attributs et calculer le remboursement.
