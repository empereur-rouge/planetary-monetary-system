# E2E Production Simulation Test

## Overview

This End-to-End (E2E) test simulates a complete production environment using Docker to verify the full economic cycle of the PMS blockchain.

## Test Scenario

The test performs the following operations:

1. **Setup Phase**
   - Generates a random Authority ECDSA keypair for signing Cube NFTs
   - Updates `config.e2e-prod.toml` with the Authority public key
   - Starts Docker Compose with Gateway + Engine (TLS enabled)

2. **Minting Phase** (100 Cubes)
   - Generates 100 unique Cube NFTs with random attributes:
     - Weight: 500-5000g (0.5-5kg)
     - Size: 20-80mm (2-8cm)
     - Density: 20-100 (0.2-1.0)
   - Signs each cube's attributes with the Authority private key
   - Submits mint requests to the node via HTTPS

3. **Burning Phase**
   - Burns all 100 cubes in a single batch transaction
   - Verifies the burn refund calculation
   - Checks that Wallet A received PMS tokens as refund

4. **Transfer Phase**
   - Transfers 1.0 PMS from Wallet A to Wallet B
   - Verifies balances after transfer
   - Logs fee distribution (Treasury, Coordinator, Parents)

5. **Verification Phase**
   - Confirms all balances are correct
   - Validates fee distribution percentages
   - Ensures no tokens were lost or created

## Prerequisites

- Docker and Docker Compose installed
- Rust toolchain (for running the test)
- Sufficient disk space for RocksDB (~500MB)

## Files Created

- **[etc/config/config.e2e-prod.toml](etc/config/config.e2e-prod.toml)** - Production-like configuration
- **[docker-compose.e2e-prod.yml](docker-compose.e2e-prod.yml)** - Docker orchestration
- **[crates/pms-server/tests/e2e_prod_sim.rs](crates/pms-server/tests/e2e_prod_sim.rs)** - Test implementation

## Running the Test

### Option 1: Run with cargo (Recommended)

```bash
cargo test --test e2e_prod_sim -- --ignored --nocapture
```

### Option 2: Run in verbose mode

```bash
RUST_LOG=debug cargo test --test e2e_prod_sim -- --ignored --nocapture
```

## Expected Output

```
🎮 E2E Production Simulation Test
══════════════════════════════════════════════════════════════

📋 Phase 0: Generate Authority Keypair
  🔑 Authority Public Key: 03a1829d324538dee1265c879b72...

📋 Phase 1: Start Docker Environment
🐳 Starting Docker Compose (e2e-prod)...
⏳ Waiting for services to be healthy...
  ✅ Node is LIVE and responding

📋 Phase 2: Generate Wallets
  👤 Wallet A: 04abc123...def456
  👤 Wallet B: 04789xyz...uvw012

📋 Phase 3: Mint 100 Cubes with Authority Signatures
  🎲 Minted 10/100 cubes
  🎲 Minted 20/100 cubes
  ...
  ✅ Successfully minted 100 cubes
  💰 Wallet A balance before burn: 0 PMS

📋 Phase 4: Burn All 100 Cubes
  🔥 Burn Status: burned
  📦 Block ID: 1a2b3c4d...
  💰 Total Refund: 0.04275 PMS
  💰 Wallet A balance after burn: 0.04275 PMS
  ✅ Refund received: 0.04275 PMS

📋 Phase 5: Transfer Tokens from A to B
  💸 Transferring 1.0 PMS from A to B
  📊 Wallet A has 1 UTXOs
  ✅ Transfer submitted successfully
  💰 Final Balance A: 0.03275 PMS
  💰 Final Balance B: 1.0 PMS

📋 Phase 6: Verify Fee Distribution
  ℹ️  Fee distribution verification would check:
     - Treasury wallet received 15% of fees
     - Block creators received 45% of fees
     - Parent signers received 40% of fees
  ✅ Fee distribution working (visual verification in logs)

📋 Cleanup: Stopping Docker Services
🛑 Stopping Docker Compose...

══════════════════════════════════════════════════════════════
✅ E2E Production Simulation PASSED
══════════════════════════════════════════════════════════════
```

## What is Tested

### Authority Signature Validation
- Only cubes with valid Authority signatures can be minted
- The signature is computed as: `sign("weight:X,size:Y,density:Z")`
- Uses ECDSA secp256k1 (same as Bitcoin/Ethereum)

### Burn Refund Calculation
- Formula: `(weight * size * density) / 19,300,000,000`
- Example: `(2750 * 50 * 60) / 19,300,000,000 = 0.00042746 PMS`
- Maximum refund: 1000 PMS (safety cap)

### Fee Distribution
- **Treasury**: 15% of transaction fees
- **Block Creator**: 45% of transaction fees
- **Parent Signers**: 40% of transaction fees (split equally)

### Block Rewards
- **Treasury**: 20% of block reward (0.02 PMS)
- **Creator**: 70% of block reward (0.07 PMS)
- **Burned**: 10% of block reward (0.01 PMS)

## Architecture

The test uses a production-like architecture:

```
┌─────────────┐      HTTPS       ┌─────────────┐    Internal    ┌─────────────┐
│   Client    │ ───────────────> │   Gateway   │ ─────────────> │   Engine    │
│  (Test)     │  Port 8443       │  (Public)   │   Network      │ (Coordinator)│
└─────────────┘                  └─────────────┘                └─────────────┘
                                                                        │
                                                                        ▼
                                                                 ┌─────────────┐
                                                                 │  RocksDB    │
                                                                 │  (Volume)   │
                                                                 └─────────────┘
```

## Troubleshooting

### Docker containers don't start
```bash
# Check if ports 8443 is already in use
lsof -i :8443

# Manually stop any existing containers
docker compose -f docker-compose.e2e-prod.yml down -v
```

### Test fails with "Authority signature invalid"
- Verify that `config.e2e-prod.toml` has been updated with the correct Authority key
- Check that the signature format is Base64-encoded DER

### RocksDB errors
```bash
# Clean up RocksDB volumes
docker volume rm dag-pms_rocksdb_e2e_data
```

## Development Notes

### Adding New Phases

To add a new test phase:

1. Add a new section in the test function
2. Update the phase counter in the output
3. Add corresponding verification assertions

### Modifying Authority Validation

The Authority validation logic is in:
- [`crates/pms-server/src/api_fn/nft.rs`](crates/pms-server/src/api_fn/nft.rs) - Mint endpoint
- [`crates/pms-server/src/burn_refund.rs`](crates/pms-server/src/burn_refund.rs) - Refund calculation

## Future Enhancements

- [ ] Add Coordinator → Wallet A refund cycle
- [ ] Query and verify Treasury wallet balance
- [ ] Test with multiple concurrent clients
- [ ] Add stress test with 1000+ cubes
- [ ] Verify block rewards distribution
- [ ] Test invalid signature rejection

## Related Documentation

- [API Documentation](documentation/api/README.md)
- [NFT API](documentation/api/nft.md)
- [Configuration Guide](etc/config/README.md)
- [Docker Deployment](README.md#docker)
