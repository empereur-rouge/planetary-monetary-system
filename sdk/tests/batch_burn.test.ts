import { describe, it, expect, vi, beforeEach } from "vitest";
import { PmsClient } from "../src/client";
import { PmsWallet } from "../src/wallet";

describe("PmsClient.burnNfts()", () => {
    let client: PmsClient;
    let wallet: PmsWallet;

    beforeEach(() => {
        wallet = PmsWallet.generate();
        client = new PmsClient({
            nodeUrl: "http://localhost:3000",
            enableRacing: false,
        });

        // Mock fetch
        global.fetch = vi.fn().mockImplementation((url: string, init?: RequestInit) => {
            const mockResponse = (data: any) => Promise.resolve({
                ok: true,
                status: 200,
                json: () => Promise.resolve(data),
                text: () => Promise.resolve(JSON.stringify(data)),
            } as Response);

            // Mock /v1/dag/tips
            if (url.includes("/v1/dag/tips")) {
                return mockResponse(["tip1", "tip2"]);
            }

            // Mock /v1/nft/burn
            if (url.includes("/v1/nft/burn")) {
                const body = JSON.parse(init?.body as string);
                return mockResponse({
                    status: "burned",
                    block_id: body.id,
                    token_id: "batch",
                    token_ids: JSON.parse(body.payload_json).Plain.Nft.BatchBurn.token_ids,
                    refund: {
                        amount: "0.00010000",
                        recipient: wallet.address,
                    }
                });
            }

            return Promise.reject(new Error(`Unmocked URL: ${url}`));
        });
    });

    it("constructs a correct BatchBurn payload", async () => {
        const tokenIds = ["token1", "token2", "token3"];

        const result = await client.burnNfts({
            tokenIds,
            wallet,
        });

        const fetchCalls = (global.fetch as ReturnType<typeof vi.fn>).mock.calls;
        const burnCall = fetchCalls.find(call => call[0].includes("/v1/nft/burn"));

        expect(burnCall).toBeDefined();

        const body = JSON.parse(burnCall![1].body);
        const payload = JSON.parse(body.payload_json);

        expect(payload.Plain.Nft.BatchBurn).toBeDefined();
        expect(payload.Plain.Nft.BatchBurn.token_ids).toEqual(tokenIds);
        expect(payload.Plain.Nft.BatchBurn.burner).toBe(wallet.address);

        expect(result.status).toBe("burned");
        expect(result.token_ids).toEqual(tokenIds);
    });

    it("throws error if tokenIds list is empty", async () => {
        await expect(client.burnNfts({
            tokenIds: [],
            wallet,
        })).rejects.toThrow("No token IDs provided");
    });
});
