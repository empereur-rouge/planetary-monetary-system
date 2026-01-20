/**
 * @pms/sdk - SDK TypeScript pour le réseau PMS.
 * 
 * @example
 * ```typescript
 * import { PmsWallet, PmsClient } from "@pms/sdk";
 * 
 * // Créer un wallet
 * const wallet = PmsWallet.generate();
 * console.log(wallet.mnemonic); // 24 mots
 * 
 * // Connecter au réseau
 * const client = new PmsClient({ nodeUrl: "https://node.pms.network" });
 * 
 * // Consulter la balance
 * const balance = await client.getBalance(wallet.address);
 * 
 * // Envoyer des tokens
 * await client.send({
 *   to: "04abc...",
 *   amount: "10.0",
 *   wallet,
 * });
 * ```
 */

// Wallet
export { PmsWallet, isValidMnemonic } from "./wallet";

// Client
export { PmsClient } from "./client";

// Types
export * from "./types";

// Utils
export {
    computeBlockId,
    checkPowBits,
    parseAmount,
    formatAmount,
    toHex,
    fromHex,
} from "./utils";

// Crypto (encryption)
export {
    encryptPayload,
    decryptPayload,

    generateX25519Keypair,
    deriveX25519PublicKey,

    // Authority signing (for burn-to-mint)
    formatCubeAttributesMessage,
    signCubeAttributes,
} from "./crypto";
