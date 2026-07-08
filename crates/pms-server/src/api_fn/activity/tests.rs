use super::*;
use super::classify::{classify_activity, classify_activity_sync, is_fee_output_only, parse_type_filter};
use pms_types::{OutputId, Transaction, TxInput, TxOutput};
use pms_types_nft::NftAction;
use pms_types_payload::{PlainPayload, TokenMetadata};

// ── helpers ──────────────────────────────────────────────────────

fn out(addr: &str, amount: &str) -> TxOutput {
    TxOutput::new(addr, amount, None)
}

fn out_asset(addr: &str, amount: &str, asset: &str) -> TxOutput {
    TxOutput::new(addr, amount, Some(asset.to_string()))
}

// ── parse_type_filter ────────────────────────────────────────────

#[test]
fn parse_type_filter_none() {
    assert!(parse_type_filter(&None).is_empty());
}

#[test]
fn parse_type_filter_empty() {
    assert!(parse_type_filter(&Some(String::new())).is_empty());
}

#[test]
fn parse_type_filter_single() {
    let s = Some("fee_received".into());
    let f = parse_type_filter(&s);
    assert_eq!(f, vec!["fee_received"]);
}

#[test]
fn parse_type_filter_multi() {
    let s = Some("fee_received, transfer_in , mint".into());
    let f = parse_type_filter(&s);
    assert_eq!(f, vec!["fee_received", "transfer_in", "mint"]);
}

// ── classify_activity_sync: Mint ─────────────────────────────────

#[test]
fn classify_mint_for_recipient() {
    let plain = PlainPayload::Mint {
        outputs: vec![out("alice", "100"), out("bob", "50")],
    };
    let items = classify_activity_sync(&plain, "alice");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "mint");
    assert_eq!(items[0].direction, "in");
    assert_eq!(items[0].amount.as_deref(), Some("100"));
}

#[test]
fn classify_mint_not_involved() {
    let plain = PlainPayload::Mint {
        outputs: vec![out("alice", "100")],
    };
    let items = classify_activity_sync(&plain, "carol");
    assert!(items.is_empty());
}

// ── classify_activity_sync: TxUtxo (outputs-only in sync) ───────

#[test]
fn classify_tx_receiver() {
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("bob", "50"), out("alice", "30")],
        fee: "1".into(),
        unlocks: vec![],
    };
    let items = classify_activity_sync(&PlainPayload::TxUtxo(tx), "alice");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "transfer_in");
    assert_eq!(items[0].direction, "in");
    assert_eq!(items[0].amount.as_deref(), Some("30"));
}

#[test]
fn classify_tx_not_in_outputs() {
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("bob", "50")],
        fee: "1".into(),
        unlocks: vec![],
    };
    // In sync mode, sender isn't resolved, so if addr not in outputs → empty
    let items = classify_activity_sync(&PlainPayload::TxUtxo(tx), "alice");
    assert!(items.is_empty());
}

// ── classify_activity_sync: Reward ───────────────────────────────

#[test]
fn classify_fee_received() {
    let plain = PlainPayload::Reward {
        fee_outputs: vec![out("coordinator", "1.95")],
        reward_outputs: vec![],
        burned: "0.05".into(),
        tx_block_id: "txblk1".into(),
    };
    let items = classify_activity_sync(&plain, "coordinator");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "fee_received");
    assert_eq!(items[0].direction, "in");
    assert_eq!(items[0].amount.as_deref(), Some("1.95"));
}

#[test]
fn classify_reward_received() {
    let plain = PlainPayload::Reward {
        fee_outputs: vec![],
        reward_outputs: vec![out("treasury", "10")],
        burned: "0".into(),
        tx_block_id: "txblk2".into(),
    };
    let items = classify_activity_sync(&plain, "treasury");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "reward");
    assert_eq!(items[0].direction, "in");
    assert_eq!(items[0].amount.as_deref(), Some("10"));
}

