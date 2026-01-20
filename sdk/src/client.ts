/**
 * PmsClient - Client HTTP pour interagir avec un nœud PMS.
 * 
 * @example
 * ```typescript
 * const client = new PmsClient({ nodeUrl: "https://node.pms.network" });
 * 
 * // Lecture
 * const tips = await client.getTips();
 * const balance = await client.getBalance("04abc...");
 * 
 * // Écriture
 * const result = await client.send({
 *   to: "04def...",
 *   amount: "10.0",
 *   wallet: myWallet,
 * });
 * ```
 */

import type {
    PmsClientConfig,
    Block,
    WireBlock,
    SupplyInfo,
    SubmitResponse,
    MintCubeResponse,
    BurnNftResponse,
    CubeAttributes,
    Utxo,
    OutputRef,
    TxOutput,
    TxUtxo,
    PayloadEnvelope,
    NodeListResponse,
    NftMetadata,
    CoordinatorInfoResponse,
} from "./types";
import { DEFAULT_CONFIG, type BalanceInfo } from "./types";
import { PmsWallet } from "./wallet";
import { computeBlockId, encodeUtf8, parseAmount, formatAmount, fromHex } from "./utils";


/**
 * Client pour interagir avec l'API REST d'un nœud PMS.
 */
export class PmsClient {
    private readonly config: Required<PmsClientConfig>;
    private knownNodes: Set<string> = new Set();
    private lastNodeRefresh = 0;
    private readonly NODE_REFRESH_INTERVAL = 60_000; // 1 min

    /**
     * Crée un nouveau client PMS.
     * @param config - Configuration du client
     * @param config.nodeUrl - URL du nœud principal
     * @param config.enableRacing - Activer le racing pattern
     * @param config.seedNodes - Liste des nœuds de seed
     */
    constructor(config: PmsClientConfig) {
        this.config = {
            ...DEFAULT_CONFIG,
            seedNodes: config.seedNodes ?? [],
            enableRacing: config.enableRacing ?? true,
            ...config,
        };

        // Initialize known nodes with seeds and main node
        this.addKnownNode(this.config.nodeUrl);
        this.config.seedNodes.forEach(url => this.addKnownNode(url));
    }

