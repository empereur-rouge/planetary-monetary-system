//! Tests for ConcurrentDag.

use super::*;
use std::sync::atomic::Ordering;


fn make_block(id: &str, parents: Vec<&str>) -> Block {
    Block {
        id: id.to_string(),
        parents: parents.into_iter().map(|s| s.to_string()).collect(),
        payload: None,
        nonce: 0,
        metadata: None,
        signer_pk: None,
        signature: None,
    }
}

#[test]
fn test_insert_and_get() {
    let dag = ConcurrentDag::new();

    let genesis = make_block("genesis", vec![]);

    assert!(dag.insert_block(genesis.clone()));
    assert!(!dag.insert_block(genesis.clone())); // Duplicate

    assert!(dag.contains_block("genesis"));
    assert!(!dag.contains_block("nonexistent"));

    let retrieved = dag.get_block("genesis").unwrap();
    assert_eq!(retrieved.id, "genesis");
}

#[test]
fn test_children_tracking() {
    let dag = ConcurrentDag::new();

    dag.insert_block(make_block("genesis", vec![]));
    dag.insert_block(make_block("child1", vec!["genesis"]));

    assert_eq!(dag.get_children_count("genesis"), 1);
    assert_eq!(dag.get_children("genesis"), vec!["child1".to_string()]);
}

#[test]
fn test_spent_outpoints() {
    let dag = ConcurrentDag::new();

    assert!(!dag.is_spent("tx1", 0));

    dag.mark_spent("tx1", 0);
    assert!(dag.is_spent("tx1", 0));
    assert!(!dag.is_spent("tx1", 1));
}

#[test]
fn test_pruning_respects_capacity() {
    // DAG with max 5 blocks
    let dag = ConcurrentDag::with_capacity(5);

    // Build a chain: genesis -> b1 -> b2 -> b3 -> b4 -> b5 -> b6 -> b7
    dag.insert_block(make_block("genesis", vec![]));
    dag.insert_block(make_block("b1", vec!["genesis"]));
    dag.insert_block(make_block("b2", vec!["b1"]));
    dag.insert_block(make_block("b3", vec!["b2"]));
    dag.insert_block(make_block("b4", vec!["b3"]));
    // At this point: 5 blocks, at capacity

    dag.insert_block(make_block("b5", vec!["b4"]));
    dag.insert_block(make_block("b6", vec!["b5"]));
    dag.insert_block(make_block("b7", vec!["b6"]));
    // Force prune (insert_block only auto-prunes every PRUNE_CHECK_INTERVAL)
    dag.prune_oldest();

    // Should have pruned down to ~5 blocks
    assert!(
        dag.len() <= 6,
        "DAG should be pruned to around max_blocks, got {}",
        dag.len()
    );

    // The tip (b7) must still exist
    assert!(dag.contains_block("b7"), "Latest tip should not be pruned");

    // Oldest blocks should be gone
    assert!(
        !dag.contains_block("genesis"),
        "Genesis (deeply buried) should be pruned"
    );
}

#[test]
fn test_pruning_removes_oldest_tips_too() {
    // DAG with max 3 blocks
    let dag = ConcurrentDag::with_capacity(3);

    // Fan-out: genesis -> [t1, t2, t3, t4]
    dag.insert_block(make_block("genesis", vec![]));
    dag.insert_block(make_block("t1", vec!["genesis"]));
    dag.insert_block(make_block("t2", vec!["genesis"]));
    dag.insert_block(make_block("t3", vec!["genesis"]));
    dag.insert_block(make_block("t4", vec!["genesis"]));
    dag.prune_oldest();

    // 5 blocks, capacity 3 → remove 2 oldest (genesis, t1)
    // Remaining: t2, t3, t4
    assert_eq!(dag.len(), 3, "should prune to exactly capacity");
    assert!(
        !dag.contains_block("genesis"),
        "genesis (oldest) should be pruned"
    );
    assert!(
        !dag.contains_block("t1"),
        "t1 (2nd oldest) should be pruned"
    );

    // Latest tips survive (they're at the back of insertion_order)
    assert!(dag.contains_block("t4"), "t4 (latest) should survive");
}

