/**
 * Tests pour PmsWallet
 */

import { describe, it, expect } from "vitest";
import { PmsWallet, isValidMnemonic } from "../src/wallet";

describe("PmsWallet", () => {
    describe("generate()", () => {
        it("génère un wallet avec 24 mots", () => {
            const wallet = PmsWallet.generate();

            expect(wallet.mnemonic).toBeDefined();
            const words = wallet.mnemonic!.split(" ");
            expect(words.length).toBe(24);
        });

        it("génère une adresse valide (clé publique)", () => {
            const wallet = PmsWallet.generate();

            // Clé publique non compressée: 04 + 64 bytes = 130 hex chars
            expect(wallet.address).toHaveLength(130);
            expect(wallet.address.startsWith("04")).toBe(true);
        });

        it("génère des wallets uniques", () => {
            const wallet1 = PmsWallet.generate();
            const wallet2 = PmsWallet.generate();

            expect(wallet1.address).not.toBe(wallet2.address);
            expect(wallet1.mnemonic).not.toBe(wallet2.mnemonic);
        });
    });

    describe("fromMnemonic()", () => {
        it("restaure un wallet depuis un mnemonic valide", () => {
            const original = PmsWallet.generate();
            const restored = PmsWallet.fromMnemonic(original.mnemonic!);

            expect(restored.address).toBe(original.address);
            expect(restored.publicKeyHex).toBe(original.publicKeyHex);
        });

        it("rejette un mnemonic invalide", () => {
            expect(() => {
                PmsWallet.fromMnemonic("invalid mnemonic phrase");
            }).toThrow("Invalid mnemonic phrase");
        });

        it("normalise les espaces et la casse", () => {
            const wallet = PmsWallet.generate();
            const mnemonicUpper = wallet.mnemonic!.toUpperCase();
            const restored = PmsWallet.fromMnemonic(`  ${mnemonicUpper}  `);

            expect(restored.address).toBe(wallet.address);
        });
    });

    describe("fromPrivateKey()", () => {
        it("crée un wallet depuis une clé privée hex", () => {
            const original = PmsWallet.generate();
            const privateKey = original.exportPrivateKey();

            const restored = PmsWallet.fromPrivateKey(privateKey);

            expect(restored.address).toBe(original.address);
            expect(restored.mnemonic).toBeUndefined(); // Pas de mnemonic
        });

        it("rejette une clé privée de mauvaise taille", () => {
            expect(() => {
                PmsWallet.fromPrivateKey("abcd"); // Trop courte
            }).toThrow("Private key must be 32 bytes");
        });
    });

    describe("fromSeed()", () => {
        it("crée un wallet depuis une seed", () => {
            const seed = new Uint8Array(32).fill(0xab);
            const wallet = PmsWallet.fromSeed(seed);

            expect(wallet.address).toHaveLength(130);
            expect(wallet.mnemonic).toBeUndefined();
        });

        it("produit des résultats déterministes", () => {
            const seed = new Uint8Array(32).fill(0x42);
            const wallet1 = PmsWallet.fromSeed(seed);
            const wallet2 = PmsWallet.fromSeed(seed);

            expect(wallet1.address).toBe(wallet2.address);
        });
    });

    describe("sign() et verify()", () => {
        it("signe et vérifie un message", () => {
            const wallet = PmsWallet.generate();
            const message = new TextEncoder().encode("Hello PMS!");

            const signature = wallet.sign(message);

            expect(signature).toBeDefined();
            expect(signature.length).toBeGreaterThan(0);

            // Vérification
            const isValid = PmsWallet.verify(message, signature, wallet.publicKeyHex);
            expect(isValid).toBe(true);
        });

        it("rejette une signature invalide", () => {
            const wallet = PmsWallet.generate();
            const message = new TextEncoder().encode("Hello");
            const wrongMessage = new TextEncoder().encode("Wrong");

            const signature = wallet.sign(message);

            const isValid = PmsWallet.verify(wrongMessage, signature, wallet.publicKeyHex);
            expect(isValid).toBe(false);
        });

        it("rejette une clé publique différente", () => {
            const wallet1 = PmsWallet.generate();
            const wallet2 = PmsWallet.generate();
            const message = new TextEncoder().encode("Test");

            const signature = wallet1.sign(message);

            const isValid = PmsWallet.verify(message, signature, wallet2.publicKeyHex);
            expect(isValid).toBe(false);
        });
    });
});

describe("isValidMnemonic()", () => {
    it("valide un mnemonic correct", () => {
        const wallet = PmsWallet.generate();
        expect(isValidMnemonic(wallet.mnemonic!)).toBe(true);
    });

    it("rejette un mnemonic invalide", () => {
        expect(isValidMnemonic("not a valid mnemonic")).toBe(false);
        expect(isValidMnemonic("")).toBe(false);
    });
});
