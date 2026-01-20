/**
 * PmsWallet - Wallet pour le réseau PMS.
 * 
 * Supporte:
 * - Génération de wallet avec mnémonique 24 mots (BIP39)
 * - Import depuis clé privée
 * - Signature de messages
 * - Export de la phrase mnémonique
 * 
 * @example
 * ```typescript
 * // Générer un nouveau wallet
 * const wallet = PmsWallet.generate();
 * console.log(wallet.mnemonic); // 24 mots
 * 
 * // Restaurer depuis mnemonic
 * const wallet2 = PmsWallet.fromMnemonic("word1 word2 ...");
 * 
 * // Signer un message
 * const sig = await wallet.sign(messageBytes);
 * ```
 */

import { secp256k1 } from "@noble/curves/secp256k1";
import { x25519 } from "@noble/curves/ed25519";
import { sha256 } from "@noble/hashes/sha2";
import { hkdf } from "@noble/hashes/hkdf";
import { generateMnemonic, mnemonicToSeedSync, validateMnemonic } from "@scure/bip39";
import { wordlist } from "@scure/bip39/wordlists/english";
import { toHex, fromHex } from "./utils";

/**
 * Wallet PMS avec gestion des clés cryptographiques.
 */
export class PmsWallet {
    /** Clé privée secp256k1 (32 bytes) - pour signatures */
    private readonly _privateKey: Uint8Array;

    /** Clé publique secp256k1 non compressée (65 bytes: 04 + x + y) */
    private readonly _publicKey: Uint8Array;

    /** Clé privée X25519 (32 bytes) - pour chiffrement */
    private readonly _x25519PrivateKey: Uint8Array;

    /** Clé publique X25519 (32 bytes) */
    private readonly _x25519PublicKey: Uint8Array;

    /** Phrase mnémonique (24 mots) si générée/importée */
    private readonly _mnemonic?: string;

    /**
     * Constructeur privé - utiliser les méthodes statiques.
     */
    private constructor(privateKey: Uint8Array, mnemonic?: string) {
        this._privateKey = privateKey;
        this._publicKey = secp256k1.getPublicKey(privateKey, false); // false = uncompressed

        // Dérive une clé X25519 depuis la même seed via HKDF
        // Cela permet d'avoir une seule phrase mnémonique pour tout
        // (signatures secp256k1 ET chiffrement X25519)
        this._x25519PrivateKey = hkdf(
            sha256,
            privateKey,
            new TextEncoder().encode("pms-x25519"),  // salt
            new TextEncoder().encode("encryption"),  // info
            32
        );
        this._x25519PublicKey = x25519.getPublicKey(this._x25519PrivateKey);

        this._mnemonic = mnemonic;
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Méthodes statiques de création
    // ═══════════════════════════════════════════════════════════════════════

    /**
     * Génère un nouveau wallet avec une phrase de 24 mots.
     */
    static generate(): PmsWallet {
        // 256 bits = 24 mots
        const mnemonic = generateMnemonic(wordlist, 256);
        return PmsWallet.fromMnemonic(mnemonic);
    }

    /**
     * Crée un wallet à partir d'une phrase mnémonique (12, 15, 18, 21 ou 24 mots).
     * @throws Error si la phrase est invalide
     */
    static fromMnemonic(mnemonic: string): PmsWallet {
        const normalized = mnemonic.trim().toLowerCase();

        if (!validateMnemonic(normalized, wordlist)) {
            throw new Error("Invalid mnemonic phrase");
        }

        // Dérive la seed depuis le mnemonic (sans passphrase)
        const seed = mnemonicToSeedSync(normalized);

        // Utilise les 32 premiers bytes comme clé privée
        // (Simplification - en production, utiliser BIP32 derivation)
        const privateKey = seed.slice(0, 32);

        return new PmsWallet(privateKey, normalized);
    }

    /**
     * Crée un wallet à partir d'une clé privée hexadécimale.
     */
    static fromPrivateKey(privateKeyHex: string): PmsWallet {
        const privateKey = fromHex(privateKeyHex);

        if (privateKey.length !== 32) {
            throw new Error("Private key must be 32 bytes");
        }

        return new PmsWallet(privateKey);
    }

    /**
     * Crée un wallet à partir d'une seed (32 bytes).
     */
    static fromSeed(seed: Uint8Array): PmsWallet {
        if (seed.length !== 32) {
            throw new Error("Seed must be 32 bytes");
        }
        return new PmsWallet(seed);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Propriétés publiques
    // ═══════════════════════════════════════════════════════════════════════

    /**
     * Adresse du wallet (clé publique hex).
     * Format: "04" + 64 bytes hex = 130 caractères
     */
    get address(): string {
        return toHex(this._publicKey);
    }

    /**
     * Clé publique en bytes.
     */
    get publicKey(): Uint8Array {
        return this._publicKey;
    }

    /**
     * Clé publique en hex.
     */
    get publicKeyHex(): string {
        return toHex(this._publicKey);
    }

    /**
     * Phrase mnémonique (si disponible).
     * @returns undefined si le wallet a été créé depuis une clé privée
     */
    get mnemonic(): string | undefined {
        return this._mnemonic;
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Clés X25519 (pour chiffrement)
    // ═══════════════════════════════════════════════════════════════════════

    /**
     * Clé publique X25519 en hex (pour chiffrement).
     * Utiliser cette clé comme destinataire pour encryptPayload().
     */
    get x25519PublicKeyHex(): string {
        return toHex(this._x25519PublicKey);
    }

    /**
     * Clé privée X25519 en hex (pour déchiffrement).
     * ⚠️ Ne pas exposer cette clé publiquement !
     */
    get x25519PrivateKeyHex(): string {
        return toHex(this._x25519PrivateKey);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Méthodes de signature
    // ═══════════════════════════════════════════════════════════════════════

    /**
     * Signe un message avec la clé privée.
     * @param message - Message à signer (sera hashé avec SHA256)
     * @returns Signature DER encodée en hex
     */
    sign(message: Uint8Array): string {
        const hash = sha256(message);
        const sig = secp256k1.sign(hash, this._privateKey);
        return sig.toDERHex();
    }

    /**
     * Signe un message déjà hashé.
     * @param hash - Hash 32 bytes du message
     * @returns Signature DER encodée en hex
     */
    signHash(hash: Uint8Array): string {
        if (hash.length !== 32) {
            throw new Error("Hash must be 32 bytes");
        }
        const sig = secp256k1.sign(hash, this._privateKey);
        return sig.toDERHex();
    }

    /**
     * Exporte la clé privée en hex.
     * ⚠️ À utiliser avec précaution !
     */
    exportPrivateKey(): string {
        return toHex(this._privateKey);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Méthodes statiques de vérification
    // ═══════════════════════════════════════════════════════════════════════

    /**
     * Vérifie une signature.
     * @param message - Message original
     * @param signature - Signature DER hex
     * @param publicKeyHex - Clé publique hex du signataire
     */
    static verify(message: Uint8Array, signature: string, publicKeyHex: string): boolean {
        try {
            const hash = sha256(message);
            const pubKey = fromHex(publicKeyHex);
            const sig = secp256k1.Signature.fromDER(signature);
            return secp256k1.verify(sig.toCompactRawBytes(), hash, pubKey);
        } catch {
            return false;
        }
    }
}

/**
 * Vérifie si une phrase mnémonique est valide.
 */
export function isValidMnemonic(mnemonic: string): boolean {
    return validateMnemonic(mnemonic.trim().toLowerCase(), wordlist);
}