#[test]
fn classify_reward_both_fee_and_reward() {
    let plain = PlainPayload::Reward {
        fee_outputs: vec![out("addr", "5")],
        reward_outputs: vec![out("addr", "10")],
        burned: "0".into(),
        tx_block_id: "txblk3".into(),
    };
    let items = classify_activity_sync(&plain, "addr");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].activity_type, "fee_received");
    assert_eq!(items[1].activity_type, "reward");
}

#[test]
fn classify_reward_not_involved() {
    let plain = PlainPayload::Reward {
        fee_outputs: vec![out("coordinator", "5")],
        reward_outputs: vec![],
        burned: "0".into(),
        tx_block_id: "txblk4".into(),
    };
    let items = classify_activity_sync(&plain, "random_addr");
    assert!(items.is_empty());
}

// ── classify_activity_sync: NFT ──────────────────────────────────

#[test]
fn classify_nft_mint() {
    let plain = PlainPayload::Nft(NftAction::Mint {
        token_id: "nft-001".into(),
        creator: "alice".into(),
        metadata: Default::default(),
    });
    let items = classify_activity_sync(&plain, "alice");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "nft_mint");
    assert_eq!(items[0].direction, "in");
    assert!(items[0].amount.is_none());
}

#[test]
fn classify_nft_mint_not_creator() {
    let plain = PlainPayload::Nft(NftAction::Mint {
        token_id: "nft-001".into(),
        creator: "alice".into(),
        metadata: Default::default(),
    });
    let items = classify_activity_sync(&plain, "bob");
    assert!(items.is_empty());
}

#[test]
fn classify_nft_transfer_in() {
    let plain = PlainPayload::Nft(NftAction::Transfer {
        token_id: "nft-001".into(),
        from: "alice".into(),
        to: "bob".into(),
        new_owner_x25519_pubkey: None,
        encrypted_metadata: None,
    });
    let items = classify_activity_sync(&plain, "bob");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "nft_transfer_in");
    assert_eq!(items[0].direction, "in");
    assert_eq!(items[0].counterparty.as_deref(), Some("alice"));
}

#[test]
fn classify_nft_transfer_out() {
    let plain = PlainPayload::Nft(NftAction::Transfer {
        token_id: "nft-001".into(),
        from: "alice".into(),
        to: "bob".into(),
        new_owner_x25519_pubkey: None,
        encrypted_metadata: None,
    });
    let items = classify_activity_sync(&plain, "alice");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "nft_transfer_out");
    assert_eq!(items[0].direction, "out");
    assert_eq!(items[0].counterparty.as_deref(), Some("bob"));
}

#[test]
fn classify_nft_burn() {
    let plain = PlainPayload::Nft(NftAction::Burn {
        token_id: "nft-001".into(),
        burner: "alice".into(),
    });
    let items = classify_activity_sync(&plain, "alice");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "nft_burn");
    assert_eq!(items[0].direction, "out");
}

#[test]
fn classify_nft_batch_burn() {
    let plain = PlainPayload::Nft(NftAction::BatchBurn {
        token_ids: vec!["nft-001".into(), "nft-002".into()],
        burner: "alice".into(),
    });
    let items = classify_activity_sync(&plain, "alice");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "nft_burn");
}

#[test]
fn classify_nft_use() {
    let plain = PlainPayload::Nft(NftAction::Use {
        token_id: "nft-001".into(),
        user: "alice".into(),
        action_type: "redeem".into(),
        action_data: None,
    });
    let items = classify_activity_sync(&plain, "alice");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "nft_use");
    assert_eq!(items[0].direction, "info");
}

// ── classify_activity_sync: TokenCreate ──────────────────────────

