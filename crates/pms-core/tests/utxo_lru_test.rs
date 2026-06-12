use pms_core::utxo::{ShardedUtxoSet, UtxoFetcher};
use pms_types::{OutputId, TxOutput};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

/// Helper: create an OutputId with a given hex prefix (for shard routing) and index.
fn make_oid(hex_prefix: &str, index: u32) -> OutputId {
    // Pad to 64-char hex txid (typical length)
    let txid = format!("{:0<64}", hex_prefix);
    OutputId { txid, index }
}

/// Helper: create a TxOutput with given address and amount.
fn make_txo(address: &str, amount: &str) -> TxOutput {
    TxOutput {
        address: address.to_string(),
        amount: amount.to_string(),
        asset_id: None,
    }
}

/// Build a mock UtxoFetcher backed by a HashMap (simulates RocksDB).
fn mock_store(entries: Vec<(OutputId, TxOutput)>) -> UtxoFetcher {
    let map: Arc<Mutex<HashMap<(String, u32), TxOutput>>> = Arc::new(Mutex::new(
        entries
            .into_iter()
            .map(|(oid, txo)| ((oid.txid, oid.index), txo))
            .collect(),
    ));

    Arc::new(move |txid: &str, index: u32| {
        let m = map.lock().unwrap();
        m.get(&(txid.to_string(), index)).cloned()
    })
}

// ═══════════════════════════════════════════════════════════════════════
// Test 1: LRU eviction preserves supply cache
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn lru_eviction_preserves_supply_cache() {
    // Capacity = 256 (1 per shard minimum). We'll add 512 UTXOs to force eviction.
    let max_utxos = 256;
    let utxos = ShardedUtxoSet::new(max_utxos, None);

    let mut total_expected = Decimal::ZERO;
    let count = 512;

    // Add 512 UTXOs, each with amount "10.0"
    for i in 0..count {
        let hex = format!("{:02x}{:062}", i % 256, i);
        let oid = OutputId {
            txid: hex,
            index: 0,
        };
        let txo = make_txo("addr_A", "10.0");
        utxos.add(oid, txo).await;
        total_expected += Decimal::from(10);
    }

    let cached = utxos.total_len().await;
    let (supply, utxo_count) = utxos.circulating_supply().await;
    let balance = utxos.balance_by_address("addr_A").await;

    println!("  cached in LRU:     {}", cached);
    println!("  total supply:      {} ({} UTXOs)", supply, utxo_count);
    println!("  balance addr_A:    {}", balance);
    println!("  expected supply:   {} ({} UTXOs)", total_expected, count);

    // Supply cache should reflect ALL 512 UTXOs, not just the cached ones
    assert_eq!(utxo_count, count, "supply utxo_count must be full count");
    assert_eq!(supply, total_expected, "supply total must be full amount");
    assert_eq!(balance, total_expected, "balance must be full amount");

    // But the LRU cache should be capped
    assert!(
        cached <= max_utxos,
        "cached {} should be <= max_utxos {}",
        cached,
        max_utxos
    );
    println!("  PASS: supply/balance accurate despite LRU eviction");
}

// ═══════════════════════════════════════════════════════════════════════
// Test 2: get() falls back to store on cache miss
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn lru_get_fallback_to_store() {
    // Capacity = 256 (minimal), insert 1 UTXO then evict it by filling the shard
    let oid_target = make_oid("aa", 0); // shard 0xAA = 170
    let txo_target = make_txo("addr_B", "42.5");

    // Build mock store with target UTXO
    let store = mock_store(vec![(oid_target.clone(), txo_target.clone())]);

    let utxos = ShardedUtxoSet::new(256, Some(store));

    // Add the target UTXO
    utxos.add(oid_target.clone(), txo_target.clone()).await;

    // Verify it's in cache
    let got = utxos.get(&oid_target).await;
    assert!(got.is_some(), "should be in cache");
    println!("  in-cache get:      {:?}", got.as_ref().unwrap().amount);

    // Now evict it by filling shard 0xAA with many entries
    // Each shard gets max_utxos / 256 = 1 entry. So adding 2 more to shard AA will evict it.
    for i in 1..=5 {
        let oid = make_oid("aa", i);
        let txo = make_txo("addr_filler", "1.0");
        utxos.add(oid, txo).await;
    }

    // The target should be evicted from LRU by now
    // But get() should still find it via fallback
    let got_fallback = utxos.get(&oid_target).await;
    println!(
        "  fallback get:      {:?}",
        got_fallback.as_ref().map(|t| &t.amount)
    );
    assert!(got_fallback.is_some(), "should find via fallback");
    assert_eq!(
        got_fallback.as_ref().unwrap().amount, "42.5",
        "fallback should return correct amount"
    );
    assert_eq!(
        got_fallback.as_ref().unwrap().address, "addr_B",
        "fallback should return correct address"
    );
    println!("  PASS: get() falls back to store on cache miss");
}

