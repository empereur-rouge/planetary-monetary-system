/**
 * Tests pour les fonctions de chiffrement
 */

import { describe, it, expect } from "vitest";
import {
    encryptPayload,
    decryptPayload,
    generateX25519Keypair,
    deriveX25519PublicKey,
    formatCubeAttributesMessage,
    signCubeAttributes,
} from "../src/crypto";
import { PmsWallet } from "../src/wallet";

describe("X25519 Key Generation", () => {
    it("génère une paire de clés valide", () => {
        const keypair = generateX25519Keypair();

        // Clé privée: 32 bytes = 64 hex chars
        expect(keypair.privateKey).toHaveLength(64);
        // Clé publique: 32 bytes = 64 hex chars
        expect(keypair.publicKey).toHaveLength(64);
    });

    it("dérive correctement une clé publique depuis une privée", () => {
        const keypair = generateX25519Keypair();
        const derived = deriveX25519PublicKey(keypair.privateKey);

        expect(derived).toBe(keypair.publicKey);
    });

    it("génère des clés uniques", () => {
        const kp1 = generateX25519Keypair();
        const kp2 = generateX25519Keypair();

        expect(kp1.privateKey).not.toBe(kp2.privateKey);
        expect(kp1.publicKey).not.toBe(kp2.publicKey);
    });
});

describe("PmsWallet X25519 Integration", () => {
    it("expose les clés X25519 dérivées", () => {
        const wallet = PmsWallet.generate();

        // Les clés X25519 doivent être disponibles
        expect(wallet.x25519PublicKeyHex).toHaveLength(64);
        expect(wallet.x25519PrivateKeyHex).toHaveLength(64);
    });

    it("dérive les mêmes clés X25519 depuis le même mnemonic", () => {
        const wallet1 = PmsWallet.generate();
        const wallet2 = PmsWallet.fromMnemonic(wallet1.mnemonic!);

        expect(wallet2.x25519PublicKeyHex).toBe(wallet1.x25519PublicKeyHex);
        expect(wallet2.x25519PrivateKeyHex).toBe(wallet1.x25519PrivateKeyHex);
    });

    it("génère des clés X25519 différentes de secp256k1", () => {
        const wallet = PmsWallet.generate();

        // Les clés X25519 ne doivent pas être identiques aux clés secp256k1
        expect(wallet.x25519PublicKeyHex).not.toBe(wallet.publicKeyHex);
    });
});

describe("encryptPayload / decryptPayload", () => {
    it("chiffre et déchiffre un message simple", () => {
        const keypair = generateX25519Keypair();
        const plaintext = "Hello, encrypted world!";

        const encrypted = encryptPayload(plaintext, [keypair.publicKey]);
        const decrypted = decryptPayload(encrypted, keypair.privateKey);

        expect(decrypted).toBe(plaintext);
    });

    it("chiffre et déchiffre un objet JSON", () => {
        const keypair = generateX25519Keypair();
        const data = { name: "Test", value: 42, nested: { a: 1, b: 2 } };
        const plaintext = JSON.stringify(data);

        const encrypted = encryptPayload(plaintext, [keypair.publicKey]);
        const decrypted = decryptPayload(encrypted, keypair.privateKey);

        expect(JSON.parse(decrypted)).toEqual(data);
    });

    it("produit le bon format de sortie", () => {
        const keypair = generateX25519Keypair();
        const encrypted = encryptPayload("test", [keypair.publicKey]);

        // Vérification du format compatible Rust
        expect(encrypted.scheme).toBe("x25519+aes256gcm");
        expect(encrypted.key_version).toBe(1);
        expect(encrypted.aad).toHaveProperty("len_hint");
        expect(encrypted.commitment).toHaveLength(64); // SHA256 hex
        expect(encrypted.ciphertext_b64).toBeDefined();
        expect(encrypted.nonce_b64).toBeDefined();
        expect(encrypted.recipients).toHaveLength(1);
        expect(encrypted.recipients[0]).toHaveProperty("kid");
        expect(encrypted.recipients[0]).toHaveProperty("ephem_pub");
        expect(encrypted.recipients[0]).toHaveProperty("wrapped_key_b64");
        expect(encrypted.recipients[0]).toHaveProperty("kw_nonce_b64");
    });

    it("supporte plusieurs destinataires", () => {
        const kp1 = generateX25519Keypair();
        const kp2 = generateX25519Keypair();
        const plaintext = "Message for multiple recipients";

        const encrypted = encryptPayload(plaintext, [kp1.publicKey, kp2.publicKey]);

        // Les deux destinataires peuvent déchiffrer
        expect(decryptPayload(encrypted, kp1.privateKey)).toBe(plaintext);
        expect(decryptPayload(encrypted, kp2.privateKey)).toBe(plaintext);
    });

    it("échoue avec une mauvaise clé", () => {
        const kp1 = generateX25519Keypair();
        const kp2 = generateX25519Keypair();
        const encrypted = encryptPayload("secret", [kp1.publicKey]);

        // La mauvaise clé ne doit pas pouvoir déchiffrer
        expect(() => decryptPayload(encrypted, kp2.privateKey)).toThrow();
    });
});