#[test]
fn classify_token_create() {
    let plain = PlainPayload::TokenCreate(TokenMetadata {
        asset_id: "edenite".into(),
        symbol: "EDEN".into(),
        name: "Edenite".into(),
        decimals: 8,
        max_supply: None,
        creator: "alice".into(),
        mint_authority: "alice".into(),
        demurrage_bps_per_day: None,
        collateral_address: None,
        collateral_asset_id: None,
        collateral_ratio_bps: None,
        royalty_bps: None,
        royalty_beneficiary: None,
        royalty_version: 0,
    });
    let items = classify_activity_sync(&plain, "alice");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "token_create");
    assert_eq!(items[0].direction, "info");
    assert_eq!(items[0].asset_id.as_deref(), Some("edenite"));
}

#[test]
fn classify_token_create_not_creator() {
    let plain = PlainPayload::TokenCreate(TokenMetadata {
        asset_id: "edenite".into(),
        symbol: "EDEN".into(),
        name: "Edenite".into(),
        decimals: 8,
        max_supply: None,
        creator: "alice".into(),
        mint_authority: "alice".into(),
        demurrage_bps_per_day: None,
        collateral_address: None,
        collateral_asset_id: None,
        collateral_ratio_bps: None,
        royalty_bps: None,
        royalty_beneficiary: None,
        royalty_version: 0,
    });
    let items = classify_activity_sync(&plain, "bob");
    assert!(items.is_empty());
}

// ── classify_activity_sync: Compliance ───────────────────────────

#[test]
fn classify_freeze() {
    let plain = PlainPayload::Freeze {
        address: "alice".into(),
        reason: "suspicious".into(),
    };
    let items = classify_activity_sync(&plain, "alice");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "freeze");
    assert_eq!(items[0].direction, "info");
}

#[test]
fn classify_freeze_not_target() {
    let plain = PlainPayload::Freeze {
        address: "alice".into(),
        reason: "suspicious".into(),
    };
    let items = classify_activity_sync(&plain, "bob");
    assert!(items.is_empty());
}

#[test]
fn classify_unfreeze() {
    let plain = PlainPayload::Unfreeze {
        address: "alice".into(),
        reason: "cleared".into(),
        freeze_block_id: "blk0".into(),
    };
    let items = classify_activity_sync(&plain, "alice");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "unfreeze");
    assert_eq!(items[0].direction, "info");
}

#[test]
fn classify_seized_from_target() {
    let plain = PlainPayload::Seize {
        from_address: "alice".into(),
        inputs: vec![],
        outputs: vec![out("treasury", "1000")],
        reason: "court order".into(),
    };
    let items = classify_activity_sync(&plain, "alice");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "seized");
    assert_eq!(items[0].direction, "out");
    assert_eq!(items[0].amount.as_deref(), Some("1000"));
}

#[test]
fn classify_seize_received() {
    let plain = PlainPayload::Seize {
        from_address: "alice".into(),
        inputs: vec![],
        outputs: vec![out("treasury", "1000")],
        reason: "court order".into(),
    };
    let items = classify_activity_sync(&plain, "treasury");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "seize_received");
    assert_eq!(items[0].direction, "in");
    assert_eq!(items[0].amount.as_deref(), Some("1000"));
    assert_eq!(items[0].counterparty.as_deref(), Some("alice"));
}

#[test]
fn classify_reverse_received() {
    let plain = PlainPayload::Reverse {
        original_block_id: "blk1".into(),
        inputs: vec![],
        outputs: vec![out("alice", "500")],
        reason: "fraud".into(),
    };
    let items = classify_activity_sync(&plain, "alice");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "reverse_received");
    assert_eq!(items[0].direction, "in");
    assert_eq!(items[0].amount.as_deref(), Some("500"));
}

#[test]
fn classify_reverse_not_involved() {
    let plain = PlainPayload::Reverse {
        original_block_id: "blk1".into(),
        inputs: vec![],
        outputs: vec![out("alice", "500")],
        reason: "fraud".into(),
    };
    let items = classify_activity_sync(&plain, "bob");
    assert!(items.is_empty());
}

// ── classify_activity_sync: Bridge ───────────────────────────────