#[test]
fn test_children_count_preserved_on_out_of_order_insert() {
    // Verifies the fix for the bootstrap bug: children loaded before parents.
    // all_block_ids() returns lexicographic order (hash-based = random),
    // so a child can be loaded before its parent. insert_block must NOT
    // overwrite the parent's children_count.
    let dag = ConcurrentDag::new(); // unlimited, just checking counts

    // Load child FIRST, then parent
    dag.insert_block(make_block("child", vec!["parent"]));
    // At this point, parent's children_count == 1 (set by child)
    assert_eq!(
        dag.children_count
            .get("parent")
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0),
        1,
        "parent children_count should be 1 after child loaded"
    );

    // Now load parent - must NOT overwrite children_count to 0
    dag.insert_block(make_block("parent", vec!["genesis"]));
    assert_eq!(
        dag.children_count
            .get("parent")
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0),
        1,
        "parent children_count must be preserved (not overwritten to 0)"
    );
}

#[test]
fn test_pruning_with_out_of_order_bootstrap() {
    // Simulates a real bootstrap: 20 blocks in a chain, loaded in shuffled
    // order (children before parents), then pruned to max_blocks=8.
    let dag = ConcurrentDag::with_capacity(8);

    // Chain: g -> b1 -> b2 -> ... -> b19
    // Load in "random" order to simulate lexicographic hash order
    let shuffled_order: Vec<usize> = vec![
        12, 5, 18, 1, 9, 15, 3, 7, 0, 11, 16, 6, 19, 2, 13, 8, 4, 17, 10, 14,
    ];

    for &i in &shuffled_order {
        let id = format!("b{}", i);
        let parents = if i == 0 {
            vec![]
        } else {
            vec![format!("b{}", i - 1)]
        };
        dag.insert_block(Block {
            id,
            parents,
            payload: None,
            nonce: 0,
            metadata: None,
            signer_pk: None,
            signature: None,
        });
    }

    assert_eq!(dag.len(), 20);

    // Force post-bootstrap prune
    dag.prune_oldest();

    // Must prune to ~8 blocks
    assert!(
        dag.len() <= 10,
        "DAG should be pruned to around 8, got {}",
        dag.len()
    );

    // Tip (b19) must survive
    assert!(dag.contains_block("b19"), "tip b19 should survive");
}

#[test]
fn test_unlimited_capacity_no_pruning() {
    let dag = ConcurrentDag::new(); // unlimited

    for i in 0..100 {
        let parents = if i == 0 {
            vec![]
        } else {
            vec![format!("b{}", i - 1)]
        };
        dag.insert_block(Block {
            id: format!("b{}", i),
            parents,
            payload: None,
            nonce: 0,
            metadata: None,
            signer_pk: None,
            signature: None,
        });
    }
    dag.prune_oldest();

    assert_eq!(dag.len(), 100, "Unlimited DAG should keep all blocks");
}

// ─── Ghost entries ─────────────────────────────────────────────

#[test]
fn test_ghost_entries_do_not_block_pruning() {
    // After a first prune, removed block IDs become "ghosts" in
    // insertion_order. The next prune must skip them without counting
    // them as tips, so subsequent prune cycles keep working.
    let dag = ConcurrentDag::with_capacity(50);

    // Build a chain of 100 blocks
    for i in 0..100 {
        let parents = if i == 0 {
            vec![]
        } else {
            vec![format!("b{}", i - 1)]
        };
        dag.insert_block(make_block(
            &format!("b{}", i),
            parents.iter().map(|s| s.as_str()).collect(),
        ));
    }
    assert_eq!(dag.len(), 100);

    // First prune: removes ~50 oldest
    dag.prune_oldest();
    let after_first = dag.len();
    assert!(
        after_first <= 52,
        "first prune should reduce to ~50, got {}",
        after_first
    );

    // Insert 30 more blocks (continuing the chain)
    for i in 100..130 {
        dag.insert_block(make_block(&format!("b{}", i), vec![&format!("b{}", i - 1)]));
    }

    // Second prune: must still work despite ghost entries from first prune
    dag.prune_oldest();
    let after_second = dag.len();
    assert!(
        after_second <= 55,
        "second prune should keep ~50, got {} (ghost entries blocking?)",
        after_second
    );

    // Tip must survive
    assert!(dag.contains_block("b129"), "tip b129 must survive");
}

// ─── Continuous operation (simulates a running node) ───────────

