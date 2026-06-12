//! Tests for the token_balance_cache (v0.6.7) — O(1) balance queries for custom assets.
//!
//! Validates that `balance_by_address_and_asset(addr, Some(asset))` is O(1) via
//! the DashMap cache, identical to the `native_balance_cache` pattern for PMS.

use pms_core::utxo::ShardedUtxoSet;
use pms_types::{OutputId, TxOutput};
use rust_decimal::Decimal;
use std::str::FromStr;

/// Helper: create an OutputId with a given hex prefix and index.
fn make_oid(hex_prefix: &str, index: u32) -> OutputId {
    let txid = format!("{:0<64}", hex_prefix);
    OutputId { txid, index }
}

/// Helper: create a TxOutput with a custom asset.
fn make_token_txo(address: &str, amount: &str, asset_id: &str) -> TxOutput {
    TxOutput::new(address.to_string(), amount.to_string(), Some(asset_id.to_string()))
}

/// Helper: create a native PMS TxOutput.
fn make_pms_txo(address: &str, amount: &str) -> TxOutput {
    TxOutput::new(address.to_string(), amount.to_string(), None)
}

// ═══════════════════════════════════════════════════════════════════════
// Test 1: Token balance cache O(1) lookup
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn token_balance_cache_add_and_query() {
    let utxos = ShardedUtxoSet::new(0, None); // Unbounded

    // Add 100 edenite UTXOs to addr1
    for i in 0..100 {
        let oid = make_oid(&format!("{:02x}aa", i), 0);
        let txo = make_token_txo("addr1", "10.5", "edenite");
        utxos.add(oid, txo).await;
    }

    // Add 50 gold UTXOs to addr1
    for i in 0..50 {
        let oid = make_oid(&format!("{:02x}bb", i), 0);
        let txo = make_token_txo("addr1", "20.0", "gold");
        utxos.add(oid, txo).await;
    }

    // Add 30 edenite UTXOs to addr2
    for i in 0..30 {
        let oid = make_oid(&format!("{:02x}cc", i), 0);
        let txo = make_token_txo("addr2", "5.0", "edenite");
        utxos.add(oid, txo).await;
    }

    // Query balances — should be O(1) via token_balance_cache
    let bal_edn_1 = utxos
        .balance_by_address_and_asset("addr1", Some("edenite"))
        .await;
    let bal_gold_1 = utxos
        .balance_by_address_and_asset("addr1", Some("gold"))
        .await;
    let bal_edn_2 = utxos
        .balance_by_address_and_asset("addr2", Some("edenite"))
        .await;
    let bal_none = utxos
        .balance_by_address_and_asset("addr1", Some("nonexistent"))
        .await;
    let bal_native = utxos.balance_by_address("addr1").await;

    println!("addr1 edenite balance: {bal_edn_1}");
    println!("addr1 gold balance: {bal_gold_1}");
    println!("addr2 edenite balance: {bal_edn_2}");
    println!("addr1 nonexistent balance: {bal_none}");
    println!("addr1 native PMS balance: {bal_native}");

    assert_eq!(bal_edn_1, Decimal::from_str("1050.0").unwrap()); // 100 * 10.5
    assert_eq!(bal_gold_1, Decimal::from_str("1000.0").unwrap()); // 50 * 20.0
    assert_eq!(bal_edn_2, Decimal::from_str("150.0").unwrap()); // 30 * 5.0
    assert_eq!(bal_none, Decimal::ZERO);
    assert_eq!(bal_native, Decimal::ZERO); // No PMS UTXOs added
}

// ═══════════════════════════════════════════════════════════════════════
// Test 2: Token balance updates on remove
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn token_balance_cache_remove_updates() {
    let utxos = ShardedUtxoSet::new(0, None);

    let oid_a = make_oid("aa", 0);
    let oid_b = make_oid("bb", 0);
    let oid_c = make_oid("cc", 0);

    utxos
        .add(oid_a.clone(), make_token_txo("addr1", "100", "edenite"))
        .await;
    utxos
        .add(oid_b.clone(), make_token_txo("addr1", "200", "edenite"))
        .await;
    utxos
        .add(oid_c.clone(), make_token_txo("addr1", "50", "edenite"))
        .await;

    let bal = utxos
        .balance_by_address_and_asset("addr1", Some("edenite"))
        .await;
    println!("Before remove: {bal}");
    assert_eq!(bal, Decimal::from(350));

    // Remove one UTXO
    utxos.remove(&oid_b).await;
    let bal_after = utxos
        .balance_by_address_and_asset("addr1", Some("edenite"))
        .await;
    println!("After removing 200: {bal_after}");
    assert_eq!(bal_after, Decimal::from(150));

    // Remove remaining — should go to zero
    utxos.remove(&oid_a).await;
    utxos.remove(&oid_c).await;
    let bal_zero = utxos
        .balance_by_address_and_asset("addr1", Some("edenite"))
        .await;
    println!("After removing all: {bal_zero}");
    assert_eq!(bal_zero, Decimal::ZERO);
}