#[test]
fn classify_bridge_lock_in() {
    let plain = PlainPayload::BridgeLock {
        inputs: vec![],
        dest_address: "alice".into(),
        amount: "250".into(),
        asset_id: Some("edenite".into()),
        dest_ledger_id: "side".into(),
    };
    let items = classify_activity_sync(&plain, "alice");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "bridge_lock_in");
    assert_eq!(items[0].direction, "in");
    assert_eq!(items[0].amount.as_deref(), Some("250"));
    assert_eq!(items[0].asset_id.as_deref(), Some("edenite"));
}

#[test]
fn classify_bridge_lock_not_dest() {
    let plain = PlainPayload::BridgeLock {
        inputs: vec![],
        dest_address: "alice".into(),
        amount: "250".into(),
        asset_id: None,
        dest_ledger_id: "side".into(),
    };
    let items = classify_activity_sync(&plain, "bob");
    assert!(items.is_empty());
}

#[test]
fn classify_bridge_mint() {
    let plain = PlainPayload::BridgeMint {
        outputs: vec![out("alice", "250")],
        lock_block_id: "lock1".into(),
        source_ledger_id: "main".into(),
    };
    let items = classify_activity_sync(&plain, "alice");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "bridge_mint");
    assert_eq!(items[0].direction, "in");
    assert_eq!(items[0].amount.as_deref(), Some("250"));
}

// ── is_fee_output_only ────────────────────────────────────────────

#[test]
fn fee_output_only_detects_fee_collector() {
    // Typical TxUtxo: sender→receiver(90) + fee_collector(1), fee=1
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("receiver", "90"), out("fee_collector", "1")],
        fee: "1".into(),
        unlocks: vec![],
    };
    println!("tx.fee={}, outputs={:?}", tx.fee, tx.outputs.iter().map(|o| (&o.address, &o.amount)).collect::<Vec<_>>());
    println!("is_fee_output_only(tx, 'fee_collector') = {}", is_fee_output_only(&tx, "fee_collector"));
    println!("is_fee_output_only(tx, 'receiver') = {}", is_fee_output_only(&tx, "receiver"));

    assert!(is_fee_output_only(&tx, "fee_collector"));
    assert!(!is_fee_output_only(&tx, "receiver"));
}

#[test]
fn fee_output_only_false_when_zero_fee() {
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("alice", "100")],
        fee: "0".into(),
        unlocks: vec![],
    };
    println!("tx.fee={}, is_fee_output_only='alice': {}", tx.fee, is_fee_output_only(&tx, "alice"));
    assert!(!is_fee_output_only(&tx, "alice"));
}

#[test]
fn fee_output_only_false_when_amount_differs() {
    // Fee is 1, but fee_collector receives 2 — NOT a pure fee output
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("receiver", "90"), out("fee_collector", "2")],
        fee: "1".into(),
        unlocks: vec![],
    };
    println!("tx.fee={}, fee_collector.amount=2, result={}", tx.fee, is_fee_output_only(&tx, "fee_collector"));
    assert!(!is_fee_output_only(&tx, "fee_collector"));
}

#[test]
fn fee_output_only_with_decimal_amounts() {
    // Real-world scenario: fee=0.0003001, output to admin=0.0003001
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("bob", "9.9996999"), out("admin", "0.0003001")],
        fee: "0.0003001".into(),
        unlocks: vec![],
    };
    println!("tx.fee={}, admin.amount=0.0003001, result={}", tx.fee, is_fee_output_only(&tx, "admin"));
    println!("bob result={}", is_fee_output_only(&tx, "bob"));
    assert!(is_fee_output_only(&tx, "admin"));
    assert!(!is_fee_output_only(&tx, "bob"));
}

#[test]
fn fee_output_only_multiple_outputs_same_addr() {
    // Edge case: two outputs to fee_collector totalling exactly the fee
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![
            out("receiver", "90"),
            out("fee_collector", "0.5"),
            out("fee_collector", "0.5"),
        ],
        fee: "1".into(),
        unlocks: vec![],
    };
    println!("tx.fee=1, fee_collector has 2 outputs (0.5+0.5=1.0), result={}",
        is_fee_output_only(&tx, "fee_collector"));
    assert!(is_fee_output_only(&tx, "fee_collector"));
}

