use async_trait::async_trait;
use pms_server::Server;
use pms_storage::store::PutResult;
#[allow(unused_imports)]
use pms_wallet::Wallet;
use pms_wire::WireBlock;
use std::collections::{HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

// Adapter Mock that tracks calls
struct MockAdapter {
    has: Mutex<HashSet<String>>,
    checked_ids: Mutex<HashSet<String>>, // IDs passed to have_block
    persisted_ids: Mutex<Vec<String>>,
}

impl MockAdapter {
    fn new() -> Self {
        Self {
            has: Mutex::new(HashSet::new()),
            checked_ids: Mutex::new(HashSet::new()),
            persisted_ids: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl pms_interface::NetDagAdapter for MockAdapter {
    async fn have_block(&self, id: &str) -> bool {
        // Use std::sync::Mutex locking
        self.checked_ids.lock().unwrap().insert(id.to_string());
        self.has.lock().unwrap().contains(id)
    }
    async fn persist_block(&self, b: &WireBlock) -> anyhow::Result<PutResult> {
        self.persisted_ids.lock().unwrap().push(b.id.clone());
        Ok(PutResult::Inserted)
    }
    async fn broadcast_block(&self, _b: &WireBlock) -> anyhow::Result<()> {
        Ok(())
    }
    async fn top_tips(&self, _l: usize) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }
    async fn get_block(&self, _id: &str) -> anyhow::Result<Option<WireBlock>> {
        Ok(None)
    }
    async fn recent_ids(&self, _l: usize) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }
    async fn get_blocks_by_ids(&self, _ids: &[String]) -> anyhow::Result<Vec<WireBlock>> {
        Ok(vec![])
    }
    fn min_pow_leading_zero_bits(&self) -> u8 {
        0
    }
    async fn circulating_supply(&self) -> (rust_decimal::Decimal, u64) {
        (rust_decimal::Decimal::ZERO, 0)
    }
    async fn circulating_supply_by_asset(
        &self,
        _asset_id: Option<&str>,
    ) -> (rust_decimal::Decimal, u64) {
        (rust_decimal::Decimal::ZERO, 0)
    }
    async fn balance_by_address(&self, _address: &str) -> rust_decimal::Decimal {
        rust_decimal::Decimal::ZERO
    }
    async fn utxos_by_address(
        &self,
        _address: &str,
    ) -> Vec<(pms_types::OutputId, pms_types::TxOutput)> {
        Vec::new()
    }
    async fn add_utxo(
        &self,
        _txid: String,
        _index: u32,
        _address: String,
        _amount: String,
        _asset_id: Option<String>,
    ) {
        // Mock: no-op
    }
    async fn remove_utxo(&self, _output_id: &pms_types::OutputId) -> bool {
        false // Mock: no-op
    }
    async fn get_utxo(&self, _output_id: &pms_types::OutputId) -> Option<pms_types::TxOutput> {
        None // Mock: no UTXOs stored
    }
}

#[tokio::test]
async fn test_parallel_orphan_fetch_logic() {
    // 1. Setup Server with Mock Adapter
    let adapter = Arc::new(MockAdapter::new());
    let wallet = Arc::new(Wallet::generate());

    // We instantiate Server but do NOT run the network layer.
    // We only access: server.process_incoming_blocks()
    let server = Server::new(
        adapter.clone(),
        "testnet",
        1,
        wallet,
        &pms_config::P2pConfig::default(),
        None,
    );

    // Create Block with 2 missing parents
    let p1 = "00000000000000000000000000000000000000000000000000000000000000A1".to_string();
    let p2 = "00000000000000000000000000000000000000000000000000000000000000A2".to_string();

    let wb = WireBlock {
        id: "C1".to_string(),
        parents: vec![p1.clone(), p2.clone()],
        payload_json: None,
        nonce: 0,
        network_id: "testnet".into(),
        protocol_version: 1,
        signer_pk_hex: "00".into(),
        signature_hex: "00".into(),
        metadata: None,
    };

    // 2. Feed the block to the server logic directly
    // Using an arbitrary address since we don't have a real peer connected (unicast will fail silently)
    let peer_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
    let mut queue = VecDeque::new();
    queue.push_back(wb);

    server.process_incoming_blocks(queue, peer_addr).await;

    // 3. Verify Optimized Behavior
    // Correct behavior:
    // - It checked internal caches (skipped)
    // - It checked adapter.have_block(C1) (skipped)
    // - It iterated parents and checked adapter.have_block(P1) AND adapter.have_block(P2).
    // - It detected missing parents and ABORTED persist_block.

    let checked = adapter.checked_ids.lock().unwrap();
    let persisted = adapter.persisted_ids.lock().unwrap();

    println!("Checked IDs: {:?}", checked);
    println!("Persisted IDs: {:?}", persisted);

    // Assertions
    assert!(checked.contains("C1"), "Should check have_block(C1)");
    assert!(checked.contains(&p1), "Should check have_block(P1)");
    assert!(checked.contains(&p2), "Should check have_block(P2)");

    assert!(
        persisted.is_empty(),
        "Should NOT persist block C1 if parents are missing"
    );

    println!("✅ Test Passed: Parallel existence check logic verified.");
}