// ═══════════════════════════════════════════════════════════════════════
// Test 3: apply_diff updates token balance cache correctly
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn token_balance_cache_apply_diff() {
    let utxos = ShardedUtxoSet::new(0, None);

    // Setup: addr1 has 3 edenite UTXOs = 300
    let oid_a = make_oid("aa", 0);
    let oid_b = make_oid("bb", 0);
    let oid_c = make_oid("cc", 0);
    utxos
        .add(oid_a.clone(), make_token_txo("addr1", "100", "edenite"))
        .await;
    utxos
        .add(oid_b.clone(), make_token_txo("addr1", "100", "edenite"))
        .await;
    utxos
        .add(oid_c.clone(), make_token_txo("addr1", "100", "edenite"))
        .await;

    // apply_diff: spend oid_a and oid_b, create oid_d for addr2
    let oid_d = make_oid("dd", 0);
    let creates = vec![(oid_d.clone(), make_token_txo("addr2", "180", "edenite"))];
    utxos.apply_diff(&[oid_a, oid_b], &creates).await;

    let bal1 = utxos
        .balance_by_address_and_asset("addr1", Some("edenite"))
        .await;
    let bal2 = utxos
        .balance_by_address_and_asset("addr2", Some("edenite"))
        .await;
    println!("addr1 after diff: {bal1}");
    println!("addr2 after diff: {bal2}");

    assert_eq!(bal1, Decimal::from(100)); // Only oid_c remains
    assert_eq!(bal2, Decimal::from(180)); // oid_d created
}

// ═══════════════════════════════════════════════════════════════════════
// Test 4: Mixed native + token balances are independent
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn token_and_native_balances_independent() {
    let utxos = ShardedUtxoSet::new(0, None);

    // Add PMS native
    utxos
        .add(make_oid("aa", 0), make_pms_txo("addr1", "1000"))
        .await;

    // Add edenite token
    utxos
        .add(
            make_oid("bb", 0),
            make_token_txo("addr1", "500", "edenite"),
        )
        .await;

    let native = utxos.balance_by_address("addr1").await;
    let native_via_asset = utxos
        .balance_by_address_and_asset("addr1", None)
        .await;
    let token = utxos
        .balance_by_address_and_asset("addr1", Some("edenite"))
        .await;

    println!("Native balance: {native}");
    println!("Native via asset API: {native_via_asset}");
    println!("Token balance: {token}");

    assert_eq!(native, Decimal::from(1000));
    assert_eq!(native_via_asset, Decimal::from(1000));
    assert_eq!(token, Decimal::from(500));

    // Remove native — token unaffected
    utxos.remove(&make_oid("aa", 0)).await;
    let native_after = utxos.balance_by_address("addr1").await;
    let token_after = utxos
        .balance_by_address_and_asset("addr1", Some("edenite"))
        .await;
    println!("After native remove — native: {native_after}, token: {token_after}");

    assert_eq!(native_after, Decimal::ZERO);
    assert_eq!(token_after, Decimal::from(500));
}

// ═══════════════════════════════════════════════════════════════════════
// Test 5: LRU eviction preserves token balance cache
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn lru_eviction_preserves_token_balance_cache() {
    // Capacity 256 (1 per shard minimum). Add 512 UTXOs to force eviction.
    let max_utxos = 256;
    let utxos = ShardedUtxoSet::new(max_utxos, None);

    for i in 0..512u32 {
        let oid = make_oid(&format!("{:02x}ff{:04x}", i % 256, i), 0);
        let txo = make_token_txo("coordinator", "1.0", "edenite");
        utxos.add(oid, txo).await;
    }

    // Token balance cache should reflect ALL 512 UTXOs (not just the cached ones)
    let bal = utxos
        .balance_by_address_and_asset("coordinator", Some("edenite"))
        .await;
    let (supply, count) = utxos
        .circulating_supply_by_asset(Some("edenite"))
        .await;

    println!("Token balance (512 UTXOs, 256 cache): {bal}");
    println!("Supply: {supply}, count: {count}");

    assert_eq!(bal, Decimal::from(512)); // 512 * 1.0
    assert_eq!(supply, Decimal::from(512));
    assert_eq!(count, 512);
}