    private addKnownNode(url: string) {
        // Normalize URL: remove trailing slash
        const normalized = url.replace(/\/$/, "");
        if (normalized.startsWith("http")) {
            this.knownNodes.add(normalized);
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Méthodes de lecture (GET)
    // ═══════════════════════════════════════════════════════════════════════

    /**
     * Récupère les tips actuels du DAG.
     */
    async getTips(): Promise<string[]> {
        const res = await this.fetch<string[]>("/v1/dag/tips", {
            method: "POST",
            body: JSON.stringify({ limit: 10 })
        });
        return res;
    }

    /**
     * Récupère les informations publiques du coordinateur (clés).
     */
    async getCoordinatorInfo(): Promise<CoordinatorInfoResponse> {
        return this.fetch<CoordinatorInfoResponse>("/v1/coordinator/info");
    }

    /**
     * Récupère un bloc par son ID.
     */
    async getBlock(blockId: string): Promise<Block> {
        return this.fetch(`/v1/blocks/${blockId}`);
    }

    /**
     * Récupère les informations de supply.
     */
    async getSupply(): Promise<SupplyInfo> {
        return this.fetch("/v1/supply");
    }

    /**
     * Récupère les UTXOs d'une adresse.
     */
    async getUtxos(address: string): Promise<Utxo[]> {
        const res = await this.fetch<{ utxos: Utxo[] }>(`/v1/wallet/${address}/utxos`);
        return res.utxos ?? [];
    }

    /**
     * Récupère la balance d'une adresse.
     */
    /**
     * Récupère la balance d'une adresse.
     */
    async getBalance(address: string): Promise<string> {
        const res = await this.fetch<{ balance: string }>("/v1/balance", {
            method: "POST",
            body: JSON.stringify({ address }),
        });
        return res.balance;
    }

    /**
     * Récupère la balance complète avec les UTXOs.
     */
    async getBalanceInfo(address: string): Promise<BalanceInfo> {
        const utxos = await this.getUtxos(address);
        let total = 0n;
        for (const utxo of utxos) {
            total += parseAmount(utxo.amount);
        }
        return {
            address,
            balance: formatAmount(total),
            utxos,
        };
    }

    /**
     * Récupère la liste des NFTs appartenant à une adresse.
     * 
     * @param address - Adresse publique (hex) du propriétaire
     * @returns Liste des token_ids possédés par cette adresse
     * 
     * @example
     * ```typescript
     * const myNfts = await client.getNfts(myWallet.address);
     * console.log(`Vous possédez ${myNfts.length} NFT(s)`);
     * for (const tokenId of myNfts) {
     *     console.log(`- ${tokenId}`);
     * }
     * ```
     */
    async getNfts(address: string): Promise<string[]> {
        const res = await this.fetch<{ token_ids: string[] }>(`/v1/wallet/${address}/nfts`);
        return res.token_ids ?? [];
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Méthodes d'écriture (POST)
    // ═══════════════════════════════════════════════════════════════════════

    /**
     * Soumet un bloc au réseau.
     * Utilise le racing pattern si activé pour envoyer à plusieurs noeuds.
     */
    async submitBlock(wireBlock: WireBlock): Promise<SubmitResponse> {
        if (this.config.enableRacing) {
            return this.submitBlockRacing(wireBlock);
        }

        return this.fetch("/submit/block", {
            method: "POST",
            body: JSON.stringify(wireBlock),
        });
    }

    /**
     * Discovery & Racing Pattern:
     * 1. Refresh node list if stale
     * 2. Send to all known nodes in parallel
     * 3. Return first success
     */
    private async submitBlockRacing(wireBlock: WireBlock): Promise<SubmitResponse> {
        // 1. Refresh nodes (optimistic - don't block if fails)
        this.refreshNodeList().catch(err => console.debug("Node refresh failed:", err));

        // 2. Prepare targets (Main + Seeds + Discovered)
        // Shuffle to distribute load if we had too many (limit to top 5 for efficiency?)
        // For now, use all known nodes (assuming < 20)
        const targets = Array.from(this.knownNodes);

        if (targets.length === 0) {
            targets.push(this.config.nodeUrl.replace(/\/$/, ""));
        }

        // 3. Race!
        const body = JSON.stringify(wireBlock);
        const controller = new AbortController();

        const promises = targets.map(async (baseUrl) => {
            try {
                const res = await this.fetchUrl<SubmitResponse>(baseUrl, "/submit/block", {
                    method: "POST",
                    body,
                    signal: controller.signal
                });
                return res;
            } catch (err) {
                throw err;
            }
        });

        try {
            // First success wins
            const result = await Promise.any(promises);

            // Abort others to save bandwidth (optional, currently fetch wrapper creates its own controller)
            controller.abort();

            return result;
        } catch (error) {
            // If all failed - extract individual errors for debugging
            const aggregateError = error as AggregateError;
            const errors = aggregateError.errors || [];
            const errorMessages = errors.map((e: Error) => e.message || String(e)).join("; ");
            throw new Error(`Submit failed on all ${targets.length} nodes. Errors: ${errorMessages}`);
        }
    }

    /**
     * Rafraîchit la liste des noeuds connus depuis le registre
     */
    private async refreshNodeList() {
        if (Date.now() - this.lastNodeRefresh < this.NODE_REFRESH_INTERVAL) {
            return;
        }

        try {
            const res = await this.fetch<NodeListResponse>("/v1/nodes");
            res.nodes.forEach(node => {
                if (node.api_url) this.addKnownNode(node.api_url);
            });
            this.lastNodeRefresh = Date.now();
        } catch (e) {
            // Should verify this doesn't crash the app, just log debug
            // console.debug("Failed to refresh node list (ignoring)", e);
        }
    }

    /**
     * Envoie des tokens à une adresse.
     * Construit automatiquement la transaction, la signe et la soumet.
     */
    async send(params: {
        to: string;
        amount: string;
        wallet: PmsWallet;
        memo?: string;
    }): Promise<SubmitResponse> {
        const { to, amount, wallet, memo } = params;

        // 1. Récupérer les UTXOs du wallet
        const utxos = await this.getUtxos(wallet.address);
        if (utxos.length === 0) {
            throw new Error("No UTXOs available");
        }

        // 2. Sélectionner les inputs
        const amountSats = parseAmount(amount);
        const feeRate = 100n; // 1% minimum
        const fee = (amountSats * feeRate) / 10000n;
        const totalNeeded = amountSats + fee;

        let selectedSats = 0n;
        const inputs: OutputRef[] = [];

        for (const utxo of utxos) {
            inputs.push(utxo.outpoint);
            selectedSats += parseAmount(utxo.amount);
            if (selectedSats >= totalNeeded) break;
        }

        if (selectedSats < totalNeeded) {
            throw new Error(
                `Insufficient balance: have ${formatAmount(selectedSats)}, need ${formatAmount(totalNeeded)}`
            );
        }

        // 3. Construire les outputs
        const outputs: TxOutput[] = [
            { address: to, amount: formatAmount(amountSats) },
        ];

        // Change
        const change = selectedSats - amountSats - fee;
        if (change > 0n) {
            outputs.push({ address: wallet.address, amount: formatAmount(change) });
        }

        // 4. Construire la transaction
        const tx: TxUtxo = {
            inputs,
            outputs,
            fee: formatAmount(fee),
            data: memo,
        };

        // 5. Créer le bloc
        const tips = await this.getTips();
        const parents = tips.slice(0, 2); // Max 2 parents

        const payload: PayloadEnvelope = { Plain: { TxUtxo: tx } };
        const payloadJson = JSON.stringify(payload);

        // 6. Trouver un nonce valide (PoW léger)
        let nonce = 0;
        let blockId = computeBlockId(parents, payloadJson, nonce);
        // Pour l'instant, pas de PoW requis côté client
        // En production, boucler jusqu'à avoir les bits requis

        // 7. Signer le bloc
        const messageToSign = encodeUtf8(blockId);
        const signature = await wallet.sign(messageToSign);

        // 8. Construire le WireBlock
        const wireBlock: WireBlock = {
            id: blockId,
            parents,
            payload_json: payloadJson,
            nonce,
            network_id: this.config.networkId,
            protocol_version: this.config.protocolVersion,
            signer_pk_hex: wallet.publicKeyHex,
            signature_hex: signature,
        };

        // 9. Soumettre
        return this.submitBlock(wireBlock);
    }





    // ═══════════════════════════════════════════════════════════════════════
    // Méthodes NFT Cube (Burn et Mint spécialisé)
    // ═══════════════════════════════════════════════════════════════════════

    /**
     * Brûle (détruit) un NFT existant.
     * 
     * Seul le propriétaire du NFT peut le brûler.
     * Une fois brûlé, le NFT est supprimé définitivement.
     * 
     * Pour les Cubes authentiques (avec signature Authority valide), 
     * un remboursement est calculé selon la formule:
     * `(weight * size * density) / 10000` PMS
     * 
     * @param params - Paramètres du burn
     * @param params.tokenId - Identifiant du NFT à brûler
     * @param params.wallet - Wallet PMS du propriétaire (doit être l'owner actuel)
     * @returns BurnNftResponse avec refund preview si cube authentique
     * 
     * @example
     * ```typescript
     * const result = await client.burnNft({
     *     tokenId: "abc123def456...",
     *     wallet: myWallet,
     * });
     * 
     * if (result.refund) {
     *     console.log(`Remboursement: ${result.refund.amount} PMS`);
     * }
     * ```
     */
    async burnNft(params: {
        tokenId: string;
        wallet: PmsWallet;
    }): Promise<BurnNftResponse> {
        const { tokenId, wallet } = params;

        // 1. Récupérer les tips du DAG (parents du nouveau bloc)
        const tips = await this.getTips();
        const parents = tips.slice(0, 2);

        // 2. Construire le payload NFT Burn
        //    Format: { Plain: { Nft: { Burn: { token_id, burner } } } }
        //    Le "burner" est l'adresse du wallet signataire (doit être le propriétaire)
        const payload: PayloadEnvelope = {
            Plain: {
                Nft: {
                    Burn: {
                        token_id: tokenId,
                        burner: wallet.address,
                    }
                }
            }
        };
        const payloadJson = JSON.stringify(payload);

        // 3. Calculer le block ID
        const nonce = 0;
        const blockId = computeBlockId(parents, payloadJson, nonce);

        // 4. Construire le message canonique à signer (comme dag-pms canonical_wireblock_message)
        //    IMPORTANT: L'ordre des champs doit être exactement le même que dans Rust
        //    Le message est un JSON du WireBlock SANS la signature
        const canonicalView = {
            id: blockId,
            parents: parents,
            payload_json: payloadJson,
            nonce: nonce,
            network_id: this.config.networkId,
            protocol_version: this.config.protocolVersion,
            signer_pk_hex: wallet.publicKeyHex,
        };
        const messageToSign = JSON.stringify(canonicalView);

        // 5. Signer le message canonique
        const signatureHex = wallet.sign(encodeUtf8(messageToSign));

        // 6. Convertir signature hex -> base64 (dag-pms attend du base64)
        const signatureBytes = fromHex(signatureHex);
        const signatureB64 = btoa(String.fromCharCode(...signatureBytes));

        // 7. Construire le request body avec signature en base64
        const burnRequest = {
            id: blockId,
            parents,
            payload_json: payloadJson,
            nonce,
            network_id: this.config.networkId,
            protocol_version: this.config.protocolVersion,
            signer_pk_hex: wallet.publicKeyHex,
            signature_hex: signatureB64,  // base64 malgré le nom "hex"
        };

        // 8. Soumettre à /v1/nft/burn (endpoint spécialisé avec refund preview)
        return this.fetch<BurnNftResponse>("/v1/nft/burn", {
            method: "POST",
            body: JSON.stringify(burnRequest),
        });
    }

    /**
     * Mint un NFT via le Coordinateur (Server-Side Signing).
     * Le client génère l'ID et les métadonnées, mais c'est le serveur qui signe et chiffre.
     */
    async mintNft(params: {
        wallet: PmsWallet;
        metadata: NftMetadata;
        tokenId?: string;
    }): Promise<SubmitResponse & { token_id: string }> {
        const { wallet, metadata } = params;
        const tokenId = params.tokenId || this.generateRandomHex(32);

        // Appel au endpoint générique du serveur
        const response = await this.fetch<SubmitResponse>("/v1/nft/mint", {
            method: "POST",
            body: JSON.stringify({
                token_id: tokenId,
                owner_address: wallet.address,
                owner_x25519_pubkey: wallet.x25519PublicKeyHex,
                metadata: metadata,
            }),
        });

        return { ...response, token_id: tokenId };
    }

    /**
     * Mint un Cube avec des attributs générés et signés par le backend Authority.
     * @param params.wallet - Wallet PMS du propriétaire
     * @param params.generatorUrl - URL du backend générateur de cubes (ex: "http://localhost:3000")
     */
    async mintCube(params: {
        wallet: PmsWallet;
        generatorUrl: string;
    }): Promise<MintCubeResponse> {
        const { wallet, generatorUrl } = params;

        // 1. Fetch cube attributes and signature from backend
        const cubeResponse = await fetch(`${generatorUrl}/api/cube/generate`, {
            method: "POST",
        });
        if (!cubeResponse.ok) {
            const text = await cubeResponse.text();
            throw new Error(`Cube generation failed: ${cubeResponse.status} - ${text}`);
        }
        const cubeData = await cubeResponse.json() as {
            rarity: string;
            attributes: CubeAttributes;
            roll: number;
            signature: string;
        };

        // 2. Générer le tokenId
        const tokenId = this.generateRandomHex(32);

        // 3. Construire les métadonnées avec la signature
        const metadata: NftMetadata = {
            name: `Cube ${cubeData.rarity}`,
            description: `A ${cubeData.rarity} cube with unique properties.`,
            nft_type: "cube",
            extra: JSON.stringify({
                rarity: cubeData.rarity,
                attributes: cubeData.attributes,
                roll: cubeData.roll,
                signature: cubeData.signature, // Authority signature
            }),
        };

        // 4. Appel générique pour mint
        const submitResult = await this.mintNft({
            wallet,
            metadata,
            tokenId,
        });

        return {
            ...submitResult,
            rarity: cubeData.rarity as MintCubeResponse["rarity"],
            roll: cubeData.roll,
            attributes: cubeData.attributes,
        };
    }

    /**
     * Génère une chaîne hexadécimale aléatoire de la longueur spécifiée (en bytes).
     * Utilise crypto.getRandomValues pour la sécurité cryptographique.
     */
    private generateRandomHex(bytes: number): string {
        const array = new Uint8Array(bytes);
        crypto.getRandomValues(array);
        return Array.from(array).map(b => b.toString(16).padStart(2, '0')).join('');
    }


    // ═══════════════════════════════════════════════════════════════════════
    // Helper HTTP
    // ═══════════════════════════════════════════════════════════════════════

    private async fetch<T>(path: string, init?: RequestInit): Promise<T> {
        // Use primary node URL by default
        return this.fetchUrl(this.config.nodeUrl, path, init);
    }

    private async fetchUrl<T>(baseUrl: string, path: string, init?: RequestInit): Promise<T> {
        // Ensure no double slash if baseUrl ends with / and path starts with /
        const url = `${baseUrl.replace(/\/$/, "")}/${path.replace(/^\//, "")}`;

        const controller = new AbortController();
        const timeout = setTimeout(() => controller.abort(), this.config.timeout);

        // Merge signals if init provied one
        const signal = init?.signal || controller.signal;

        try {
            const res = await fetch(url, {
                ...init,
                headers: {
                    "Content-Type": "application/json",
                    ...init?.headers,
                },
                signal,
            });

            if (!res.ok) {
                const text = await res.text();
                throw new Error(`HTTP ${res.status} (${url}): ${text}`);
            }

            return res.json() as Promise<T>;
        } finally {
            clearTimeout(timeout);
        }
    }
}