#[test]
fn fee_output_only_addr_receives_fee_plus_transfer() {
    // Edge case: addr receives fee output (1) AND a real transfer (50) = 51 total ≠ 1
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("alice", "1"), out("alice", "50")],
        fee: "1".into(),
        unlocks: vec![],
    };
    println!("tx.fee=1, alice total=51, result={}", is_fee_output_only(&tx, "alice"));
    assert!(!is_fee_output_only(&tx, "alice"));
}

// ── classify_activity_sync: TxUtxo fee output ─────────────────────

#[test]
fn classify_tx_fee_receiver_as_fee_received() {
    // TxUtxo where "admin" receives exactly the fee amount → fee_received
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("bob", "99"), out("admin", "1")],
        fee: "1".into(),
        unlocks: vec![],
    };
    let items = classify_activity_sync(&PlainPayload::TxUtxo(tx), "admin");
    println!("items for admin: {:?}", items.iter().map(|i| (&i.activity_type, &i.amount)).collect::<Vec<_>>());
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "fee_received");
    assert_eq!(items[0].direction, "in");
    assert_eq!(items[0].amount.as_deref(), Some("1"));
}

#[test]
fn classify_tx_regular_receiver_not_fee() {
    // TxUtxo where "bob" receives 99 (not equal to fee=1) → transfer_in
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("bob", "99"), out("admin", "1")],
        fee: "1".into(),
        unlocks: vec![],
    };
    let items = classify_activity_sync(&PlainPayload::TxUtxo(tx), "bob");
    println!("items for bob: {:?}", items.iter().map(|i| (&i.activity_type, &i.amount)).collect::<Vec<_>>());
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "transfer_in");
    assert_eq!(items[0].amount.as_deref(), Some("99"));
}

#[test]
fn classify_tx_fee_received_with_real_amounts() {
    // Simulates the real scenario: 160 nodes each getting micro-fees
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![
            out("receiver", "9.9996999"),
            out("treasury", "0.0003001"),
        ],
        fee: "0.0003001".into(),
        unlocks: vec![],
    };
    let items_treasury = classify_activity_sync(&PlainPayload::TxUtxo(tx.clone()), "treasury");
    let items_receiver = classify_activity_sync(&PlainPayload::TxUtxo(tx), "receiver");
    println!("treasury: {:?}", items_treasury.iter().map(|i| (&i.activity_type, &i.amount)).collect::<Vec<_>>());
    println!("receiver: {:?}", items_receiver.iter().map(|i| (&i.activity_type, &i.amount)).collect::<Vec<_>>());
    assert_eq!(items_treasury[0].activity_type, "fee_received");
    assert_eq!(items_receiver[0].activity_type, "transfer_in");
}

// ── classify_activity (async): fee detection ────────────────────────

