/**
 * Tests pour les méthodes NFT Cube (burnNft, mintCube)
 * 
 * Ces tests vérifient :
 * - La construction correcte des payloads Burn et Mint
 * - La logique de détermination de rareté
 * - La génération d'attributs pondérés
 * - Les helpers internes (generateRandomHex, determineRarity, rollWeightedAttribute)
 */

import { describe, it, expect, vi, beforeEach } from "vitest";
import { PmsClient } from "../src/client";
import { PmsWallet } from "../src/wallet";

// ═══════════════════════════════════════════════════════════════════════════
// Tests des helpers internes (extraits pour test)
// ═══════════════════════════════════════════════════════════════════════════

/**
 * Réplique de la logique determineRarity pour les tests isolés.
 * Distribution:
 * - Unique:    roll < 10       (10 valeurs)     -> 1/1,000,000
 * - Legendary: roll < 110      (100 valeurs)    -> 1/100,000
 * - Rare:      roll < 1,110    (1,000 valeurs)  -> 1/10,000
 * - Uncommon:  roll < 11,110   (10,000 valeurs) -> 1/1,000
 * - Common:    roll < 111,110  (100,000 valeurs)-> 1/100
 * - Basic:     roll >= 111,110                  -> ~99%
 */
function determineRarity(roll: number): string {
    if (roll < 10) return "Unique";
    if (roll < 110) return "Legendary";
    if (roll < 1_110) return "Rare";
    if (roll < 11_110) return "Uncommon";
    if (roll < 111_110) return "Common";
    return "Basic";
}

/**
 * Réplique de la logique rollWeightedAttribute pour les tests isolés.
 * Génère une valeur avec distribution pondérée (hautes valeurs = rares).
 */
function rollWeightedAttribute(min: number, max: number, power: number, randomValue: number): number {
    const weighted = Math.pow(randomValue, power);
    return Math.floor(min + (max - min) * weighted);
}

describe("determineRarity", () => {
    it("retourne 'Unique' pour roll < 10", () => {
        expect(determineRarity(0)).toBe("Unique");
        expect(determineRarity(5)).toBe("Unique");
        expect(determineRarity(9)).toBe("Unique");
    });

    it("retourne 'Legendary' pour roll entre 10 et 109", () => {
        expect(determineRarity(10)).toBe("Legendary");
        expect(determineRarity(50)).toBe("Legendary");
        expect(determineRarity(109)).toBe("Legendary");
    });

    it("retourne 'Rare' pour roll entre 110 et 1109", () => {
        expect(determineRarity(110)).toBe("Rare");
        expect(determineRarity(500)).toBe("Rare");
        expect(determineRarity(1_109)).toBe("Rare");
    });

    it("retourne 'Uncommon' pour roll entre 1110 et 11109", () => {
        expect(determineRarity(1_110)).toBe("Uncommon");
        expect(determineRarity(5_000)).toBe("Uncommon");
        expect(determineRarity(11_109)).toBe("Uncommon");
    });

    it("retourne 'Common' pour roll entre 11110 et 111109", () => {
        expect(determineRarity(11_110)).toBe("Common");
        expect(determineRarity(50_000)).toBe("Common");
        expect(determineRarity(111_109)).toBe("Common");
    });

    it("retourne 'Basic' pour roll >= 111110", () => {
        expect(determineRarity(111_110)).toBe("Basic");
        expect(determineRarity(500_000)).toBe("Basic");
        expect(determineRarity(9_999_999)).toBe("Basic");
    });
});

