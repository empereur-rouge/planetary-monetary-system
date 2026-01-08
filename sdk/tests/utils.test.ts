/**
 * Tests pour les utilitaires
 */

import { describe, it, expect } from "vitest";
import {
    toHex,
    fromHex,
    parseAmount,
    formatAmount,
    computeBlockId,
    checkPowBits,
} from "../src/utils";

describe("toHex / fromHex", () => {
    it("convertit bytes -> hex -> bytes", () => {
        const original = new Uint8Array([0x01, 0x23, 0x45, 0xab, 0xcd, 0xef]);
        const hex = toHex(original);
        const back = fromHex(hex);

        expect(hex).toBe("012345abcdef");
        expect(back).toEqual(original);
    });

    it("gère les bytes vides", () => {
        expect(toHex(new Uint8Array([]))).toBe("");
        expect(fromHex("")).toEqual(new Uint8Array([]));
    });
});

describe("parseAmount / formatAmount", () => {
    it("parse un montant décimal en satoshis", () => {
        expect(parseAmount("1.0")).toBe(100_000_000n);
        expect(parseAmount("0.5")).toBe(50_000_000n);
        expect(parseAmount("10.12345678")).toBe(1_012_345_678n);
    });

    it("formate des satoshis en décimal", () => {
        expect(formatAmount(100_000_000n)).toBe("1.00000000");
        expect(formatAmount(50_000_000n)).toBe("0.50000000");
        expect(formatAmount(1_012_345_678n)).toBe("10.12345678");
    });

    it("aller-retour conserve la valeur", () => {
        const amounts = ["0.00000001", "1.0", "999999.99999999"];
        for (const amount of amounts) {
            const sats = parseAmount(amount);
            const back = formatAmount(sats);
            expect(parseAmount(back)).toBe(sats);
        }
    });
});

describe("computeBlockId", () => {
    it("calcule un ID de bloc déterministe", () => {
        const parents = ["abc123", "def456"];
        const payload = '{"test": true}';
        const nonce = 42;

        const id1 = computeBlockId(parents, payload, nonce);
        const id2 = computeBlockId(parents, payload, nonce);

        expect(id1).toBe(id2);
        expect(id1).toHaveLength(64); // SHA256 = 32 bytes = 64 hex
    });

    it("change avec des inputs différents", () => {
        const id1 = computeBlockId(["a"], undefined, 0);
        const id2 = computeBlockId(["b"], undefined, 0);
        const id3 = computeBlockId(["a"], undefined, 1);

        expect(id1).not.toBe(id2);
        expect(id1).not.toBe(id3);
    });

    it("trie les parents", () => {
        const id1 = computeBlockId(["a", "b"], undefined, 0);
        const id2 = computeBlockId(["b", "a"], undefined, 0);

        // L'ordre ne devrait pas changer l'ID
        expect(id1).toBe(id2);
    });
});

describe("checkPowBits", () => {
    it("accepte 0 bits requis", () => {
        expect(checkPowBits("ffff", 0)).toBe(true);
    });

    it("vérifie les leading zeros", () => {
        // 00xx... a au moins 8 zero bits
        expect(checkPowBits("00abcdef", 8)).toBe(true);
        expect(checkPowBits("00abcdef", 9)).toBe(false);

        // 0000... a au moins 16 zero bits
        expect(checkPowBits("0000abcd", 16)).toBe(true);
        expect(checkPowBits("0000abcd", 17)).toBe(false);
    });
});
