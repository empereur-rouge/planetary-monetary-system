// crates/pms-core/tests/utxo_concurrent.rs
//
// Tests de concurrence pour ShardedUtxoSet.
// Reproduisent le scénario exact qui causait un deadlock en production :
// des requêtes balance/utxos (lecture) concurrentes avec apply_diff (écriture)
// sur les mêmes adresses et les mêmes shards.
//
// Chaque test utilise tokio::time::timeout pour détecter un deadlock :
// si les opérations ne se terminent pas en 10 secondes, c'est un hang.

use pms_core::utxo::ShardedUtxoSet;
use pms_types::{OutputId, TxOutput};
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;
use tokio::time::{Duration, timeout};

fn out_id(txid: &str, index: u32) -> OutputId {
    OutputId {
        txid: txid.into(),
        index,
    }
}

fn pms_output(address: &str, amount: &str) -> TxOutput {
    TxOutput {
        address: address.into(),
        amount: amount.into(),
        asset_id: None,
    }
}

fn token_output(address: &str, amount: &str, asset_id: &str) -> TxOutput {
    TxOutput {
        address: address.into(),
        amount: amount.into(),
        asset_id: Some(asset_id.into()),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 1 : balance_by_address pendant apply_diff sur la MÊME adresse
//
// C'est le scénario exact du deadlock en production :
// - Le simulator envoie des TX (→ apply_diff modifie les UTXOs d'Alice)
// - Le dashboard requête /v1/balance pour Alice (→ balance_by_address_and_asset)
// - Avant le fix : DashMap guard tenu pendant .await sur shard → deadlock
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_balance_query_during_apply_diff() {
    let utxos = Arc::new(ShardedUtxoSet::new(0, None));

    // Seed 100 UTXOs pour Alice avec des txids qui tombent sur différents shards
    for i in 0u32..100 {
        let txid = format!("{:02x}{:06x}", i % 256, i);
        utxos
            .add(out_id(&txid, 0), pms_output("Alice", "1.00000000"))
            .await;
    }

    let result = timeout(Duration::from_secs(10), async {
        let mut handles = Vec::new();

        // Spawner 4 tâches de lecture balance en continu
        for _ in 0..4 {
            let utxos = utxos.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..200 {
                    let _balance = utxos.balance_by_address("Alice").await;
                    tokio::task::yield_now().await;
                }
            }));
        }

        // Spawner 2 tâches d'écriture apply_diff en continu sur les UTXOs d'Alice
        for writer_id in 0u32..2 {
            let utxos = utxos.clone();
            handles.push(tokio::spawn(async move {
                for round in 0u32..50 {
                    let old_txid = format!(
                        "{:02x}w{}{}",
                        (writer_id * 50 + round) % 256,
                        writer_id,
                        round
                    );
                    let new_txid = format!(
                        "{:02x}n{}{}",
                        (writer_id * 50 + round) % 256,
                        writer_id,
                        round
                    );

                    // Créer un UTXO puis le spend dans le round suivant
                    utxos
                        .add(out_id(&old_txid, 0), pms_output("Alice", "0.50000000"))
                        .await;

                    let spends = vec![out_id(&old_txid, 0)];
                    let creates = vec![(out_id(&new_txid, 0), pms_output("Alice", "0.50000000"))];
                    utxos.apply_diff(&spends, &creates).await;

                    tokio::task::yield_now().await;
                }
            }));
        }

        for h in handles {
            h.await.unwrap();
        }
    })
    .await;

    assert!(
        result.is_ok(),
        "DEADLOCK: concurrent balance queries + apply_diff hung for 10s"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 2 : utxos_by_address pendant apply_diff sur la MÊME adresse
//
// Même pattern que le test 1 mais avec utxos_by_address (l'endpoint /v1/utxos).
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_utxos_query_during_apply_diff() {
    let utxos = Arc::new(ShardedUtxoSet::new(0, None));

    for i in 0u32..50 {
        let txid = format!("{:02x}{:06x}", i % 256, i);
        utxos
            .add(out_id(&txid, 0), pms_output("Bob", "2.00000000"))
            .await;
    }

    let result = timeout(Duration::from_secs(10), async {
        let mut handles = Vec::new();

        // Lecteurs : utxos_by_address
        for _ in 0..4 {
            let utxos = utxos.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..200 {
                    let _list = utxos.utxos_by_address("Bob").await;
                    tokio::task::yield_now().await;
                }
            }));
        }

        // Écrivains : apply_diff cycle spend+create
        for writer_id in 0u32..2 {
            let utxos = utxos.clone();
            handles.push(tokio::spawn(async move {
                for round in 0u32..50 {
                    let old_txid = format!(
                        "{:02x}w{}{}",
                        (writer_id * 50 + round) % 256,
                        writer_id,
                        round
                    );
                    let new_txid = format!(
                        "{:02x}n{}{}",
                        (writer_id * 50 + round) % 256,
                        writer_id,
                        round
                    );

                    utxos
                        .add(out_id(&old_txid, 0), pms_output("Bob", "1.00000000"))
                        .await;

                    let spends = vec![out_id(&old_txid, 0)];
                    let creates = vec![(out_id(&new_txid, 0), pms_output("Bob", "1.00000000"))];
                    utxos.apply_diff(&spends, &creates).await;

                    tokio::task::yield_now().await;
                }
            }));
        }

        for h in handles {
            h.await.unwrap();
        }
    })
    .await;

    assert!(
        result.is_ok(),
        "DEADLOCK: concurrent utxos_by_address + apply_diff hung for 10s"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 3 : balance + utxos + supply queries pendant add/remove continus
//
// Simule le pattern de production complet :
// - Le coordinator ajoute/supprime des UTXOs (add/remove)
// - Le dashboard requête balance, utxos, supply simultanément
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_mixed_reads_during_add_remove() {
    let utxos = Arc::new(ShardedUtxoSet::new(0, None));

    // Seed initial
    for i in 0u32..20 {
        let txid = format!("{:02x}init{}", i % 256, i);
        utxos
            .add(out_id(&txid, 0), pms_output("Charlie", "5.00000000"))
            .await;
    }

    let result = timeout(Duration::from_secs(10), async {
        let mut handles = Vec::new();

        // Lecteur balance
        let u = utxos.clone();
        handles.push(tokio::spawn(async move {
            for _ in 0..300 {
                let _b = u.balance_by_address("Charlie").await;
                tokio::task::yield_now().await;
            }
        }));

        // Lecteur utxos
        let u = utxos.clone();
        handles.push(tokio::spawn(async move {
            for _ in 0..300 {
                let _list = u.utxos_by_address("Charlie").await;
                tokio::task::yield_now().await;
            }
        }));

        // Lecteur supply
        let u = utxos.clone();
        handles.push(tokio::spawn(async move {
            for _ in 0..300 {
                let _s = u.circulating_supply().await;
                tokio::task::yield_now().await;
            }
        }));

        // Écrivain : cycles add → remove sur Charlie
        let u = utxos.clone();
        handles.push(tokio::spawn(async move {
            for round in 0u32..200 {
                let txid = format!("{:02x}rw{}", round % 256, round);
                let id = out_id(&txid, 0);
                u.add(id.clone(), pms_output("Charlie", "1.00000000")).await;
                u.remove(&id).await;
                tokio::task::yield_now().await;
            }
        }));

        for h in handles {
            h.await.unwrap();
        }
    })
    .await;

    assert!(
        result.is_ok(),
        "DEADLOCK: mixed reads during add/remove hung for 10s"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 4 : apply_diff sur le MÊME shard avec balance queries concurrentes
//
// Force la contention maximale : tous les txids commencent par "aa" (shard 170).
// C'est le pire cas pour le deadlock car toutes les opérations veulent
// le même shard write/read lock.
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_same_shard_contention() {
    let utxos = Arc::new(ShardedUtxoSet::new(0, None));

    // Tous les txids sur le shard "aa" (170)
    for i in 0u32..50 {
        let txid = format!("aa{:06x}", i);
        utxos
            .add(out_id(&txid, 0), pms_output("Dave", "1.00000000"))
            .await;
    }

    let result = timeout(Duration::from_secs(10), async {
        let mut handles = Vec::new();

        // 4 lecteurs balance sur Dave (tous frappent le shard "aa")
        for _ in 0..4 {
            let utxos = utxos.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..500 {
                    let _b = utxos.balance_by_address("Dave").await;
                    tokio::task::yield_now().await;
                }
            }));
        }

        // 2 écrivains apply_diff sur le shard "aa"
        for w in 0u32..2 {
            let utxos = utxos.clone();
            handles.push(tokio::spawn(async move {
                for round in 0u32..100 {
                    let old_txid = format!("aa{:02x}{:04x}", w, round);
                    let new_txid = format!("aa{:02x}{:04x}", w, round + 10000);

                    utxos
                        .add(out_id(&old_txid, 0), pms_output("Dave", "0.10000000"))
                        .await;

                    let spends = vec![out_id(&old_txid, 0)];
                    let creates = vec![(out_id(&new_txid, 0), pms_output("Dave", "0.10000000"))];
                    utxos.apply_diff(&spends, &creates).await;

                    tokio::task::yield_now().await;
                }
            }));
        }

        for h in handles {
            h.await.unwrap();
        }
    })
    .await;

    assert!(
        result.is_ok(),
        "DEADLOCK: same-shard contention between readers and writers hung for 10s"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 5 : Vérification de cohérence après opérations concurrentes
//
// Après un burst concurrent de transactions, vérifie que :
// - Le supply total est correct (conservation)
// - Les balances par adresse sont cohérentes avec les UTXOs
// - L'index adresse est synchronisé avec les shards
// - Aucun UTXO fantôme ni perdu
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn consistency_after_concurrent_operations() {
    let utxos = Arc::new(ShardedUtxoSet::new(0, None));

    // Chaque adresse reçoit 10 PMS initiaux
    let addresses = ["Alpha", "Beta", "Gamma", "Delta"];
    for (i, addr) in addresses.iter().enumerate() {
        let txid = format!("{:02x}seed{}", i * 40, addr);
        utxos
            .add(out_id(&txid, 0), pms_output(addr, "10.00000000"))
            .await;
    }
    // Total initial : 40 PMS

    let result = timeout(Duration::from_secs(10), async {
        let mut handles = Vec::new();

        // 4 writers : chacun fait des transferts entre 2 adresses
        // Alpha ↔ Beta, Gamma ↔ Delta
        // Chaque transfert conserve le montant (pas de fee pour simplifier)
        let pairs = [
            ("Alpha", "Beta"),
            ("Beta", "Alpha"),
            ("Gamma", "Delta"),
            ("Delta", "Gamma"),
        ];

        for (w_id, (from, to)) in pairs.iter().enumerate() {
            let utxos = utxos.clone();
            let from = from.to_string();
            let to = to.to_string();
            handles.push(tokio::spawn(async move {
                for round in 0u32..50 {
                    let spend_txid = format!(
                        "{:02x}s{}r{}",
                        (w_id * 60 + round as usize) % 256,
                        w_id,
                        round
                    );
                    let out_txid = format!(
                        "{:02x}o{}r{}",
                        (w_id * 60 + round as usize) % 256,
                        w_id,
                        round
                    );

                    // Créer un UTXO, puis le transférer
                    utxos
                        .add(out_id(&spend_txid, 0), pms_output(&from, "0.10000000"))
                        .await;

                    let spends = vec![out_id(&spend_txid, 0)];
                    let creates = vec![(out_id(&out_txid, 0), pms_output(&to, "0.10000000"))];
                    utxos.apply_diff(&spends, &creates).await;
                }
            }));
        }

        // 2 readers concurrents
        for _ in 0..2 {
            let utxos = utxos.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..200 {
                    let _b = utxos.balance_by_address("Alpha").await;
                    let _u = utxos.utxos_by_address("Beta").await;
                    let _s = utxos.circulating_supply().await;
                    tokio::task::yield_now().await;
                }
            }));
        }

        for h in handles {
            h.await.unwrap();
        }
    })
    .await;

    assert!(result.is_ok(), "DEADLOCK: consistency test hung for 10s");

    // --- Vérifications de cohérence post-concurrence ---

    // 1) Supply total = somme de TOUS les UTXOs en RAM
    let (supply, supply_count) = utxos.circulating_supply().await;
    let total_len = utxos.total_len().await;
    assert_eq!(
        supply_count, total_len,
        "supply count ({supply_count}) != total UTXOs in shards ({total_len})"
    );

    // 2) Somme des balances par adresse = supply total
    let mut balance_sum = Decimal::ZERO;
    // Collecter toutes les adresses qui ont des UTXOs
    let all_addresses: Vec<String> = {
        let mut addrs = std::collections::HashSet::new();
        for addr in &addresses {
            addrs.insert(addr.to_string());
        }
        // Les transferts ont pu créer des UTXOs sous d'autres noms
        for addr in &addresses {
            let utxo_list = utxos.utxos_by_address(addr).await;
            for (_, output) in &utxo_list {
                addrs.insert(output.address.clone());
            }
        }
        addrs.into_iter().collect()
    };

    for addr in &all_addresses {
        balance_sum += utxos.balance_by_address(addr).await;
    }
    assert_eq!(
        balance_sum, supply,
        "sum of balances ({balance_sum}) != circulating supply ({supply})"
    );

    // 3) Chaque UTXO retourné par utxos_by_address est réellement dans le set
    for addr in &all_addresses {
        let utxo_list = utxos.utxos_by_address(addr).await;
        for (outpoint, _) in &utxo_list {
            let fetched = utxos.get(outpoint).await;
            assert!(
                fetched.is_some(),
                "UTXO {outpoint:?} listed for {addr} but not found in shards"
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 6 : Multi-asset concurrent — balance queries par asset pendant apply_diff
//
// Simule le scénario testnet : PMS + EDEN (custom token) en parallèle.
// Vérifie que les balances par asset restent correctes.
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_multi_asset_balance_during_apply_diff() {
    let utxos = Arc::new(ShardedUtxoSet::new(0, None));

    // Seed : Alice a 100 PMS + 500 EDEN
    utxos
        .add(out_id("aa000000", 0), pms_output("Alice", "100.00000000"))
        .await;
    utxos
        .add(
            out_id("bb000000", 0),
            token_output("Alice", "500.00000000", "edenite"),
        )
        .await;

    let result = timeout(Duration::from_secs(10), async {
        let mut handles = Vec::new();

        // Lecteurs PMS balance
        for _ in 0..2 {
            let utxos = utxos.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..300 {
                    let _b = utxos.balance_by_address_and_asset("Alice", None).await;
                    tokio::task::yield_now().await;
                }
            }));
        }

        // Lecteurs EDEN balance
        for _ in 0..2 {
            let utxos = utxos.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..300 {
                    let _b = utxos
                        .balance_by_address_and_asset("Alice", Some("edenite"))
                        .await;
                    tokio::task::yield_now().await;
                }
            }));
        }

        // Writer : apply_diff de transactions EDEN (Alice → Bob)
        let utxos_w = utxos.clone();
        handles.push(tokio::spawn(async move {
            for round in 0u32..100 {
                let spend_txid = format!("cc{:06x}", round);
                let new_txid_1 = format!("dd{:06x}", round);
                let new_txid_2 = format!("ee{:06x}", round);

                // Crée un UTXO EDEN pour Alice
                utxos_w
                    .add(
                        out_id(&spend_txid, 0),
                        token_output("Alice", "1.00000000", "edenite"),
                    )
                    .await;

                // Transfert : Alice → Bob via apply_diff
                let spends = vec![out_id(&spend_txid, 0)];
                let creates = vec![
                    (
                        out_id(&new_txid_1, 0),
                        token_output("Bob", "0.60000000", "edenite"),
                    ),
                    (
                        out_id(&new_txid_2, 0),
                        token_output("Alice", "0.40000000", "edenite"),
                    ),
                ];
                utxos_w.apply_diff(&spends, &creates).await;
                tokio::task::yield_now().await;
            }
        }));

        for h in handles {
            h.await.unwrap();
        }
    })
    .await;

    assert!(
        result.is_ok(),
        "DEADLOCK: multi-asset concurrent balance + apply_diff hung for 10s"
    );

    // Cohérence post-test : le supply EDEN total doit être conservé
    // Initial 500 + 100 rounds × (créé 1 - spend 1 + créé 0.60 + 0.40) = 500 + 100*1 = 600
    // Non : on crée 1 EDEN, on le spend, on crée 0.60+0.40=1.00. Net = 500 + 0 = 500+100=600
    // En fait : chaque round ajoute 1 (add), spend 1 (diff spend), crée 0.60+0.40=1.00 (diff create)
    // Net par round : +1 -1 +1 = +1. 100 rounds = 500 + 100 = 600.
    let (eden_supply, _) = utxos.circulating_supply_by_asset(Some("edenite")).await;
    assert_eq!(
        eden_supply,
        Decimal::from_str("600.00000000").unwrap(),
        "EDEN supply should be conserved (500 initial + 100 net from rounds)"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 7 : rebuild_indexes produit le même résultat que le cache incrémental
//
// Vérifie que le cache supply et l'index adresse construits incrémentalement
// (via add/remove/apply_diff) sont identiques à ceux reconstruits depuis le
// contenu brut des shards.
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn rebuild_indexes_matches_incremental_state() {
    let utxos = ShardedUtxoSet::new(0, None);

    // Séquence réaliste d'opérations
    utxos
        .add(out_id("aa0001", 0), pms_output("Alice", "100.00000000"))
        .await;
    utxos
        .add(out_id("bb0001", 0), pms_output("Bob", "50.00000000"))
        .await;
    utxos
        .add(
            out_id("cc0001", 0),
            token_output("Alice", "1000.00000000", "edenite"),
        )
        .await;
    utxos
        .add(
            out_id("dd0001", 0),
            token_output("Charlie", "200.0000", "gold"),
        )
        .await;

    // Apply diff : Alice envoie 30 PMS à Bob
    utxos
        .apply_diff(
            &[out_id("aa0001", 0)],
            &[
                (out_id("tx0001", 0), pms_output("Bob", "30.00000000")),
                (out_id("tx0001", 1), pms_output("Alice", "70.00000000")),
            ],
        )
        .await;

    // Remove un UTXO
    utxos.remove(&out_id("dd0001", 0)).await;

    // Snapshot de l'état incrémental AVANT rebuild
    let supply_before = utxos.circulating_supply().await;
    let eden_supply_before = utxos.circulating_supply_by_asset(Some("edenite")).await;
    let gold_supply_before = utxos.circulating_supply_by_asset(Some("gold")).await;
    let alice_pms_before = utxos.balance_by_address("Alice").await;
    let bob_pms_before = utxos.balance_by_address("Bob").await;
    let alice_utxos_before = utxos.utxos_by_address("Alice").await;
    let bob_utxos_before = utxos.utxos_by_address("Bob").await;

    // Rebuild depuis les shards
    utxos.rebuild_indexes().await;

    // Vérifier que tout est identique
    let supply_after = utxos.circulating_supply().await;
    let eden_supply_after = utxos.circulating_supply_by_asset(Some("edenite")).await;
    let gold_supply_after = utxos.circulating_supply_by_asset(Some("gold")).await;
    let alice_pms_after = utxos.balance_by_address("Alice").await;
    let bob_pms_after = utxos.balance_by_address("Bob").await;
    let alice_utxos_after = utxos.utxos_by_address("Alice").await;
    let bob_utxos_after = utxos.utxos_by_address("Bob").await;

    assert_eq!(
        supply_before, supply_after,
        "PMS supply mismatch after rebuild"
    );
    assert_eq!(
        eden_supply_before, eden_supply_after,
        "EDEN supply mismatch"
    );
    assert_eq!(
        gold_supply_before, gold_supply_after,
        "GOLD supply mismatch"
    );
    assert_eq!(
        alice_pms_before, alice_pms_after,
        "Alice PMS balance mismatch"
    );
    assert_eq!(bob_pms_before, bob_pms_after, "Bob PMS balance mismatch");
    assert_eq!(
        alice_utxos_before.len(),
        alice_utxos_after.len(),
        "Alice UTXO count mismatch"
    );
    assert_eq!(
        bob_utxos_before.len(),
        bob_utxos_after.len(),
        "Bob UTXO count mismatch"
    );

    // Valeurs attendues
    assert_eq!(alice_pms_after, Decimal::from_str("70.00000000").unwrap());
    assert_eq!(bob_pms_after, Decimal::from_str("80.00000000").unwrap());
    assert_eq!(
        eden_supply_after.0,
        Decimal::from_str("1000.00000000").unwrap()
    );
    assert_eq!(gold_supply_after.0, Decimal::ZERO, "gold was removed");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 8 : Stress test haute contention — simule le workload VPS
//
// 100 adresses, 500 transactions appliquées via apply_diff,
// avec 8 lecteurs concurrents qui requêtent des balances aléatoires.
// Vérifie la non-régression du deadlock et la cohérence finale.
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn stress_high_contention_no_deadlock() {
    let utxos = Arc::new(ShardedUtxoSet::new(0, None));

    // Seed : 100 adresses × 10 PMS chacune = 1000 PMS
    for i in 0u32..100 {
        let txid = format!("{:02x}seed{:04}", i % 256, i);
        let addr = format!("addr_{}", i);
        utxos
            .add(out_id(&txid, 0), pms_output(&addr, "10.00000000"))
            .await;
    }

    let initial_supply = utxos.circulating_supply().await;
    assert_eq!(
        initial_supply.0,
        Decimal::from_str("1000.00000000").unwrap()
    );

    let result = timeout(Duration::from_secs(15), async {
        let mut handles = Vec::new();

        // 8 lecteurs : requêtent des balances et utxos sur des adresses variées
        for reader_id in 0u32..8 {
            let utxos = utxos.clone();
            handles.push(tokio::spawn(async move {
                for i in 0u32..500 {
                    let addr = format!("addr_{}", (reader_id * 13 + i) % 100);
                    match i % 3 {
                        0 => {
                            let _b = utxos.balance_by_address(&addr).await;
                        }
                        1 => {
                            let _u = utxos.utxos_by_address(&addr).await;
                        }
                        _ => {
                            let _s = utxos.circulating_supply().await;
                        }
                    }
                    tokio::task::yield_now().await;
                }
            }));
        }

        // 4 écrivains : transactions entre adresses aléatoires
        for writer_id in 0u32..4 {
            let utxos = utxos.clone();
            handles.push(tokio::spawn(async move {
                for round in 0u32..200 {
                    let from_addr = format!("addr_{}", (writer_id * 7 + round) % 100);
                    let to_addr = format!("addr_{}", (writer_id * 11 + round + 1) % 100);
                    let spend_txid = format!(
                        "{:02x}tx{}r{}",
                        (writer_id * 60 + round) % 256,
                        writer_id,
                        round
                    );
                    let out_txid = format!(
                        "{:02x}out{}r{}",
                        (writer_id * 60 + round) % 256,
                        writer_id,
                        round
                    );

                    // Crée un UTXO puis le transfère (net supply = 0)
                    utxos
                        .add(out_id(&spend_txid, 0), pms_output(&from_addr, "0.01000000"))
                        .await;

                    let spends = vec![out_id(&spend_txid, 0)];
                    let creates = vec![(out_id(&out_txid, 0), pms_output(&to_addr, "0.01000000"))];
                    utxos.apply_diff(&spends, &creates).await;

                    tokio::task::yield_now().await;
                }
            }));
        }

        for h in handles {
            h.await.unwrap();
        }
    })
    .await;

    assert!(
        result.is_ok(),
        "DEADLOCK: stress test with 100 addresses hung for 15s"
    );

    // Chaque writer a ajouté 0.01 (add) et fait un transfer net 0 (apply_diff).
    // Donc le supply total devrait être : 1000 + 4×200×0.01 = 1000 + 8 = 1008
    let (final_supply, _) = utxos.circulating_supply().await;
    assert_eq!(
        final_supply,
        Decimal::from_str("1008.00000000").unwrap(),
        "supply should be initial 1000 + net 8 from add operations"
    );
}