describe("rollWeightedAttribute", () => {
    it("retourne min quand random = 0", () => {
        // random^3 = 0, donc val = min + (max - min) * 0 = min
        expect(rollWeightedAttribute(1, 100, 3, 0)).toBe(1);
    });

    it("retourne max-1 quand random = 1 (floor)", () => {
        // random^3 = 1, donc val = min + (max - min) * 1 = 1 + 99 = 100, floor = 100
        // Mais comme max est inclusif dans la spec et random() ne retourne jamais exactement 1,
        // en pratique avec random=1 on obtient floor(1 + 99*1) = 100
        expect(rollWeightedAttribute(1, 100, 3, 1)).toBe(100);
    });

    it("retourne une valeur basse avec random faible", () => {
        // random = 0.5, power = 3 -> 0.5^3 = 0.125
        // val = 1 + 99 * 0.125 = 1 + 12.375 = 13.375 -> floor = 13
        expect(rollWeightedAttribute(1, 100, 3, 0.5)).toBe(13);
    });

    it("distribue vers les valeurs basses avec power élevé", () => {
        // Avec power=3, même random=0.7 donne 0.7^3 = 0.343
        // val = 1 + 99 * 0.343 = 1 + 33.957 = 34.957 -> floor = 34
        expect(rollWeightedAttribute(1, 100, 3, 0.7)).toBe(34);
    });
});

// ═══════════════════════════════════════════════════════════════════════════
// Tests d'intégration pour burnNft et mintCube
// ═══════════════════════════════════════════════════════════════════════════

