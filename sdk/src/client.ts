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
    Utxo,
    OutputRef,
    TxOutput,
    TxUtxo,
    PayloadEnvelope,
} from "./types";
import { DEFAULT_CONFIG, type BalanceInfo } from "./types";
import { PmsWallet } from "./wallet";
import { computeBlockId, encodeUtf8, parseAmount, formatAmount } from "./utils";

/**
 * Client pour interagir avec l'API REST d'un nœud PMS.
 */
export class PmsClient {
    private readonly config: Required<PmsClientConfig>;

    /**
     * Crée un nouveau client PMS.
     * @param config - Configuration du client
     */
    constructor(config: PmsClientConfig) {
        this.config = {
            ...DEFAULT_CONFIG,
            ...config,
        };
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Méthodes de lecture (GET)
    // ═══════════════════════════════════════════════════════════════════════

    /**
     * Récupère les tips actuels du DAG.
     */
    async getTips(): Promise<string[]> {
        const res = await this.fetch<{ tips: string[] }>("/v1/tips");
        return res.tips;
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
    async getBalance(address: string): Promise<string> {
        const utxos = await this.getUtxos(address);
        let total = 0n;
        for (const utxo of utxos) {
            total += parseAmount(utxo.amount);
        }
        return formatAmount(total);
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

    // ═══════════════════════════════════════════════════════════════════════
    // Méthodes d'écriture (POST)
    // ═══════════════════════════════════════════════════════════════════════

    /**
     * Soumet un bloc au réseau.
     */
    async submitBlock(wireBlock: WireBlock): Promise<SubmitResponse> {
        return this.fetch("/v1/submit", {
            method: "POST",
            body: JSON.stringify(wireBlock),
        });
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
    // Helper HTTP
    // ═══════════════════════════════════════════════════════════════════════

    private async fetch<T>(path: string, init?: RequestInit): Promise<T> {
        const url = `${this.config.nodeUrl}${path}`;

        const controller = new AbortController();
        const timeout = setTimeout(() => controller.abort(), this.config.timeout);

        try {
            const res = await fetch(url, {
                ...init,
                headers: {
                    "Content-Type": "application/json",
                    ...init?.headers,
                },
                signal: controller.signal,
            });

            if (!res.ok) {
                const text = await res.text();
                throw new Error(`HTTP ${res.status}: ${text}`);
            }

            return res.json() as Promise<T>;
        } finally {
            clearTimeout(timeout);
        }
    }
}