// ═══════════════════════════════════════════════════════════════════════
// Test 3: remove() with fallback updates caches correctly
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn lru_remove_fallback_updates_caches() {
    let oid_a = make_oid("bb", 0);
    let txo_a = make_txo("addr_C", "100.0");
    let oid_b = make_oid("bb", 1);
    let txo_b = make_txo("addr_C", "50.0");

    let store = mock_store(vec![
        (oid_a.clone(), txo_a.clone()),
        (oid_b.clone(), txo_b.clone()),
    ]);

    // Use tiny capacity to force eviction
    let utxos = ShardedUtxoSet::new(256, Some(store));

    // Add both UTXOs
    utxos.add(oid_a.clone(), txo_a).await;
    utxos.add(oid_b.clone(), txo_b).await;

    let balance_before = utxos.balance_by_address("addr_C").await;
    let (supply_before, count_before) = utxos.circulating_supply().await;
    println!("  balance before:    {}", balance_before);
    println!("  supply before:     {} ({} UTXOs)", supply_before, count_before);

    assert_eq!(balance_before, Decimal::from(150));
    assert_eq!(supply_before, Decimal::from(150));
    assert_eq!(count_before, 2);

    // Evict oid_a by filling the shard
    for i in 2..=10 {
        let oid = make_oid("bb", i);
        let txo = make_txo("addr_filler", "1.0");
        utxos.add(oid, txo).await;
    }

    // Now remove oid_a (which should be evicted from cache)
    let removed = utxos.remove(&oid_a).await;
    println!(
        "  removed (via fallback): {:?}",
        removed.as_ref().map(|t| &t.amount)
    );
    assert!(removed.is_some(), "remove should succeed via fallback");
    assert_eq!(removed.as_ref().unwrap().amount, "100.0");

    let balance_after = utxos.balance_by_address("addr_C").await;
    println!("  balance after remove:  {}", balance_after);

    // Balance should be reduced by the removed amount
    assert_eq!(
        balance_after,
        Decimal::from(50),
        "balance should be 50 after removing 100"
    );
    println!("  PASS: remove() via fallback correctly updates supply/balance caches");
}

// ═══════════════════════════════════════════════════════════════════════
// Test 4: unlimited mode (max_utxos = 0) works like unbounded
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn lru_unlimited_mode() {
    let utxos = ShardedUtxoSet::new(0, None);

    // Add 1000 UTXOs
    for i in 0..1000u32 {
        let hex = format!("{:02x}{:062}", i % 256, i);
        let oid = OutputId {
            txid: hex,
            index: 0,
        };
        let txo = make_txo("addr_D", "1.0");
        utxos.add(oid, txo).await;
    }

    let cached = utxos.total_len().await;
    let (supply, count) = utxos.circulating_supply().await;
    let balance = utxos.balance_by_address("addr_D").await;

    println!("  unlimited mode:");
    println!("    cached:    {}", cached);
    println!("    supply:    {} ({} UTXOs)", supply, count);
    println!("    balance:   {}", balance);

    // All 1000 should be in cache (no eviction)
    assert_eq!(cached, 1000, "all UTXOs should be cached in unlimited mode");
    assert_eq!(count, 1000);
    assert_eq!(supply, Decimal::from(1000));
    assert_eq!(balance, Decimal::from(1000));
    println!("  PASS: unlimited mode (max_utxos=0) keeps all UTXOs in cache");
}

// ═══════════════════════════════════════════════════════════════════════
// Test 5: apply_diff with cache miss uses fallback
// ═══════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn lru_apply_diff_with_cache_miss() {
    let oid_spend = make_oid("cc", 0);
    let txo_spend = make_txo("addr_E", "200.0");

    let store = mock_store(vec![(oid_spend.clone(), txo_spend.clone())]);

    let utxos = ShardedUtxoSet::new(256, Some(store));

    // Add the UTXO to spend
    utxos.add(oid_spend.clone(), txo_spend).await;

    // Evict it
    for i in 1..=10 {
        let oid = make_oid("cc", i);
        let txo = make_txo("addr_filler", "1.0");
        utxos.add(oid, txo).await;
    }

    let balance_before = utxos.balance_by_address("addr_E").await;
    println!("  balance before apply_diff: {}", balance_before);
    assert_eq!(balance_before, Decimal::from(200));

    // Apply diff: spend the evicted UTXO, create a new one
    let oid_new = make_oid("dd", 0);
    let txo_new = make_txo("addr_F", "200.0");
    utxos
        .apply_diff(&[oid_spend], &[(oid_new.clone(), txo_new)])
        .await;

    let balance_e_after = utxos.balance_by_address("addr_E").await;
    let balance_f_after = utxos.balance_by_address("addr_F").await;
    println!("  balance addr_E after: {}", balance_e_after);
    println!("  balance addr_F after: {}", balance_f_after);

    assert_eq!(
        balance_e_after,
        Decimal::ZERO,
        "addr_E should be 0 after spend"
    );
    assert_eq!(
        balance_f_after,
        Decimal::from(200),
        "addr_F should be 200 after create"
    );

    // Verify the new UTXO is retrievable
    let got_new = utxos.get(&oid_new).await;
    assert!(got_new.is_some(), "newly created UTXO should be in cache");
    println!("  PASS: apply_diff handles cache miss via fallback");
}

