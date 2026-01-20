/**
 * Fonctions de chiffrement pour le SDK PMS.
 * 
 * Compatible avec le backend Rust (pms-types-payload/encrypted_payload.rs).
 * Schéma: X25519 + AES-256-GCM avec key wrapping multi-destinataires.
 * 
 * CONCEPTS CLÉS (pour débutants Rust, applicable ici aussi):
 * - DEK (Data Encryption Key): Clé symétrique pour chiffrer les données
 * - KEK (Key Encryption Key): Clé pour "envelopper" la DEK
 * - ECDH: Échange de clé Diffie-Hellman sur courbe elliptique
 * - HKDF: Fonction de dérivation de clé à partir d'un secret partagé
 */

import { x25519 } from "@noble/curves/ed25519";
import { gcm } from "@noble/ciphers/aes.js";
import { hkdf } from "@noble/hashes/hkdf";
import { sha256 } from "@noble/hashes/sha2";
import { randomBytes, bytesToHex, hexToBytes } from "@noble/hashes/utils";
import { base64 } from "@scure/base";
import type { EncryptedPayload, KeyWrap } from "./types";

// ═══════════════════════════════════════════════════════════════════════════
// Constantes (identiques au backend Rust)
// ═══════════════════════════════════════════════════════════════════════════

const SCHEME = "x25519+aes256gcm";
const KEY_VERSION = 1;
const HKDF_SALT = new TextEncoder().encode("pms-dek-wrap");
const HKDF_INFO_KEK = new TextEncoder().encode("kek-v1");
const HKDF_INFO_KID = new TextEncoder().encode("kid-v1");

// ═══════════════════════════════════════════════════════════════════════════
// Fonctions utilitaires
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Encode en base64 standard (compatible avec Rust general_purpose::STANDARD).
 */
function toBase64(data: Uint8Array): string {
    return base64.encode(data);
}

/**
 * Décode depuis base64.
 */
function fromBase64(str: string): Uint8Array {
    return base64.decode(str);
}

/**
 * Calcule SHA256 et retourne en hex.
 */
function sha256Hex(data: Uint8Array): string {
    return bytesToHex(sha256(data));
}

// ═══════════════════════════════════════════════════════════════════════════
// Encryption
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Chiffre des données pour un ou plusieurs destinataires.
 * 
 * Chaque destinataire doit fournir sa clé publique X25519 (32 bytes hex).
 * Seul le détenteur de la clé privée correspondante pourra déchiffrer.
 * 
 * @param plaintext - Données à chiffrer (string ou bytes)
 * @param recipientPublicKeysHex - Liste des clés publiques X25519 des destinataires
 * @returns EncryptedPayload compatible avec le backend Rust
 * 
 * @example
 * ```typescript
 * const encrypted = encryptPayload(
 *     JSON.stringify({ secret: "data" }),
 *     [recipientX25519PublicKeyHex]
 * );
 * ```
 */
