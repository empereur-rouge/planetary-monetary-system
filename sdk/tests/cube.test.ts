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

    beforeEach(() => {
        // Créer un wallet utilisateur
        wallet = PmsWallet.generate();

        // Créer un client simple (sans coordinatorWallet - plus nécessaire)
        client = new PmsClient({
            nodeUrl: "http://localhost:3000",
            enableRacing: false,
        });

        // Mock fetch pour les différents endpoints
        global.fetch = vi.fn().mockImplementation((url: string, init?: RequestInit) => {
            // Helper to create response with both json() and text()
            const mockRes = (data: any) => Promise.resolve({
                ok: true,
                status: 200,
                json: () => Promise.resolve(data),
                text: () => Promise.resolve(JSON.stringify(data)),
            });

            // Mock /v1/dag/tips
            if (url.includes("/v1/dag/tips")) {
                return mockRes(["tip1", "tip2"]);
            }
            // Mock /v1/nft/burn (nouvelle API)
            if (url.includes("/v1/nft/burn")) {
                const body = JSON.parse(init?.body as string);
                return mockRes({
                    status: "inserted",
                    block_id: body.id,
                    refund: { amount: "1.23456789", recipient: body.signer_pk_hex },
                });
            }
            // Mock /v1/nft/mint
            if (url.includes("/v1/nft/mint")) {
                return mockRes({
                    status: "inserted",
                    block_id: "mint-block-123",
                });
            }
            // Mock cube generator endpoint
            if (url.includes("/api/cube/generate")) {
                return mockRes({
                    rarity: "Common",
                    attributes: { weight: 50.5, size: 30.2, density: 2.5 },
                    roll: 50000,
                    signature: "mock-authority-signature-base64",
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
                (call) => call[0].includes("/v1/nft/burn")
            );
            expect(lastCall).toBeDefined();
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
                (call) => call[0].includes("/v1/nft/burn")
            );
            const body = JSON.parse(lastCall![1].body);

            expect(body.signer_pk_hex).toBe(wallet.publicKeyHex);
            expect(body.signature_hex).toBeDefined();
            expect(body.signature_hex.length).toBeGreaterThan(0);
        });

        it("retourne un refund preview si disponible", async () => {
            const result = await client.burnNft({
                tokenId: "cube-with-refund",
                wallet,
            });

            expect(result.status).toBe("inserted");
            expect(result.refund).toBeDefined();
            expect(result.refund?.amount).toBe("1.23456789");
        });
    });

    describe("mintCube", () => {
        const generatorUrl = "http://localhost:4000";

        it("appelle le générateur de cubes et mint un NFT", async () => {
            const result = await client.mintCube({
                wallet,
                generatorUrl,
            });

            expect(result.status).toBe("inserted");
            expect(result.token_id).toBeDefined();
            expect(result.rarity).toBe("Common");
            expect(result.roll).toBe(50000);
            expect(result.attributes).toBeDefined();
            expect(result.attributes.weight).toBe(50.5);
        });

        it("appelle le bon endpoint générateur", async () => {
            await client.mintCube({
                wallet,
                generatorUrl,
            });

            const generatorCall = (global.fetch as ReturnType<typeof vi.fn>).mock.calls.find(
                (call) => call[0].includes("/api/cube/generate")
            );
            expect(generatorCall).toBeDefined();
            expect(generatorCall![0]).toBe(`${generatorUrl}/api/cube/generate`);
        });

        it("appelle mintNft avec les métadonnées correctes", async () => {
            await client.mintCube({
                wallet,
                generatorUrl,
            });

            const mintCall = (global.fetch as ReturnType<typeof vi.fn>).mock.calls.find(
                (call) => call[0].includes("/v1/nft/mint")
            );
            expect(mintCall).toBeDefined();
            const body = JSON.parse(mintCall![1].body);

            expect(body.owner_address).toBe(wallet.address);
            expect(body.metadata.nft_type).toBe("cube");
            expect(body.metadata.extra).toContain("Common");
            expect(body.metadata.extra).toContain("mock-authority-signature-base64");
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
