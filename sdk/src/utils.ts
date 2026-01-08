/**
 * Utilitaires cryptographiques pour le SDK PMS.
 */

import { sha256 } from "@noble/hashes/sha2";
import { bytesToHex, hexToBytes } from "@noble/hashes/utils";

/**
 * Calcule le SHA256 d'un message.
 */
export function sha256Hash(data: Uint8Array): Uint8Array {
    return sha256(data);
}

/**
 * Convertit des bytes en hex.
 */
export function toHex(bytes: Uint8Array): string {
    return bytesToHex(bytes);
}

/**
 * Convertit un hex en bytes.
 */
export function fromHex(hex: string): Uint8Array {
    return hexToBytes(hex);
}

/**
 * Encode une string en UTF-8.
 */
export function encodeUtf8(str: string): Uint8Array {
    return new TextEncoder().encode(str);
}

/**
 * Décode des bytes UTF-8 en string.
 */
export function decodeUtf8(bytes: Uint8Array): string {
    return new TextDecoder().decode(bytes);
}

/**
 * Génère un ID de bloc à partir du contenu.
 * Format: SHA256(parents + payload + nonce)
 */
export function computeBlockId(
    parents: string[],
    payloadJson: string | undefined,
    nonce: number
): string {
    const parentsStr = parents.sort().join(",");
    const payloadStr = payloadJson ?? "";
    const content = `${parentsStr}|${payloadStr}|${nonce}`;
    const hash = sha256Hash(encodeUtf8(content));
    return toHex(hash);
}

/**
 * Vérifie si un ID a le nombre requis de leading zero bits (PoW).
 */
export function checkPowBits(blockId: string, requiredBits: number): boolean {
    if (requiredBits === 0) return true;

    const bytes = fromHex(blockId);
    let zeroBits = 0;

    for (const byte of bytes) {
        if (byte === 0) {
            zeroBits += 8;
        } else {
            // Compte les leading zeros dans ce byte
            let mask = 0x80;
            while (mask > 0 && (byte & mask) === 0) {
                zeroBits++;
                mask >>= 1;
            }
            break;
        }
    }

    return zeroBits >= requiredBits;
}

/**
 * Parse un montant décimal en satoshis (8 décimales).
 */
export function parseAmount(amount: string): bigint {
    const [whole, frac = ""] = amount.split(".");
    const fracPadded = frac.padEnd(8, "0").slice(0, 8);
    return BigInt(whole) * 100_000_000n + BigInt(fracPadded);
}

/**
 * Formate des satoshis en montant décimal.
 */
export function formatAmount(sats: bigint): string {
    const whole = sats / 100_000_000n;
    const frac = sats % 100_000_000n;
    const fracStr = frac.toString().padStart(8, "0");
    return `${whole}.${fracStr}`;
}