#[tokio::test]
async fn classify_tx_fee_received_async() {
    use async_trait::async_trait;
    use pms_interface::NetDagAdapter;

    struct MockAdapter;

    #[async_trait]
    impl NetDagAdapter for MockAdapter {
        async fn have_block(&self, _id: &str) -> bool { false }
        async fn persist_block(&self, _wb: &pms_wire::WireBlock) -> anyhow::Result<pms_storage::PutResult> { Ok(pms_storage::PutResult::Inserted) }
        async fn broadcast_block(&self, _b: &pms_wire::WireBlock) -> anyhow::Result<()> { Ok(()) }
        async fn top_tips(&self, _limit: usize) -> anyhow::Result<Vec<String>> { Ok(vec![]) }
        async fn get_block(&self, _id: &str) -> anyhow::Result<Option<pms_wire::WireBlock>> { Ok(None) }
        async fn recent_ids(&self, _limit: usize) -> anyhow::Result<Vec<String>> { Ok(vec![]) }
        async fn get_blocks_by_ids(&self, _ids: &[String]) -> anyhow::Result<Vec<pms_wire::WireBlock>> { Ok(vec![]) }
        fn min_pow_leading_zero_bits(&self) -> u8 { 0 }
        async fn circulating_supply(&self) -> (rust_decimal::Decimal, u64) { (rust_decimal::Decimal::ZERO, 0) }
        async fn circulating_supply_by_asset(&self, _asset_id: Option<&str>) -> (rust_decimal::Decimal, u64) { (rust_decimal::Decimal::ZERO, 0) }
        async fn balance_by_address(&self, _address: &str) -> rust_decimal::Decimal { rust_decimal::Decimal::ZERO }
        async fn balance_by_address_and_asset(&self, _address: &str, _asset_id: Option<&str>) -> rust_decimal::Decimal { rust_decimal::Decimal::ZERO }
        async fn utxos_by_address(&self, _address: &str) -> Vec<(pms_types::OutputId, pms_types::TxOutput)> { vec![] }
        async fn add_utxo(&self, _txid: String, _index: u32, _output: pms_types::TxOutput) {}
        async fn remove_utxo(&self, _output_id: &pms_types::OutputId) -> bool { false }
        async fn get_utxo(&self, _output_id: &pms_types::OutputId) -> Option<pms_types::TxOutput> {
            Some(pms_types::TxOutput::new("sender", "100", None))
        }
    }

    let tx = Transaction {
        inputs: vec![TxInput { out: OutputId { txid: "tx1".into(), index: 0 } }],
        outputs: vec![out("receiver", "99"), out("admin", "1")],
        fee: "1".into(),
        unlocks: vec![],
    };

    let adapter = MockAdapter;

    // admin receives exactly the fee → fee_received
    let items_admin = classify_activity(&PlainPayload::TxUtxo(tx.clone()), "admin", &adapter).await;
    println!("async admin: {:?}", items_admin.iter().map(|i| (&i.activity_type, &i.amount)).collect::<Vec<_>>());
    assert_eq!(items_admin.len(), 1);
    assert_eq!(items_admin[0].activity_type, "fee_received");

    // receiver gets 99 ≠ fee(1) → transfer_in
    let items_recv = classify_activity(&PlainPayload::TxUtxo(tx), "receiver", &adapter).await;
    println!("async receiver: {:?}", items_recv.iter().map(|i| (&i.activity_type, &i.amount)).collect::<Vec<_>>());
    assert_eq!(items_recv.len(), 1);
    assert_eq!(items_recv[0].activity_type, "transfer_in");
}

// ── classify_activity_sync: irrelevant payloads ──────────────────

#[test]
fn classify_genesis_returns_empty() {
    let plain = PlainPayload::Genesis;
    let items = classify_activity_sync(&plain, "alice");
    assert!(items.is_empty());
}

// ── classify_activity (async) with mock adapter ──────────────────

