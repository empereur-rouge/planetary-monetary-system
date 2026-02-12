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
 * 
 * Pour les fonctionnalités avancées (construction manuelle de blocs, chiffrement),
 * voir `@pms/sdk/advanced`.
 */

// ============================================================================
// Wallet
// ============================================================================
export { PmsWallet, isValidMnemonic } from "./wallet";

// ============================================================================
// Client
// ============================================================================
export { PmsClient } from "./client";

// ============================================================================
// Types (high-level only)
// ============================================================================
export type {
    // Client config & responses
    PmsClientConfig,
    SubmitResponse,
    BalanceInfo,
    SupplyInfo,
    CoordinatorInfoResponse,

    // NFT high-level
    NftMetadata,
    NftResponse,
    MintCubeResponse,
    BurnNftResponse,
    CubeAttributes,

    // Block & Transaction (read-only)
    Block,
    Utxo,

    // History
    WalletHistoryResp,
    HistoryItem,
    RuntimeConfig,

    // Transaction Preparation
    PrepareTxRequest,
    PrepareTxResponse,
    UtxoDetail,

    // Token Registry
    TokenMetadata,
} from "./types";

// ============================================================================
// Utils (simple, user-friendly)
// ============================================================================
export {
    parseAmount,
    formatAmount,
    toHex,
    fromHex,
} from "./utils";

// ============================================================================
// Crypto
// ============================================================================
export { decryptPayload } from "./crypto";