// ═══════════════════════════════════════════════════════════════════════
// Test 6: utxos_by_address with mixed cached/non-cached
// ═══════════════════════════════════════════════════════════════════════

/// NB (v0.9.4) : ce test affirmait à tort que `utxos_by_address` retrouvait des
/// UTXO ÉVINCÉS via fallback. Or `add()` retire intentionnellement les entrées
/// évincées de l'`address_index` (design v0.6.3 — mémoire bornée pour un
/// coordinateur à des millions d'UTXO), donc `utxos_by_address` (qui itère
/// l'index) ne peut PAS les voir. De plus `new(256)` donne une capacité de 1
/// par shard (`ceil(256/SHARD_COUNT=256)`), et les deux OID partagent le préfixe
/// txid "ee" → même shard → oid1 était évincé dès l'ajout d'oid2. Réécrit pour
/// le comportement RÉEL : (A) sans éviction, `utxos_by_address` retourne tout ;
/// (B) le VRAI fallback est sur `get()` (cache miss → store), pas sur l'index.
#[tokio::test]
async fn lru_utxos_by_address_and_get_fallback() {
    let oid1 = make_oid("ee", 0);
    let txo1 = make_txo("addr_G", "10.0");
    let oid2 = make_oid("ee", 1);
    let txo2 = make_txo("addr_G", "20.0");

    // ── Scénario A : capacité illimitée → aucune éviction.
    // Les deux UTXO de addr_G (même shard) restent cachés ET indexés.
    {
        let store = mock_store(vec![
            (oid1.clone(), txo1.clone()),
            (oid2.clone(), txo2.clone()),
        ]);
        let utxos = ShardedUtxoSet::new(0, Some(store)); // 0 = unbounded
        utxos.add(oid1.clone(), txo1.clone()).await;
        utxos.add(oid2.clone(), txo2.clone()).await;

        let found = utxos.utxos_by_address("addr_G").await;
        println!("  [A unbounded] utxos for addr_G: {}", found.len());
        for (oid, txo) in &found {
            println!("    {} #{} = {} PMS", txo.address, oid.index, txo.amount);
        }
        assert_eq!(found.len(), 2, "both same-address UTXOs must be indexed/returned");
        let total: Decimal = found
            .iter()
            .map(|(_, t)| Decimal::from_str(&t.amount).unwrap())
            .sum();
        assert_eq!(total, Decimal::from(30), "total should be 10 + 20 = 30");
    }

    // ── Scénario B : capacité bornée → cap 1 par shard avec new(256).
    // oid1 et oid2 collisionnent (préfixe "ee") : ajouter oid2 ÉVINCE oid1.
    {
        let store = mock_store(vec![
            (oid1.clone(), txo1.clone()),
            (oid2.clone(), txo2.clone()),
        ]);
        let utxos = ShardedUtxoSet::new(256, Some(store)); // cap_per_shard = 1
        utxos.add(oid1.clone(), txo1.clone()).await;
        utxos.add(oid2.clone(), txo2.clone()).await; // évince oid1 du cache + index

        // VRAI fallback : get() ne consulte pas l'index — sur cache miss il va au
        // store. oid1 évincé est donc TOUJOURS récupérable via get().
        let got1 = utxos.get(&oid1).await;
        println!("  [B bounded] get(oid1 evicted) → {:?}", got1.as_ref().map(|t| &t.amount));
        assert!(got1.is_some(), "get() must fall back to the store for an evicted UTXO");
        assert_eq!(got1.unwrap().amount, "10.0");

        // En revanche utxos_by_address itère l'index, d'où oid1 a été retiré à
        // l'éviction (design v0.6.3 mémoire bornée) → ne voit plus que oid2.
        let by_addr = utxos.utxos_by_address("addr_G").await;
        println!("  [B bounded] utxos_by_address(addr_G) after eviction: {}", by_addr.len());
        assert_eq!(
            by_addr.len(),
            1,
            "evicted entries are removed from address_index by design (bounded memory)"
        );
        assert_eq!(by_addr[0].0.index, 1, "only the non-evicted oid2 remains indexed");
    }
    println!("  PASS: utxos_by_address (index) + get() store-fallback behave per v0.6.3 design");
}
