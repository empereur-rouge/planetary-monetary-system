/**
 * Tests pour la méthode send() - transfert de tokens
 * 
 * Ces tests vérifient :
 * - La construction correcte des transactions UTXO
 * - Le calcul des frais (1%)
 * - La gestion du change
 * - La signature et soumission du bloc
 */

import { describe, it, expect, vi, beforeEach } from "vitest";
import { PmsClient } from "../src/client";
import { PmsWallet } from "../src/wallet";

describe("PmsClient.send()", () => {
    let client: PmsClient;
    let senderWallet: PmsWallet;
    let recipientWallet: PmsWallet;

    beforeEach(() => {
        senderWallet = PmsWallet.generate();
        recipientWallet = PmsWallet.generate();

        client = new PmsClient({
            nodeUrl: "http://localhost:3000",
            enableRacing: false,
        });

        // Mock fetch pour tous les endpoints nécessaires
        global.fetch = vi.fn().mockImplementation((url: string, init?: RequestInit) => {
            // Helper for mocking responses
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

            // Mock /v1/wallet/{address}/utxos
            if (url.includes("/utxos")) {
                return mockResponse({
                    utxos: [
                        {
                            outpoint: { block_id: "block1", output_index: 0 },
                            address: senderWallet.address,
                            amount: "100.00000000",
                        },
                    ],
                });
            }

            // Mock /submit/block
            if (url.includes("/submit/block")) {
                const body = JSON.parse(init?.body as string);
                return mockResponse({
                    status: "inserted",
                    block_id: body.id,
                });
            }

            // Mock /v1/config
            if (url.includes("/v1/config")) {
                return mockResponse({
                    fee_rate_bps: 100, // 1%
                    base_fee: "0.001",
                    min_pow_bits: 0,
                    mint_enabled: true,
                    fee_recipient: "pms1adminaddress",
                    platform_fee_bps: 2000,
                    node_fee_bps: 3000,
                    max_mint_per_block: 1000000,
                    updated_at_block: "0",
                    updated_at_timestamp: 0,
                });
            }

            return Promise.reject(new Error(`Unmocked URL: ${url}`));
        });
    });

    describe("transaction basique", () => {
        it("envoie des tokens à une adresse", async () => {
            const result = await client.send({
                to: recipientWallet.address,
                amount: "10.0",
                wallet: senderWallet,
            });

            expect(result.status).toBe("inserted");
            expect(result.block_id).toBeDefined();
        });

        it("construit un payload TxUtxo correct", async () => {
            await client.send({
                to: recipientWallet.address,
                amount: "10.0",
                wallet: senderWallet,
            });

            const submitCall = (global.fetch as ReturnType<typeof vi.fn>).mock.calls.find(
                (call) => call[0].includes("/submit/block")
            );
            expect(submitCall).toBeDefined();

            const body = JSON.parse(submitCall![1].body);
            const payload = JSON.parse(body.payload_json);

            expect(payload.Plain.TxUtxo).toBeDefined();
            expect(payload.Plain.TxUtxo.inputs).toHaveLength(1);
            expect(payload.Plain.TxUtxo.outputs.length).toBeGreaterThanOrEqual(1);
        });

        it("inclut le destinataire dans les outputs", async () => {
            await client.send({
                to: recipientWallet.address,
                amount: "10.0",
                wallet: senderWallet,
            });

            const submitCall = (global.fetch as ReturnType<typeof vi.fn>).mock.calls.find(
                (call) => call[0].includes("/submit/block")
            );
            const body = JSON.parse(submitCall![1].body);
            const payload = JSON.parse(body.payload_json);
            const outputs = payload.Plain.TxUtxo.outputs;

            const recipientOutput = outputs.find(
                (o: { address: string }) => o.address === recipientWallet.address
            );
            expect(recipientOutput).toBeDefined();
            expect(recipientOutput.amount).toBe("10.00000000");
        });
    });

    describe("calcul des frais", () => {
        it("applique des frais de 1%", async () => {
            await client.send({
                to: recipientWallet.address,
                amount: "10.0",
                wallet: senderWallet,
            });

            const submitCall = (global.fetch as ReturnType<typeof vi.fn>).mock.calls.find(
                (call) => call[0].includes("/submit/block")
            );
            const body = JSON.parse(submitCall![1].body);
            const payload = JSON.parse(body.payload_json);

            // 10.0 * 1% = 0.1 minimum
            const fee = payload.Plain.TxUtxo.fee;
            expect(parseFloat(fee)).toBeGreaterThanOrEqual(0.01);
        });
    });

    describe("gestion du change", () => {
        it("retourne le change au sender", async () => {
            // Sender a 100 PMS, envoie 10 + ~0.1 frais = 89.9 change
            await client.send({
                to: recipientWallet.address,
                amount: "10.0",
                wallet: senderWallet,
            });

            const submitCall = (global.fetch as ReturnType<typeof vi.fn>).mock.calls.find(
                (call) => call[0].includes("/submit/block")
            );
            const body = JSON.parse(submitCall![1].body);
            const payload = JSON.parse(body.payload_json);
            const outputs = payload.Plain.TxUtxo.outputs;

            const changeOutput = outputs.find(
                (o: { address: string }) => o.address === senderWallet.address
            );
            expect(changeOutput).toBeDefined();
            // 100 - 10 - 0.1 (fee) = 89.9
            expect(parseFloat(changeOutput.amount)).toBeCloseTo(89.9, 1);
        });
    });

    describe("signature", () => {
        it("signe le bloc avec le wallet fourni", async () => {
            await client.send({
                to: recipientWallet.address,
                amount: "10.0",
                wallet: senderWallet,
            });

            const submitCall = (global.fetch as ReturnType<typeof vi.fn>).mock.calls.find(
                (call) => call[0].includes("/submit/block")
            );
            const body = JSON.parse(submitCall![1].body);

            expect(body.signer_pk_hex).toBe(senderWallet.publicKeyHex);
            expect(body.signature_hex).toBeDefined();
            expect(body.signature_hex.length).toBeGreaterThan(0);
        });
    });

    describe("gestion des erreurs", () => {
        it("lève une erreur si pas d'UTXOs", async () => {
            // Mock avec UTXOs vides
            global.fetch = vi.fn().mockImplementation((url: string) => {
                if (url.includes("/utxos")) {
                    return Promise.resolve({
                        ok: true,
                        status: 200,
                        json: () => Promise.resolve({ utxos: [] }),
                        text: () => Promise.resolve(JSON.stringify({ utxos: [] })),
                    } as Response);
                }
                return Promise.reject(new Error(`Unmocked URL: ${url}`));
            });

            await expect(
                client.send({
                    to: recipientWallet.address,
                    amount: "10.0",
                    wallet: senderWallet,
                })
            ).rejects.toThrow("No UTXOs available");
        });

        it("lève une erreur si balance insuffisante", async () => {
            // Mock avec UTXO de seulement 5 PMS
            global.fetch = vi.fn().mockImplementation((url: string) => {
                if (url.includes("/utxos")) {
                    return Promise.resolve({
                        ok: true,
                        json: () => Promise.resolve({
                            utxos: [{
                                outpoint: { block_id: "block1", output_index: 0 },
                                address: senderWallet.address,
                                amount: "5.00000000",
                            }],
                        }),
                        text: () => Promise.resolve(JSON.stringify({
                            utxos: [{
                                outpoint: { block_id: "block1", output_index: 0 },
                                address: senderWallet.address,
                                amount: "5.00000000",
                            }],
                        })),
                    } as Response);
                }

                // Mock /v1/config
                if (url.includes("/v1/config")) {
                    return Promise.resolve({
                        ok: true,
                        json: () => Promise.resolve({
                            fee_rate_bps: 100,
                            base_fee: "0.001",
                            min_pow_bits: 0,
                            mint_enabled: true,
                            fee_recipient: "pms1adminaddress",
                        }),
                        text: () => Promise.resolve(JSON.stringify({
                            fee_rate_bps: 100,
                            base_fee: "0.001",
                            fee_recipient: "pms1adminaddress",
                        })),
                    } as Response);
                }
                return Promise.reject(new Error(`Unmocked URL: ${url}`));
            });

            await expect(
                client.send({
                    to: recipientWallet.address,
                    amount: "10.0",
                    wallet: senderWallet,
                })
            ).rejects.toThrow("Insufficient balance");
        });
    });
});
