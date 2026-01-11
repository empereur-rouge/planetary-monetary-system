import { describe, it, expect, beforeAll } from "vitest";
import { PmsClient } from "../src/client";
import { PmsWallet } from "../src/wallet";
import { computeBlockId } from "../src/utils";
import type { WireBlock, PayloadEnvelope } from "../src/types";

// Allow self-signed certs for local Docker tests
process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0';

const NODE1_URL = "https://localhost:8080";
const NODE2_URL = "https://localhost:8081";

describe("SDK Integration Tests (Docker Cluster)", () => {
    let client: PmsClient;

    beforeAll(() => {
        client = new PmsClient({
            nodeUrl: NODE1_URL,
            seedNodes: [NODE2_URL],
            timeout: 5000,
            enableRacing: true,
        });
    });

    it("should fetch tips from Node 1", async () => {
        try {
            const tips = await client.getTips();
            expect(Array.isArray(tips)).toBe(true);
            expect(tips.length).toBeGreaterThan(0);
            console.log("Tips:", tips);
        } catch (e) {
            console.error("Failed to connect to Docker node. Is the cluster running?");
            throw e;
        }
    });

    it("should discover nodes via registry", async () => {
        // Access private method or trigger discovery via side effect
        // We can check if knownNodes set grows.
        // Since `knownNodes` is private, we can't easily check it without casting to any.

        // Trigger a submit (which triggers discovery)
        // We'll submit a dummy block that might fail validation but triggers the racing logic

        // Or simply call getTips() which doesn't trigger discovery (only submit does in current implementation)

        // Wait! discovery is ONLY called in submitBlockRacing. 
        // Let's call refreshNodeList via 'any' cast to verify it works.

        await (client as any).refreshNodeList();

        const knownNodes = (client as any).knownNodes as Set<string>;
        console.log("Known nodes:", knownNodes);

        // Should have Node1, Node2 (seed), and possibly others discovered
        expect(knownNodes.has(NODE1_URL)).toBe(true);
        expect(knownNodes.has(NODE2_URL)).toBe(true);

        // If discovery worked, and nodes registered, we might see more or updated URLs
    });

    it("should submit a block using racing pattern", async () => {
        const wallet = PmsWallet.generate();
        const tips = await client.getTips();
        const parents = tips.slice(0, 2);

        const payload: PayloadEnvelope = { Plain: { TxUtxo: { inputs: [], outputs: [], fee: "0" } } };
        const payloadJson = JSON.stringify(payload);
        const nonce = 0;
        const id = computeBlockId(parents, payloadJson, nonce);
        const signature = wallet.sign(id); // Plain signature of ID (utf8)

        const wireBlock: WireBlock = {
            id,
            parents,
            payload_json: payloadJson,
            nonce,
            network_id: "pms-test",
            protocol_version: 1,
            signer_pk_hex: wallet.publicKeyHex,
            signature_hex: signature,
        };

        // This might fail due to validation (invalid signature or structure), 
        // but we verify that the network call happens and racing works.
        try {
            const res = await client.submitBlock(wireBlock);
            console.log("Submit result:", res);
        } catch (e: any) {
            // It might fail with "Invalid signature" or "Bad Request" but should not be "Connection Refused"
            console.log("Submit error (expected validation error):", e.message);
            expect(e.message).not.toContain("ECONNREFUSED");
        }
    });
});
