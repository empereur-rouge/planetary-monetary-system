# Cube API

> Endpoints pour le systeme CUBE (claim et burn de tokens CUBE)

Le systeme CUBE permet de simuler un mecanisme de mining : les utilisateurs claim des tokens CUBE, puis les burn pour recevoir du PMS natif au taux de **10 CUBE = 1 PMS**.

Le token CUBE est auto-enregistre dans le registre au premier appel de `/v1/cube/claim` (asset_id: `cube`, 2 decimales).

---

## POST `/v1/cube/claim`

Mint 1000 CUBE vers une adresse. Simule une recompense de mining.

### Request Body

```json
{
  "to": "pms1recipient..."
}
```

| Champ | Type | Requis | Description |
|-------|------|--------|-------------|
| `to` | string | oui | Adresse Bech32 du destinataire |

### Response (Succes - 201)

```json
{
  "block_id": "claim_abc123...",
  "amount": "1000.00",
  "asset_id": "cube"
}
```

### Erreurs

| HTTP | Description |
|------|-------------|
| 503 | DAG non initialise (pas de parents disponibles) |

### Exemple

```bash
curl -k -X POST https://localhost:8443/v1/cube/claim \
  -H "Content-Type: application/json" \
  -d '{"to": "pms1qw508d6qejxtdg4y5r3zarvary0c5xw7k..."}'
```

---

## POST `/v1/cube/burn`

Brule des tokens CUBE pour recevoir du PMS natif. Le taux de conversion est fixe : **10 CUBE = 1 PMS**.

Le systeme effectue automatiquement :
1. Selection des UTXOs CUBE (largest-first)
2. Burn des CUBEs selectionnes
3. Mint du PMS equivalent
4. Retour du change CUBE si necessaire

### Request Body

```json
{
  "private_key_b64": "base64_encoded_private_key...",
  "amount": "500.00"
}
```

| Champ | Type | Requis | Description |
|-------|------|--------|-------------|
| `private_key_b64` | string | oui | Cle privee en base64 (preuve de possession) |
| `amount` | string | oui | Quantite de CUBEs a bruler |

### Response (Succes - 201)

```json
{
  "block_id": "burn_def456...",
  "cubes_burned": "500.00",
  "pms_received": "50.00000000"
}
```

### Erreurs

| HTTP | Description |
|------|-------------|
| 400 | Cle privee invalide, montant <= 0, ou montant trop petit |
| 422 | Pas de CUBE UTXOs ou solde CUBE insuffisant |

### Response (Solde insuffisant - 422)

```json
{
  "error": "insufficient CUBE balance",
  "available": "300.00",
  "required": "500.00"
}
```

### Exemple

```bash
curl -k -X POST https://localhost:8443/v1/cube/burn \
  -H "Content-Type: application/json" \
  -d '{
    "private_key_b64": "MHQCAQEEIFm0...",
    "amount": "500.00"
  }'
```

---

## Mecanisme de conversion

| Source | Destination | Taux |
|--------|-------------|------|
| 10 CUBE | 1 PMS | Fixe |
| 1000 CUBE (claim) | 100 PMS (apres burn) | - |

Le change CUBE est retourne sous forme d'un UTXO supplementaire. Exemple : burn 550 CUBE a partir de 2 UTXOs de 300 CUBE chacun :
- 550 CUBE brules -> 55 PMS mintes
- 50 CUBE de change retournes