export function encryptPayload(
    plaintext: string | Uint8Array,
    recipientPublicKeysHex: string[]
): EncryptedPayload {
    // Convertir en bytes si nécessaire
    const plaintextBytes = typeof plaintext === "string"
        ? new TextEncoder().encode(plaintext)
        : plaintext;

    // ─────────────────────────────────────────────────────────────────────────
    // ÉTAPE 1: Générer DEK (Data Encryption Key) et nonce
    // ─────────────────────────────────────────────────────────────────────────
    // DEK: clé symétrique aléatoire de 32 bytes pour AES-256
    // Nonce: valeur unique de 12 bytes pour AES-GCM
    const dek = randomBytes(32);
    const nonce = randomBytes(12);

    // ─────────────────────────────────────────────────────────────────────────
    // ÉTAPE 2: Préparer AAD (Additional Authenticated Data)
    // ─────────────────────────────────────────────────────────────────────────
    // L'AAD est authentifié mais pas chiffré - permet de vérifier l'intégrité
    const aad = { len_hint: plaintextBytes.length };
    const aadBytes = new TextEncoder().encode(JSON.stringify(aad));

    // ─────────────────────────────────────────────────────────────────────────
    // ÉTAPE 3: Chiffrer le payload avec AES-256-GCM
    // ─────────────────────────────────────────────────────────────────────────
    const cipher = gcm(dek, nonce, aadBytes);
    const ciphertext = cipher.encrypt(plaintextBytes);

    // Commitment: hash du plaintext pour vérification ultérieure
    const commitment = sha256Hex(plaintextBytes);

    // ─────────────────────────────────────────────────────────────────────────
    // ÉTAPE 4: Générer une clé éphémère X25519 (unique par message)
    // ─────────────────────────────────────────────────────────────────────────
    // Cette clé est jetée après usage (forward secrecy)
    const ephemeralPrivateKey = randomBytes(32);
    const ephemeralPublicKey = x25519.getPublicKey(ephemeralPrivateKey);
    const ephemeralPublicKeyHex = bytesToHex(ephemeralPublicKey);

    // ─────────────────────────────────────────────────────────────────────────
    // ÉTAPE 5: Envelopper la DEK pour chaque destinataire
    // ─────────────────────────────────────────────────────────────────────────
    const recipients: KeyWrap[] = [];

    for (const recipientPkHex of recipientPublicKeysHex) {
        const recipientPk = hexToBytes(recipientPkHex);

        // ECDH: calcul du secret partagé
        // ephemeral_private × recipient_public = shared_secret
        const sharedSecret = x25519.getSharedSecret(ephemeralPrivateKey, recipientPk);

        // HKDF: dériver KEK (Key Encryption Key) et KID (Key ID)
        // Le KID est un identifiant opaque qui ne révèle pas la clé publique
        const kek = hkdf(sha256, sharedSecret, HKDF_SALT, HKDF_INFO_KEK, 32);
        const kidBytes = hkdf(sha256, sharedSecret, HKDF_SALT, HKDF_INFO_KID, 16);
        const kid = bytesToHex(kidBytes);

        // Wrap: chiffrer la DEK avec la KEK
        const kwNonce = randomBytes(12);
        const kwCipher = gcm(kek, kwNonce, new TextEncoder().encode(kid));
        const wrappedKey = kwCipher.encrypt(dek);

        recipients.push({
            kid,
            ephem_pub: ephemeralPublicKeyHex,
            wrapped_key_b64: toBase64(wrappedKey),
            kw_nonce_b64: toBase64(kwNonce),
        });
    }

    // ─────────────────────────────────────────────────────────────────────────
    // ÉTAPE 6: Assembler l'enveloppe chiffrée
    // ─────────────────────────────────────────────────────────────────────────
    return {
        scheme: SCHEME,
        key_version: KEY_VERSION,
        aad,
        commitment,
        ciphertext_b64: toBase64(ciphertext),
        recipients,
        nonce_b64: toBase64(nonce),
    };
}

// ═══════════════════════════════════════════════════════════════════════════
// Decryption
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Déchiffre un payload chiffré avec la clé privée X25519 du destinataire.
 * 
 * @param encrypted - Payload chiffré (output de encryptPayload)
 * @param recipientPrivateKeyHex - Clé privée X25519 du destinataire (64 chars hex)
 * @returns Les données en clair (string)
 * @throws Error si le déchiffrement échoue
 * 
 * @example
 * ```typescript
 * const plaintext = decryptPayload(encrypted, myX25519PrivateKeyHex);
 * const data = JSON.parse(plaintext);
 * ```
 */