#[test]
fn test_continuous_insert_and_prune_cycles() {
    // Simulates a node continuously receiving blocks: capacity 30,
    // insert 200 blocks with a prune every 20 inserts.
    let dag = ConcurrentDag::with_capacity(30);

    for i in 0..200 {
        let parents = if i == 0 {
            vec![]
        } else {
            vec![format!("b{}", i - 1)]
        };
        dag.insert_block(make_block(
            &format!("b{}", i),
            parents.iter().map(|s| s.as_str()).collect(),
        ));

        // Prune every 20 inserts (simulating amortized pruning)
        if i > 0 && i % 20 == 0 {
            dag.prune_oldest();
        }
    }
    dag.prune_oldest();

    assert!(
        dag.len() <= 35,
        "after 200 inserts with periodic prunes, should be ~30, got {}",
        dag.len()
    );
    assert!(dag.contains_block("b199"), "latest tip must survive");
    assert!(!dag.contains_block("b0"), "genesis should be long pruned");
}

// ─── Diamond / multi-parent topology ───────────────────────────

#[test]
fn test_pruning_diamond_dag() {
    //   g
    //  / \
    // a   b
    //  \ /
    //   c
    //   |
    //   d
    //   |
    //   e   (tip)
    let dag = ConcurrentDag::with_capacity(3);

    dag.insert_block(make_block("g", vec![]));
    dag.insert_block(make_block("a", vec!["g"]));
    dag.insert_block(make_block("b", vec!["g"]));
    dag.insert_block(make_block("c", vec!["a", "b"])); // diamond merge
    dag.insert_block(make_block("d", vec!["c"]));
    dag.insert_block(make_block("e", vec!["d"]));
    dag.prune_oldest();

    // e is the only tip, g/a/b should be prunable
    assert!(dag.contains_block("e"), "tip e must survive");
    assert!(dag.len() <= 4, "should prune to ~3, got {}", dag.len());

    // Verify children_count consistency: for surviving blocks with parents
    // still in DAG, the parent's children_count should be > 0
    if dag.contains_block("d") {
        assert!(
            dag.get_children_count("d") > 0,
            "d has child e, count must be > 0"
        );
    }
}

// ─── Wide DAG with many concurrent tips ────────────────────────

#[test]
fn test_pruning_wide_dag_many_tips() {
    // Simulates ~20 concurrent agents, each creating a long branch.
    // Oldest branches (agents 0-9) get pruned entirely, including their tips.
    // Recent branches (agents 10-19) survive because they're at the back.
    let dag = ConcurrentDag::with_capacity(500);

    // Shared backbone: g -> b1 -> b2
    dag.insert_block(make_block("g", vec![]));
    dag.insert_block(make_block("b1", vec!["g"]));
    dag.insert_block(make_block("b2", vec!["b1"]));

    // 20 agents each create 50 blocks branching off b2
    for agent in 0..20 {
        let first = format!("a{}_0", agent);
        dag.insert_block(make_block(&first, vec!["b2"]));
        for step in 1..50 {
            let id = format!("a{}_{}", agent, step);
            let parent = format!("a{}_{}", agent, step - 1);
            dag.insert_block(make_block(&id, vec![&parent]));
        }
    }
    // Total: 3 backbone + 1000 agent blocks = 1003

    dag.prune_oldest();

    assert!(
        dag.len() <= 510,
        "wide DAG should prune to ~500, got {}",
        dag.len()
    );

    // Recent branch tips (agents 10-19) survive — they're at the back
    for agent in 10..20 {
        let tip = format!("a{}_49", agent);
        assert!(
            dag.contains_block(&tip),
            "recent agent {} tip should survive",
            agent
        );
    }

    // Oldest backbone blocks are pruned
    assert!(!dag.contains_block("g"), "oldest backbone should be pruned");
}

// ─── Spent outpoints preserved after pruning ───────────────────

#[test]
fn test_spent_outpoints_preserved_after_pruning() {
    let dag = ConcurrentDag::with_capacity(5);

    dag.insert_block(make_block("g", vec![]));
    dag.mark_spent("tx_old", 0);
    dag.mark_spent("tx_old", 1);

    // Build a chain past capacity
    for i in 1..=10 {
        let parent = if i == 1 {
            "g".to_string()
        } else {
            format!("b{}", i - 1)
        };
        dag.insert_block(make_block(&format!("b{}", i), vec![&parent]));
    }
    dag.prune_oldest();

    // g is pruned, but spent_outpoints must survive for double-spend detection
    assert!(!dag.contains_block("g"), "g should be pruned");
    assert!(
        dag.is_spent("tx_old", 0),
        "spent outpoint must survive pruning"
    );
    assert!(
        dag.is_spent("tx_old", 1),
        "spent outpoint must survive pruning"
    );
}

// ─── Amortized pruning via PRUNE_CHECK_INTERVAL ────────────────