#[tokio::test]
async fn classify_tx_transfer_in_async() {
    use async_trait::async_trait;
    use pms_interface::NetDagAdapter;

    struct MockAdapter;

    #[async_trait]
    impl NetDagAdapter for MockAdapter {
        async fn have_block(&self, _id: &str) -> bool {
            false
        }
        async fn persist_block(
            &self,
            _wb: &pms_wire::WireBlock,
        ) -> anyhow::Result<pms_storage::PutResult> {
            Ok(pms_storage::PutResult::Inserted)
        }
        async fn broadcast_block(&self, _b: &pms_wire::WireBlock) -> anyhow::Result<()> {
            Ok(())
        }
        async fn top_tips(&self, _limit: usize) -> anyhow::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn get_block(&self, _id: &str) -> anyhow::Result<Option<pms_wire::WireBlock>> {
            Ok(None)
        }
        async fn recent_ids(&self, _limit: usize) -> anyhow::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn get_blocks_by_ids(
            &self,
            _ids: &[String],
        ) -> anyhow::Result<Vec<pms_wire::WireBlock>> {
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
        async fn balance_by_address_and_asset(
            &self,
            _address: &str,
            _asset_id: Option<&str>,
        ) -> rust_decimal::Decimal {
            rust_decimal::Decimal::ZERO
        }
        async fn utxos_by_address(
            &self,
            _address: &str,
        ) -> Vec<(pms_types::OutputId, pms_types::TxOutput)> {
            vec![]
        }
        async fn add_utxo(&self, _txid: String, _index: u32, _output: pms_types::TxOutput) {
        }
        async fn remove_utxo(&self, _output_id: &pms_types::OutputId) -> bool {
            false
        }
        async fn get_utxo(
            &self,
            _output_id: &pms_types::OutputId,
        ) -> Option<pms_types::TxOutput> {
            // Return a UTXO for the sender to test sender resolution
            Some(pms_types::TxOutput::new("sender_addr", "100", None))
        }
    }

    let tx = Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: "tx1".into(),
                index: 0,
            },
        }],
        outputs: vec![out("receiver_addr", "90"), out("sender_addr", "9")],
        fee: "1".into(),
        unlocks: vec![],
    };

    let adapter = MockAdapter;
    let items = classify_activity(&PlainPayload::TxUtxo(tx), "receiver_addr", &adapter).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "transfer_in");
    assert_eq!(items[0].direction, "in");
    assert_eq!(items[0].amount.as_deref(), Some("90"));
    assert_eq!(items[0].counterparty.as_deref(), Some("sender_addr"));
}

#[tokio::test]
async fn classify_tx_transfer_out_async() {
    use async_trait::async_trait;
    use pms_interface::NetDagAdapter;

    struct MockAdapter;

    #[async_trait]
    impl NetDagAdapter for MockAdapter {
        async fn have_block(&self, _id: &str) -> bool {
            false
        }
        async fn persist_block(
            &self,
            _wb: &pms_wire::WireBlock,
        ) -> anyhow::Result<pms_storage::PutResult> {
            Ok(pms_storage::PutResult::Inserted)
        }
        async fn broadcast_block(&self, _b: &pms_wire::WireBlock) -> anyhow::Result<()> {
            Ok(())
        }
        async fn top_tips(&self, _limit: usize) -> anyhow::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn get_block(&self, _id: &str) -> anyhow::Result<Option<pms_wire::WireBlock>> {
            Ok(None)
        }
        async fn recent_ids(&self, _limit: usize) -> anyhow::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn get_blocks_by_ids(
            &self,
            _ids: &[String],
        ) -> anyhow::Result<Vec<pms_wire::WireBlock>> {
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
        async fn balance_by_address_and_asset(
            &self,
            _address: &str,
            _asset_id: Option<&str>,
        ) -> rust_decimal::Decimal {
            rust_decimal::Decimal::ZERO
        }
        async fn utxos_by_address(
            &self,
            _address: &str,
        ) -> Vec<(pms_types::OutputId, pms_types::TxOutput)> {
            vec![]
        }
        async fn add_utxo(&self, _txid: String, _index: u32, _output: pms_types::TxOutput) {
        }
        async fn remove_utxo(&self, _output_id: &pms_types::OutputId) -> bool {
            false
        }
        async fn get_utxo(
            &self,
            _output_id: &pms_types::OutputId,
        ) -> Option<pms_types::TxOutput> {
            Some(pms_types::TxOutput::new("sender_addr", "100", None))
        }
    }

    let tx = Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: "tx1".into(),
                index: 0,
            },
        }],
        outputs: vec![out("bob", "99")],
        fee: "1".into(),
        unlocks: vec![],
    };

    let adapter = MockAdapter;
    let items = classify_activity(&PlainPayload::TxUtxo(tx), "sender_addr", &adapter).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "transfer_out");
    assert_eq!(items[0].direction, "out");
    assert_eq!(items[0].amount.as_deref(), Some("99"));
    assert_eq!(items[0].counterparty.as_deref(), Some("bob"));
}