export function decryptPayload(
    encrypted: EncryptedPayload,
    recipientPrivateKeyHex: string
): string {
    // Vérifier le schéma
    if (encrypted.scheme !== SCHEME) {
        throw new Error(`Schéma non supporté: ${encrypted.scheme}`);
    }

    const recipientSk = hexToBytes(recipientPrivateKeyHex);

    // ─────────────────────────────────────────────────────────────────────────
    // ÉTAPE 1: Trouver le wrap qui nous concerne
    // ─────────────────────────────────────────────────────────────────────────
    let dek: Uint8Array | null = null;

    for (const wrap of encrypted.recipients) {
        // Recalculer le secret partagé avec la clé éphémère
        const ephemeralPk = hexToBytes(wrap.ephem_pub);
        const sharedSecret = x25519.getSharedSecret(recipientSk, ephemeralPk);

        // Dériver KEK et KID attendu
        const kek = hkdf(sha256, sharedSecret, HKDF_SALT, HKDF_INFO_KEK, 32);
        const kidBytes = hkdf(sha256, sharedSecret, HKDF_SALT, HKDF_INFO_KID, 16);
        const expectedKid = bytesToHex(kidBytes);

        // Vérifier si ce wrap nous est destiné
        if (expectedKid !== wrap.kid) {
            continue;
        }

        // Déballer la DEK
        try {
            const kwNonce = fromBase64(wrap.kw_nonce_b64);
            const wrappedKey = fromBase64(wrap.wrapped_key_b64);
            const kwCipher = gcm(kek, kwNonce, new TextEncoder().encode(wrap.kid));
            dek = kwCipher.decrypt(wrappedKey);
            break;
        } catch {
            continue;
        }
    }

    if (!dek) {
        throw new Error("Aucun destinataire correspondant trouvé ou déballage échoué");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // ÉTAPE 2: Déchiffrer le contenu
    // ─────────────────────────────────────────────────────────────────────────
    const nonce = fromBase64(encrypted.nonce_b64);
    const ciphertext = fromBase64(encrypted.ciphertext_b64);
    const aadBytes = new TextEncoder().encode(JSON.stringify(encrypted.aad));

    const cipher = gcm(dek, nonce, aadBytes);
    const plaintext = cipher.decrypt(ciphertext);

    // ─────────────────────────────────────────────────────────────────────────
    // ÉTAPE 3: Vérifier le commitment
    // ─────────────────────────────────────────────────────────────────────────
    const gotCommitment = sha256Hex(plaintext);
    if (gotCommitment !== encrypted.commitment) {
        throw new Error("Commitment mismatch - données corrompues");
    }

    return new TextDecoder().decode(plaintext);
}

// ═══════════════════════════════════════════════════════════════════════════


// ═══════════════════════════════════════════════════════════════════════════
// Génération de clés X25519
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Génère une paire de clés X25519.
 * 
 * @returns { privateKey: string, publicKey: string } en hex
 */
export function generateX25519Keypair(): { privateKey: string; publicKey: string } {
    const privateKey = randomBytes(32);
    const publicKey = x25519.getPublicKey(privateKey);
    return {
        privateKey: bytesToHex(privateKey),
        publicKey: bytesToHex(publicKey),
    };
}

/**
 * Dérive une clé publique X25519 depuis une clé privée.
 * 
 * @param privateKeyHex - Clé privée X25519 (64 chars hex)
 * @returns Clé publique X25519 (64 chars hex)
 */
export function deriveX25519PublicKey(privateKeyHex: string): string {
    const privateKey = hexToBytes(privateKeyHex);
    const publicKey = x25519.getPublicKey(privateKey);
    return bytesToHex(publicKey);
}

// ═══════════════════════════════════════════════════════════════════════════
// Authority Signing (for Cube NFTs burn-to-mint)
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Formats cube attributes into the canonical message format for signing.
 * 
 * This MUST match the Rust backend format in burn_refund.rs:
 * `"weight:X,size:Y,density:Z"`
 * 
 * @param weight - Cube weight (1-100)
 * @param size - Cube size (1-100)
 * @param density - Cube density (1-100)
 * @returns Canonical string to sign
 */
export function formatCubeAttributesMessage(
    weight: number,
    size: number,
    density: number
): string {
    return `weight:${weight},size:${size},density:${density}`;
}

/**
 * Signs cube attributes with an Authority wallet.
 * 
 * The signature proves that the cube was officially generated by the Authority
 * and enables burn-to-mint refunds on the DAG.
 * 
 * @param weight - Cube weight
 * @param size - Cube size
 * @param density - Cube density
 * @param authorityWallet - Wallet with Authority private key
 * @returns Base64-encoded DER signature
 * 
 * @example
 * ```typescript
 * const authority = PmsWallet.fromPrivateKey(AUTHORITY_PRIVATE_KEY);
 * const signature = signCubeAttributes(50, 50, 50, authority);
 * // Include this signature in cube metadata.extra
 * ```
 */
export function signCubeAttributes(
    weight: number,
    size: number,
    density: number,
    authorityWallet: { sign: (message: Uint8Array) => string }
): string {
    // 1. Format the canonical message
    const message = formatCubeAttributesMessage(weight, size, density);
    const messageBytes = new TextEncoder().encode(message);

    // 2. Sign with Authority's secp256k1 key (returns DER hex)
    const signatureHex = authorityWallet.sign(messageBytes);

    // 3. Convert to Base64 (matches Rust backend expectation)
    const signatureBytes = hexToBytes(signatureHex);
    return toBase64(signatureBytes);
}