#[test]
fn test_amortized_pruning_triggers_automatically() {
    // With PRUNE_CHECK_INTERVAL = 1000, after inserting 1050 blocks
    // into a DAG with capacity 100, pruning should have triggered
    // automatically at least once (at insert #1000).
    let dag = ConcurrentDag::with_capacity(100);

    for i in 0..1050 {
        let parents = if i == 0 {
            vec![]
        } else {
            vec![format!("b{}", i - 1)]
        };
        dag.insert_block(make_block(
            &format!("b{}", i),
            parents.iter().map(|s| s.as_str()).collect(),
        ));
    }

    // Amortized prune should have triggered at insert #1000.
    // At that point len was 1001, it should have pruned ~901 blocks.
    // Then 50 more inserts bring it to ~150.
    // We don't call prune_oldest() manually here — relying on amortized.
    assert!(
        dag.len() < 200,
        "amortized pruning should have kicked in, but len = {}",
        dag.len()
    );

    assert!(dag.contains_block("b1049"), "latest tip must survive");
}

// ─── Memory cleanup: children_count/children_idx ───────────────

#[test]
fn test_pruning_cleans_children_count_and_idx() {
    let dag = ConcurrentDag::with_capacity(5);

    // Chain: g -> b1 -> b2 -> b3 -> b4 -> b5 -> b6 -> b7
    dag.insert_block(make_block("g", vec![]));
    for i in 1..=7 {
        let parent = if i == 1 {
            "g".to_string()
        } else {
            format!("b{}", i - 1)
        };
        dag.insert_block(make_block(&format!("b{}", i), vec![&parent]));
    }
    dag.prune_oldest();

    // Pruned blocks should have their children_count/children_idx removed
    for pruned_id in &["g", "b1"] {
        if !dag.contains_block(pruned_id) {
            assert!(
                dag.children_count.get(*pruned_id).is_none(),
                "{} pruned but children_count entry leaked",
                pruned_id
            );
            assert!(
                dag.children_idx.get(*pruned_id).is_none(),
                "{} pruned but children_idx entry leaked",
                pruned_id
            );
        }
    }
}

// ─── Large bootstrap with intermediate prune cycles ────────────

#[test]
fn test_large_bootstrap_with_intermediate_prunes() {
    // Simulates loading 2500 blocks from store (PRUNE_CHECK_INTERVAL=1000
    // triggers 2 intermediate prune cycles during load) with capacity 500.
    let dag = ConcurrentDag::with_capacity(500);

    // Load in chronological order (simplest case) — 2500 block chain
    for i in 0..2500 {
        let parents = if i == 0 {
            vec![]
        } else {
            vec![format!("b{}", i - 1)]
        };
        dag.insert_block(make_block(
            &format!("b{}", i),
            parents.iter().map(|s| s.as_str()).collect(),
        ));
    }

    // After the loop, amortized prune ran at inserts 0, 1000, 2000.
    // Final forced prune:
    dag.prune_oldest();

    assert!(
        dag.len() <= 510,
        "should prune to ~500 after bootstrap, got {}",
        dag.len()
    );
    assert!(dag.contains_block("b2499"), "tip must survive");
    assert!(!dag.contains_block("b0"), "old genesis should be gone");
}

// ─── Concurrent multi-threaded insert + prune ──────────────────

