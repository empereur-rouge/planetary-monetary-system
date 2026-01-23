/**
 * API Avancée - Pour les développeurs nécessitant un contrôle fin sur le protocole PMS.
 * 
 * ⚠️ Ces APIs sont considérées comme avancées et peuvent changer sans préavis.
 * Pour la plupart des cas d'usage, préférez l'API standard via `@pms/sdk`.
 * 
 * @example
 * ```typescript
 * import { computeBlockId, WireBlock } from "@pms/sdk/advanced";
 * ```
 */

// ============================================================================
// Block Building Utilities
// ============================================================================

/**
 * Génère un ID de bloc à partir du contenu.
 * Utilisé pour construire manuellement des blocs avant soumission.
 */
export { computeBlockId, checkPowBits } from "./utils";


// ============================================================================
// Raw Cryptography
// ============================================================================

/**
 * Fonctions de chiffrement/déchiffrement pour les payloads.
 * Utilisées pour créer des transactions avec métadonnées chiffrées.
 */
export {
    encryptPayload,
    decryptPayload,
    generateX25519Keypair,
    deriveX25519PublicKey,
    formatCubeAttributesMessage,
    signCubeAttributes,
} from "./crypto";


// ============================================================================
// Low-Level Types
// ============================================================================

/**
 * Types pour la construction manuelle de blocs et transactions.
 */
export type {
    WireBlock,
    PayloadEnvelope,
    TxUtxo,
    OutputRef,
    TxOutput,
} from "./types";