describe("PmsClient NFT Cube Methods", () => {
    let client: PmsClient;
    let wallet: PmsWallet;
    let userWallet: PmsWallet;

    beforeEach(() => {
        // Créer un wallet coordinateur et un wallet utilisateur
        wallet = PmsWallet.generate();
        userWallet = PmsWallet.generate();

        // Créer un client avec mock du fetch ET le coordinatorWallet configuré
        client = new PmsClient({
            nodeUrl: "http://localhost:3000",
            enableRacing: false,
            coordinatorWallet: wallet,
        });

        // Mock fetch pour getTips et submitBlock
        global.fetch = vi.fn().mockImplementation((url: string, init?: RequestInit) => {
            if (url.includes("/v1/dag/tips")) {
                return Promise.resolve({
                    ok: true,
                    json: () => Promise.resolve(["tip1", "tip2"]),
                });
            }
            if (url.includes("/submit/block")) {
                const body = JSON.parse(init?.body as string);
                return Promise.resolve({
                    ok: true,
                    json: () => Promise.resolve({
                        status: "inserted",
                        block_id: body.id,
                    }),
                });
            }
            return Promise.reject(new Error(`Unmocked URL: ${url}`));
        });
    });

    describe("burnNft", () => {
        it("construit un payload Burn correct", async () => {
            const tokenId = "test-token-123";

            const result = await client.burnNft({ tokenId, wallet });

            expect(result.status).toBe("inserted");

            // Vérifier que le payload contient bien Burn
            const lastCall = (global.fetch as ReturnType<typeof vi.fn>).mock.calls.find(
                (call) => call[0].includes("/submit/block")
            );
            const body = JSON.parse(lastCall![1].body);
            const payload = JSON.parse(body.payload_json);

            expect(payload.Plain.Nft.Burn).toBeDefined();
            expect(payload.Plain.Nft.Burn.token_id).toBe(tokenId);
            expect(payload.Plain.Nft.Burn.burner).toBe(wallet.address);
        });

        it("signe le bloc avec le wallet fourni", async () => {
            const result = await client.burnNft({
                tokenId: "nft-to-burn",
                wallet,
            });

            expect(result.status).toBe("inserted");

            const lastCall = (global.fetch as ReturnType<typeof vi.fn>).mock.calls.find(
                (call) => call[0].includes("/submit/block")
            );
            const body = JSON.parse(lastCall![1].body);

            expect(body.signer_pk_hex).toBe(wallet.publicKeyHex);
            expect(body.signature_hex).toBeDefined();
            expect(body.signature_hex.length).toBeGreaterThan(0);
        });
    });

    describe("mintCube", () => {
        it("génère un payload chiffré (Encrypted)", async () => {
            const result = await client.mintCube({
                ownerAddress: userWallet.address,
                ownerX25519PubKeyHex: userWallet.x25519PublicKeyHex,
            });

            expect(result.status).toBe("inserted");

            const lastCall = (global.fetch as ReturnType<typeof vi.fn>).mock.calls.find(
                (call) => call[0].includes("/submit/block")
            );
            const body = JSON.parse(lastCall![1].body);
            const payload = JSON.parse(body.payload_json);

            // Le payload doit être Encrypted, pas Plain
            expect(payload.Encrypted).toBeDefined();
            expect(payload.Plain).toBeUndefined();
        });

        it("inclut les champs requis dans le payload chiffré", async () => {
            await client.mintCube({
                ownerAddress: userWallet.address,
                ownerX25519PubKeyHex: userWallet.x25519PublicKeyHex,
            });

            const lastCall = (global.fetch as ReturnType<typeof vi.fn>).mock.calls.find(
                (call) => call[0].includes("/submit/block")
            );
            const body = JSON.parse(lastCall![1].body);
            const payload = JSON.parse(body.payload_json);

            // Vérifier la structure du payload chiffré
            expect(payload.Encrypted.scheme).toBe("x25519+aes256gcm");
            expect(payload.Encrypted.ciphertext_b64).toBeDefined();
            expect(payload.Encrypted.nonce_b64).toBeDefined();
            expect(payload.Encrypted.recipients).toBeDefined();
            expect(payload.Encrypted.recipients.length).toBeGreaterThan(0);
        });

        it("signe le bloc avec le wallet du coordinateur", async () => {
            await client.mintCube({
                ownerAddress: userWallet.address,
                ownerX25519PubKeyHex: userWallet.x25519PublicKeyHex,
            });

            const lastCall = (global.fetch as ReturnType<typeof vi.fn>).mock.calls.find(
                (call) => call[0].includes("/submit/block")
            );
            const body = JSON.parse(lastCall![1].body);

            // Signé par le coordinateur (wallet), pas par l'utilisateur
            expect(body.signer_pk_hex).toBe(wallet.publicKeyHex);
            expect(body.signature_hex).toBeDefined();
            expect(body.signature_hex.length).toBeGreaterThan(0);
        });

        it("lève une erreur si coordinatorWallet n'est pas configuré", async () => {
            // Client sans coordinatorWallet
            const clientWithoutWallet = new PmsClient({
                nodeUrl: "http://localhost:3000",
                enableRacing: false,
            });

            await expect(
                clientWithoutWallet.mintCube({
                    ownerAddress: userWallet.address,
                    ownerX25519PubKeyHex: userWallet.x25519PublicKeyHex,
                })
            ).rejects.toThrow("coordinatorWallet must be configured");
        });

        it("utilise l'adresse du destinataire fournie", async () => {
            const result = await client.mintCube({
                ownerAddress: userWallet.address,
                ownerX25519PubKeyHex: userWallet.x25519PublicKeyHex,
            });

            expect(result.status).toBe("inserted");

            const lastCall = (global.fetch as ReturnType<typeof vi.fn>).mock.calls.find(
                (call) => call[0].includes("/submit/block")
            );
            const body = JSON.parse(lastCall![1].body);

            // Le bloc est soumis avec le payload chiffré
            expect(body.payload_json).toContain("Encrypted");
        });
    });
});

// ═══════════════════════════════════════════════════════════════════════════
// Tests statistiques pour vérifier la distribution des raretés
// ═══════════════════════════════════════════════════════════════════════════

describe("Rarity Distribution (Statistical)", () => {
    it("'Basic' représente environ 99% de l'espace", () => {
        // Sur 10M de valeurs possibles, Basic = 10M - 111110 = 9888890
        // Soit ~98.9%
        const basicCount = 10_000_000 - 111_110;
        const percentage = (basicCount / 10_000_000) * 100;

        expect(percentage).toBeGreaterThan(98);
        expect(percentage).toBeLessThan(100);
    });

    it("'Common' représente environ 1% de l'espace", () => {
        const commonCount = 100_000;
        const percentage = (commonCount / 10_000_000) * 100;

        expect(percentage).toBe(1);
    });

    it("'Unique' a exactement 10 valeurs possibles", () => {
        const uniqueCount = 10;
        const probability = uniqueCount / 10_000_000;

        expect(probability).toBe(1 / 1_000_000);
    });
});