// ═══════════════════════════════════════════════════════════════════════════
// Tests pour Authority Signing (Burn-to-Mint)
// ═══════════════════════════════════════════════════════════════════════════

describe("formatCubeAttributesMessage", () => {
    it("formate les attributs dans l'ordre canonique", () => {
        const message = formatCubeAttributesMessage(50, 75, 25);
        expect(message).toBe("weight:50,size:75,density:25");
    });

    it("gère les valeurs minimales (1)", () => {
        const message = formatCubeAttributesMessage(1, 1, 1);
        expect(message).toBe("weight:1,size:1,density:1");
    });

    it("gère les valeurs maximales (100)", () => {
        const message = formatCubeAttributesMessage(100, 100, 100);
        expect(message).toBe("weight:100,size:100,density:100");
    });

    it("gère les zéros", () => {
        const message = formatCubeAttributesMessage(0, 0, 0);
        expect(message).toBe("weight:0,size:0,density:0");
    });

    it("produit un format compatible avec le backend Rust", () => {
        // Ce format doit correspondre EXACTEMENT à burn_refund.rs::attributes_to_signed_message
        const message = formatCubeAttributesMessage(42, 77, 13);
        expect(message).toBe("weight:42,size:77,density:13");
    });
});

describe("signCubeAttributes", () => {
    it("produit une signature Base64 valide", () => {
        const authority = PmsWallet.generate();
        const signature = signCubeAttributes(50, 50, 50, authority);

        // La signature doit être en Base64
        expect(() => atob(signature)).not.toThrow();
        // Base64 d'une signature DER fait environ 90-100 caractères
        expect(signature.length).toBeGreaterThan(50);
    });

    it("produit des signatures différentes pour des attributs différents", () => {
        const authority = PmsWallet.generate();

        const sig1 = signCubeAttributes(10, 20, 30, authority);
        const sig2 = signCubeAttributes(30, 20, 10, authority);

        expect(sig1).not.toBe(sig2);
    });

    it("produit la même signature pour les mêmes attributs avec le même wallet", () => {
        const authority = PmsWallet.generate();

        const sig1 = signCubeAttributes(50, 50, 50, authority);
        const sig2 = signCubeAttributes(50, 50, 50, authority);

        expect(sig1).toBe(sig2);
    });

    it("produit des signatures différentes avec des wallets différents", () => {
        const authority1 = PmsWallet.generate();
        const authority2 = PmsWallet.generate();

        const sig1 = signCubeAttributes(50, 50, 50, authority1);
        const sig2 = signCubeAttributes(50, 50, 50, authority2);

        expect(sig1).not.toBe(sig2);
    });

    it("la signature peut être vérifiée avec PmsWallet.verify", () => {
        const authority = PmsWallet.generate();
        const weight = 42, size = 77, density = 13;

        const signature = signCubeAttributes(weight, size, density, authority);

        // Reconstruire le message et vérifier
        const message = formatCubeAttributesMessage(weight, size, density);
        const messageBytes = new TextEncoder().encode(message);

        // Décoder la signature Base64 en hex pour verify
        const sigBytes = Uint8Array.from(atob(signature), c => c.charCodeAt(0));
        const sigHex = Array.from(sigBytes).map(b => b.toString(16).padStart(2, '0')).join('');

        const isValid = PmsWallet.verify(messageBytes, sigHex, authority.publicKeyHex);
        expect(isValid).toBe(true);
    });

    it("la vérification échoue avec un message modifié", () => {
        const authority = PmsWallet.generate();
        const signature = signCubeAttributes(50, 50, 50, authority);

        // Message différent
        const wrongMessage = formatCubeAttributesMessage(51, 50, 50);
        const wrongMessageBytes = new TextEncoder().encode(wrongMessage);

        const sigBytes = Uint8Array.from(atob(signature), c => c.charCodeAt(0));
        const sigHex = Array.from(sigBytes).map(b => b.toString(16).padStart(2, '0')).join('');

        const isValid = PmsWallet.verify(wrongMessageBytes, sigHex, authority.publicKeyHex);
        expect(isValid).toBe(false);
    });
});