#[tokio::test]
async fn classify_tx_transfer_self_async() {
    use async_trait::async_trait;
    use pms_interface::NetDagAdapter;

    struct MockAdapter;

    #[async_trait]
    impl NetDagAdapter for MockAdapter {
        async fn have_block(&self, _id: &str) -> bool {
            false
        }
        async fn persist_block(
            &self,
            _wb: &pms_wire::WireBlock,
        ) -> anyhow::Result<pms_storage::PutResult> {
            Ok(pms_storage::PutResult::Inserted)
        }
        async fn broadcast_block(&self, _b: &pms_wire::WireBlock) -> anyhow::Result<()> {
            Ok(())
        }
        async fn top_tips(&self, _limit: usize) -> anyhow::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn get_block(&self, _id: &str) -> anyhow::Result<Option<pms_wire::WireBlock>> {
            Ok(None)
        }
        async fn recent_ids(&self, _limit: usize) -> anyhow::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn get_blocks_by_ids(
            &self,
            _ids: &[String],
        ) -> anyhow::Result<Vec<pms_wire::WireBlock>> {
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
        async fn balance_by_address_and_asset(
            &self,
            _address: &str,
            _asset_id: Option<&str>,
        ) -> rust_decimal::Decimal {
            rust_decimal::Decimal::ZERO
        }
        async fn utxos_by_address(
            &self,
            _address: &str,
        ) -> Vec<(pms_types::OutputId, pms_types::TxOutput)> {
            vec![]
        }
        async fn add_utxo(&self, _txid: String, _index: u32, _output: pms_types::TxOutput) {
        }
        async fn remove_utxo(&self, _output_id: &pms_types::OutputId) -> bool {
            false
        }
        async fn get_utxo(
            &self,
            _output_id: &pms_types::OutputId,
        ) -> Option<pms_types::TxOutput> {
            Some(pms_types::TxOutput::new("alice", "100", None))
        }
    }

    let tx = Transaction {
        inputs: vec![TxInput {
            out: OutputId {
                txid: "tx1".into(),
                index: 0,
            },
        }],
        outputs: vec![out("alice", "99")],
        fee: "1".into(),
        unlocks: vec![],
    };

    let adapter = MockAdapter;
    let items = classify_activity(&PlainPayload::TxUtxo(tx), "alice", &adapter).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "transfer_self");
    assert_eq!(items[0].direction, "info");
    assert_eq!(items[0].amount.as_deref(), Some("99"));
}

// ── Mint with asset_id ───────────────────────────────────────────

#[test]
fn classify_mint_with_asset_id() {
    let plain = PlainPayload::Mint {
        outputs: vec![out_asset("alice", "1000", "edenite")],
    };
    let items = classify_activity_sync(&plain, "alice");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "mint");
    assert_eq!(items[0].asset_id.as_deref(), Some("edenite"));
}

// ── Encrypted fallback item structure ────────────────────────────

#[test]
fn encrypted_fallback_item_has_correct_fields() {
    // Verify the structure of the encrypted fallback ActivityItem
    // matches what the endpoint produces.
    let item = ActivityItem {
        block_id: "enc_blk1".to_string(),
        ts_ms: 1000,
        activity_type: "encrypted".to_string(),
        direction: "info".to_string(),
        amount: None,
        asset_id: None,
        counterparty: None,
        ledger_id: None,
        payload: serde_json::json!({ "encrypted": true }),
    };
    assert_eq!(item.activity_type, "encrypted");
    assert_eq!(item.direction, "info");
    assert!(item.amount.is_none());
    assert!(item.counterparty.is_none());

    // Verify it serializes correctly (no skip_serializing_if surprises)
    let json = serde_json::to_value(&item).unwrap();
    assert_eq!(json["activity_type"], "encrypted");
    assert_eq!(json["payload"]["encrypted"], true);
}