#[test]
fn test_concurrent_inserts_with_pruning() {
    use std::sync::Arc;
    use std::thread;

    let dag = Arc::new(ConcurrentDag::with_capacity(200));

    // Create a shared backbone
    dag.insert_block(make_block("g", vec![]));

    // 4 threads, each inserting 100 blocks
    let mut handles = vec![];
    for t in 0..4 {
        let dag = dag.clone();
        handles.push(thread::spawn(move || {
            let first = format!("t{}_0", t);
            dag.insert_block(make_block(&first, vec!["g"]));
            for i in 1..100 {
                let id = format!("t{}_{}", t, i);
                let parent = format!("t{}_{}", t, i - 1);
                dag.insert_block(make_block(&id, vec![&parent]));
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }
    // Total: 1 + 400 = 401 blocks

    // 401 < PRUNE_CHECK_INTERVAL (1000) → aucun prune amorti pendant les
    // inserts : le DAG a bien 401 blocs ici, un seul prune explicite suit.
    dag.prune_oldest();

    let surviving_tips: Vec<usize> = (0..4)
        .filter(|t| dag.contains_block(&format!("t{}_99", t)))
        .collect();
    println!(
        "prune(max=200): 401 blocs -> len={} | tips survivants={:?}",
        dag.len(),
        surviving_tips
    );

    // Borne RAM : on retire EXACTEMENT current_len - max_blocks = 401-200 = 201
    // blocs NON-tip -> len == 200, déterministe.
    assert_eq!(
        dag.len(),
        200,
        "prune doit borner à max_blocks en retirant 201 non-tips, got {}",
        dag.len()
    );

    // Contrat « protéger tous les tips » (audit S9) : les 4 frontières de
    // branche survivent TOUTES — 397 non-tips suffisent à couvrir les 201 à
    // retirer, donc AUCUN tip n'est élagué, quel que soit l'entrelacement des
    // 4 threads. (Avant le fix : `prune_oldest` élaguait un tip « ancien » par
    // ordre d'insertion -> test flaky « thread N tip must survive ».)
    assert_eq!(
        surviving_tips,
        vec![0, 1, 2, 3],
        "les 4 tips de branche doivent TOUS survivre (non-tips suffisants)"
    );
}

/// Edge S9 — flood de tips orphelins : quand les blocs NON-tip ne suffisent
/// PAS à atteindre la borne, `prune_oldest` élague les tips LES PLUS ANCIENS
/// d'abord, garde toujours >= 1 tip, et borne bien la RAM. C'est la soupape
/// qui empêche la croissance illimitée (le scénario que protéger-tous-les-tips
/// ne doit PAS réintroduire).
#[test]
fn prune_under_tip_flood_evicts_oldest_tips_keeps_recent_and_bounds_ram() {
    let dag = ConcurrentDag::with_capacity(10);
    dag.insert_block(make_block("g", vec![]));
    // 50 tips attachés au genesis (aucun n'a d'enfant). genesis devient non-tip.
    for i in 0..50 {
        dag.insert_block(make_block(&format!("tip_{i}"), vec!["g"]));
    }
    // 51 blocs (< 1000 -> pas de prune amorti), max=10. À retirer : 41.
    // non-tips = 1 (genesis), tips = 50 -> budget tips = min(41-1, 50-1) = 40.
    // On retire genesis + les 40 tips les plus anciens (tip_0..tip_39) ;
    // survivent les 10 tips les plus récents (tip_40..tip_49).
    dag.prune_oldest();

    let survivors: Vec<usize> = (0..50)
        .filter(|i| dag.contains_block(&format!("tip_{i}")))
        .collect();
    println!(
        "tip-flood prune(max=10): 51 blocs -> len={} | genesis survit={} | tips survivants={:?}",
        dag.len(),
        dag.contains_block("g"),
        survivors
    );

    // RAM bornée malgré le flood de tips.
    assert_eq!(dag.len(), 10, "RAM doit être bornée à max_blocks sous flood, got {}", dag.len());
    // >= 1 tip survit (continuité fee-distribution / parent-selection).
    assert!(!survivors.is_empty(), "au moins 1 tip doit survivre sous flood");
    // Les plus RÉCENTS survivent, les plus ANCIENS sont élagués.
    assert_eq!(
        survivors,
        (40..50).collect::<Vec<_>>(),
        "les 10 tips les plus récents survivent, les 40 plus anciens sont élagués"
    );
    assert!(!dag.contains_block("g"), "genesis (non-tip le plus ancien) doit être élagué en premier");
}

// ─── Edge: prune on empty / single-block DAG ───────────────────

#[test]
fn test_prune_empty_dag() {
    let dag = ConcurrentDag::with_capacity(10);
    dag.prune_oldest(); // must not panic
    assert_eq!(dag.len(), 0);
}

#[test]
fn test_prune_single_block_dag() {
    let dag = ConcurrentDag::with_capacity(1);
    dag.insert_block(make_block("g", vec![]));
    dag.prune_oldest();
    // len == max_blocks → nothing to prune
    assert_eq!(dag.len(), 1, "at capacity, nothing should be pruned");
}

// ─── Edge: capacity exactly at block count ─────────────────────

#[test]
fn test_prune_at_exact_capacity() {
    let dag = ConcurrentDag::with_capacity(5);
    for i in 0..5 {
        let parents = if i == 0 {
            vec![]
        } else {
            vec![format!("b{}", i - 1)]
        };
        dag.insert_block(make_block(
            &format!("b{}", i),
            parents.iter().map(|s| s.as_str()).collect(),
        ));
    }
    dag.prune_oldest();
    // Exactly at capacity — nothing to prune
    assert_eq!(dag.len(), 5, "at exact capacity, nothing should be pruned");
}

// ─── find_tips consistency after pruning ────────────────────────

#[test]
fn test_find_tips_consistent_after_pruning() {
    let dag = ConcurrentDag::with_capacity(10);

    // Build: g -> b1 -> b2 -> ... -> b19
    for i in 0..20 {
        let parents = if i == 0 {
            vec![]
        } else {
            vec![format!("b{}", i - 1)]
        };
        dag.insert_block(make_block(
            &format!("b{}", i),
            parents.iter().map(|s| s.as_str()).collect(),
        ));
    }
    dag.prune_oldest();

    let tips = dag.find_tips();

    // Every tip returned must actually exist in the DAG
    for tip in &tips {
        assert!(
            dag.contains_block(tip),
            "find_tips returned {} which is not in the DAG",
            tip
        );
    }

    // The real tip (b19) must be in the list
    assert!(
        tips.contains(&"b19".to_string()),
        "b19 should be a tip, tips = {:?}",
        tips
    );
}

// ─── Repeated prune on already-pruned DAG (idempotent) ─────────

#[test]
fn test_repeated_prune_is_idempotent() {
    let dag = ConcurrentDag::with_capacity(10);

    for i in 0..30 {
        let parents = if i == 0 {
            vec![]
        } else {
            vec![format!("b{}", i - 1)]
        };
        dag.insert_block(make_block(
            &format!("b{}", i),
            parents.iter().map(|s| s.as_str()).collect(),
        ));
    }
    dag.prune_oldest();
    let len_after_first = dag.len();

    // Pruning again without inserting anything should be a no-op
    dag.prune_oldest();
    assert_eq!(
        dag.len(),
        len_after_first,
        "second prune without new inserts must be a no-op"
    );

    dag.prune_oldest();
    assert_eq!(
        dag.len(),
        len_after_first,
        "third prune must still be a no-op"
    );
}

// ─── Production scenario: orphaned tips from concurrent agents ───

#[test]
fn test_pruning_with_massive_orphaned_tips() {
    // Reproduces the production bug: 97 concurrent agents all pick the
    // same tip as parent, creating 96 orphaned branches per tick.
    // After N ticks, ~96*N orphaned tips accumulate. The old tip-skipping
    // logic couldn't prune ANY of them (skip limit exceeded).
    let dag = ConcurrentDag::with_capacity(500);

    dag.insert_block(make_block("g", vec![]));

    let mut latest_chain = "g".to_string();

    // Simulate 20 ticks, each with 97 agents picking the same parent
    for tick in 0..20 {
        let parent = latest_chain.clone();

        // 97 agents all create a block with the same parent
        for agent in 0..97 {
            let id = format!("t{}_a{}", tick, agent);
            dag.insert_block(make_block(&id, vec![&parent]));
        }

        // Only agent 0's block becomes the next chain link
        latest_chain = format!("t{}_a0", tick);
    }

    // Total inserted: 1 (genesis) + 20*97 (agent blocks) = 1941
    // Of which: 20 chain blocks (have children) + 1920 orphaned tips + 1 genesis
    // Note: amortized pruning already ran at insert #1000, so len < 1941.

    dag.prune_oldest();

    // Must prune to ~500 despite ~1920 orphaned tips
    assert!(
        dag.len() <= 510,
        "must prune to ~500 even with massive orphaned tips, got {}",
        dag.len()
    );

    // Latest chain tip must survive
    assert!(
        dag.contains_block("t19_a0"),
        "latest chain tip must survive"
    );
}

#[test]
fn test_pruning_continuous_with_orphaned_tips() {
    // Continuous operation with orphaned tips: simulates a running node
    // where pruning triggers periodically while orphaned tips accumulate.
    let dag = ConcurrentDag::with_capacity(200);

    dag.insert_block(make_block("g", vec![]));
    let mut chain_tip = "g".to_string();
    let mut _total_inserted = 1u64;

    for tick in 0..50 {
        let parent = chain_tip.clone();

        // 10 agents, each creating a block off the same parent
        for agent in 0..10 {
            let id = format!("t{}_a{}", tick, agent);
            dag.insert_block(make_block(&id, vec![&parent]));
            _total_inserted += 1;
        }
        chain_tip = format!("t{}_a0", tick);

        // Simulate amortized pruning every 10 ticks
        if tick % 10 == 9 {
            dag.prune_oldest();
        }
    }
    dag.prune_oldest();

    assert!(
        dag.len() <= 210,
        "continuous prune should keep ~200, got {}",
        dag.len()
    );
    assert!(
        dag.contains_block("t49_a0"),
        "latest chain tip must survive"
    );
}

// ─── Tip protection: pruning never leaves the DAG tipless ────────

#[test]
fn test_pruning_preserves_at_least_one_tip_all_orphans() {
    // Critical scenario: ALL blocks in the DAG are orphaned tips
    // (no block has children). This reproduces the 03:48 AM fee
    // distribution failure: pruning removed all tips, leaving
    // find_tips() empty and fee distribution silently blocked.
    let dag = ConcurrentDag::with_capacity(3);

    // Insert 6 orphaned tips (all children of a non-existent parent)
    dag.insert_block(make_block("t1", vec!["phantom"]));
    dag.insert_block(make_block("t2", vec!["phantom"]));
    dag.insert_block(make_block("t3", vec!["phantom"]));
    dag.insert_block(make_block("t4", vec!["phantom"]));
    dag.insert_block(make_block("t5", vec!["phantom"]));
    dag.insert_block(make_block("t6", vec!["phantom"]));
    // All 6 are tips (children_count == 0)

    dag.prune_oldest();

    // At least 1 tip must survive — the DAG must NEVER be tipless
    let tips = dag.find_tips();
    assert!(
        !tips.is_empty(),
        "DAG must never be tipless after pruning! \
         blocks.len()={}, tips={:?}",
        dag.len(),
        tips
    );
    // Capacity is 3, so we should have roughly 3 blocks
    assert!(
        dag.len() >= 1 && dag.len() <= 4,
        "DAG should be close to capacity (3), got {}",
        dag.len()
    );
}

#[test]
fn test_pruning_preserves_last_tip_in_mixed_dag() {
    // Mix of non-tip blocks and exactly 1 tip. Pruning must
    // protect the single tip even if it's the oldest block.
    let dag = ConcurrentDag::with_capacity(2);

    // Chain: g -> b1 -> b2 -> b3 (tip)
    dag.insert_block(make_block("g", vec![]));
    dag.insert_block(make_block("b1", vec!["g"]));
    dag.insert_block(make_block("b2", vec!["b1"]));
    dag.insert_block(make_block("b3", vec!["b2"]));
    // Only b3 is a tip (children_count == 0)

    dag.prune_oldest();

    // b3 must survive — it's the only tip
    assert!(
        dag.contains_block("b3"),
        "the only tip (b3) must survive pruning"
    );
    let tips = dag.find_tips();
    assert!(
        !tips.is_empty(),
        "find_tips() must not be empty after pruning"
    );
}

#[test]
fn test_pruning_all_tips_capacity_one() {
    // Edge case: capacity=1 and multiple tips. Must keep exactly 1.
    let dag = ConcurrentDag::with_capacity(1);

    dag.insert_block(make_block("t1", vec![]));
    dag.insert_block(make_block("t2", vec![]));
    dag.insert_block(make_block("t3", vec![]));

    dag.prune_oldest();

    assert_eq!(dag.len(), 1, "capacity 1 should keep exactly 1 block");
    let tips = dag.find_tips();
    assert!(!tips.is_empty(), "the surviving block must be a tip");
}

// ─── Tips DashSet consistency tests ────────────────────────────────

#[test]
fn test_tips_set_consistency_chain() {
    // Chain: g -> b1 -> b2 -> b3
    // At each step, only the latest block should be a tip.
    let dag = ConcurrentDag::new();

    dag.insert_block(make_block("g", vec![]));
    let tips = dag.find_tips();
    println!("[tips_chain] after g: tips={:?}, tips_set_len={}", tips, dag.tips.len());
    assert_eq!(tips.len(), 1);
    assert!(tips.contains(&"g".to_string()));
    assert_eq!(dag.tips.len(), 1);

    dag.insert_block(make_block("b1", vec!["g"]));
    let tips = dag.find_tips();
    println!("[tips_chain] after b1: tips={:?}, tips_set_len={}", tips, dag.tips.len());
    assert_eq!(tips.len(), 1);
    assert!(tips.contains(&"b1".to_string()));
    assert!(!dag.tips.contains("g"), "g should no longer be a tip");

    dag.insert_block(make_block("b2", vec!["b1"]));
    let tips = dag.find_tips();
    println!("[tips_chain] after b2: tips={:?}, tips_set_len={}", tips, dag.tips.len());
    assert_eq!(tips.len(), 1);
    assert!(tips.contains(&"b2".to_string()));

    dag.insert_block(make_block("b3", vec!["b2"]));
    let tips = dag.find_tips();
    println!("[tips_chain] after b3: tips={:?}, tips_set_len={}", tips, dag.tips.len());
    assert_eq!(tips.len(), 1);
    assert!(tips.contains(&"b3".to_string()));
}

#[test]
fn test_tips_set_fan_out_and_merge() {
    // Fan-out: g -> [t1, t2, t3, t4]
    // Then merge: m(t1, t2)
    // Tips should be: [t3, t4, m]
    let dag = ConcurrentDag::new();

    dag.insert_block(make_block("g", vec![]));
    dag.insert_block(make_block("t1", vec!["g"]));
    dag.insert_block(make_block("t2", vec!["g"]));
    dag.insert_block(make_block("t3", vec!["g"]));
    dag.insert_block(make_block("t4", vec!["g"]));

    let tips = dag.find_tips();
    println!("[tips_fanout] after fan-out: tips={:?}", tips);
    assert_eq!(tips.len(), 4, "4 branches = 4 tips");
    assert!(!dag.tips.contains("g"), "g has 4 children, not a tip");

    // Merge t1 + t2
    dag.insert_block(make_block("m", vec!["t1", "t2"]));
    let tips = dag.find_tips();
    println!("[tips_fanout] after merge: tips={:?}", tips);
    assert_eq!(tips.len(), 3, "merge consumed 2 tips, added 1 = 3 total");
    assert!(tips.contains(&"t3".to_string()));
    assert!(tips.contains(&"t4".to_string()));
    assert!(tips.contains(&"m".to_string()));
    assert!(!tips.contains(&"t1".to_string()), "t1 now has a child");
    assert!(!tips.contains(&"t2".to_string()), "t2 now has a child");
}

#[test]
fn test_tips_set_after_pruning() {
    // With capacity=5, insert 10 blocks (chain).
    // After pruning, tips should be consistent with blocks in DAG.
    let dag = ConcurrentDag::with_capacity(5);

    let mut parent = "g".to_string();
    dag.insert_block(make_block("g", vec![]));
    for i in 1..10 {
        let id = format!("b{}", i);
        dag.insert_block(make_block(&id, vec![&parent]));
        parent = id;
    }

    // Force pruning
    dag.prune_oldest();

    let tips = dag.find_tips();
    println!(
        "[tips_prune] dag_len={}, tips={:?}, tips_set_len={}",
        dag.len(), tips, dag.tips.len()
    );

    // Every tip must exist in the DAG
    for tip in &tips {
        assert!(
            dag.contains_block(tip),
            "tip {} not in DAG after pruning",
            tip
        );
    }

    // Every block in tips DashSet must be in the DAG
    for entry in dag.tips.iter() {
        assert!(
            dag.blocks.contains_key(entry.key()),
            "tips set contains {} which is not in blocks",
            entry.key()
        );
    }

    // At least 1 tip must exist
    assert!(!tips.is_empty(), "at least 1 tip must survive pruning");
}

#[test]
fn test_tips_set_matches_children_count() {
    // Verify that the tips DashSet exactly matches children_count == 0
    // for all blocks in the DAG. This validates incremental maintenance.
    let dag = ConcurrentDag::new();

    // Build a complex topology
    dag.insert_block(make_block("g", vec![]));
    dag.insert_block(make_block("a", vec!["g"]));
    dag.insert_block(make_block("b", vec!["g"]));
    dag.insert_block(make_block("c", vec!["a", "b"]));
    dag.insert_block(make_block("d", vec!["a"]));
    dag.insert_block(make_block("e", vec!["c"]));

    // Compute expected tips from children_count (ground truth)
    let expected_tips: Vec<String> = dag
        .children_count
        .iter()
        .filter(|e| {
            dag.blocks.contains_key(e.key())
                && e.value().load(Ordering::Relaxed) == 0
        })
        .map(|e| e.key().clone())
        .collect();

    let actual_tips: Vec<String> = dag.tips.iter().map(|r| r.key().clone()).collect();

    println!(
        "[tips_match] expected={:?}, actual={:?}",
        expected_tips, actual_tips
    );

    let mut expected_sorted = expected_tips.clone();
    expected_sorted.sort();
    let mut actual_sorted = actual_tips.clone();
    actual_sorted.sort();

    assert_eq!(
        expected_sorted, actual_sorted,
        "tips DashSet must exactly match children_count==0 blocks"
    );
}
