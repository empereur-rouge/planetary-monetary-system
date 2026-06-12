//! Failover / replay / déterminisme (audit Section 9) — proof-of-reserves.
//!
//! Invariant maître : **rejouer le DAG depuis le store reconstruit EXACTEMENT
//! les mêmes soldes**. C'est la base de la preuve de réserves — après un crash
//! ou un redémarrage, l'état (UTXOs / soldes) relu depuis RocksDB doit être
//! identique bit-pour-bit à l'état RAM d'avant l'arrêt, sans UTXO fantôme
//! (déjà dépensé mais ré-apparu) ni UTXO perdu (créé mais absent).
//!
//! Les deux tests :
//!   - `replay_from_store_reconstructs_identical_balances` : un adapter A
//!     persiste 3 mints + 1 transfert, puis un adapter B est reconstruit via
//!     LE VRAI chemin de redémarrage prod (`bootstrap_from_store` +
//!     `iter_all_utxos` → `utxos.add` → `rebuild_indexes`, cf.
//!     `pms-ledger/src/instance.rs`). Les soldes de B doivent égaler ceux de A
//!     ET des valeurs golden hardcodées (indépendantes de la formule prod —
//!     anti-tautologie, CLAUDE.md anti-faux-test #4).
//!   - `same_block_sequence_is_application_deterministic` : deux adapters
//!     indépendants appliquent la MÊME séquence de blocs ; soldes identiques.
//!     Isole le déterminisme d'APPLICATION (pas de disque, pas de timing).

use std::sync::Arc;
use std::time::{Duration, Instant};

use pms_config::load_config;
use pms_core::utxo::UtxoFetcher;
use pms_core::{ConcurrentDag, CoreAdapter};
use pms_interface::NetDagAdapter;
use pms_storage::rocks_store::store::RocksStore;
use pms_storage::{DagStorage, PutResult, StoredBlock};
use pms_testkit::{forge_signed_wire_block_for_test, sign_tx_inputs, test_rocks_store};
use pms_types::{Block, OutputId, PayloadEnvelope, PlainPayload, Transaction, TxInput, TxOutput};
use pms_utils::compute_block_id;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::{WireBlock, WireMeta};

type ConcreteAdapter = Arc<CoreAdapter<RocksStore>>;

/// Adapter concret prod-like sur `store`, DAG genesis neuf. `max_utxos = 0`
/// (illimité) pour que `rebuild_indexes` s'exécute au bootstrap (cf. instance.rs).
fn adapter_on(store: Arc<RocksStore>) -> (Arc<ConcurrentDag>, ConcreteAdapter) {
    let dag = Arc::new(ConcurrentDag::new_with_genesis(Block::genesis(compute_block_id)));
    let adapter = CoreAdapter::new(dag.clone(), store, 0, None);
    (dag, adapter)
}

/// Persiste le genesis dans le store (bootstrap propre de l'adapter B).
async fn persist_genesis(store: &RocksStore, meta: &WireMeta) -> anyhow::Result<String> {
    let genesis = Block::genesis(compute_block_id);
    let sb = StoredBlock {
        id: genesis.id.clone(),
        parents: genesis.parents.clone(),
        payload_json: serde_json::to_string(&genesis.payload).ok(),
        nonce: genesis.nonce,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    };
    store.append_block_atomic(&sb).await?;
    Ok(genesis.id)
}

/// La séquence de blocs partagée par les deux tests : 3 mints natifs + 1
/// transfert (A dépense ses 1000 → D, fee 0). Renvoie les WireBlocks dans
/// l'ordre d'application + les 4 adresses (A,B,C,D).
struct Scenario {
    blocks: Vec<WireBlock>,
    addr_a: String,
    addr_b: String,
    addr_c: String,
    addr_d: String,
}

fn build_scenario(genesis_id: &str, meta: &WireMeta) -> Scenario {
    // Admin/minteur = signataire des blocs ; possède aussi A (pour pouvoir
    // dépenser le mint de A dans le transfert).
    let admin = Wallet::from_seed(&[99u8; 32], None).unwrap();
    let addr_a = admin.get_address("8e");
    let addr_b = Wallet::from_seed(&[11u8; 32], None).unwrap().get_address("8e");
    let addr_c = Wallet::from_seed(&[22u8; 32], None).unwrap().get_address("8e");
    let addr_d = Wallet::from_seed(&[33u8; 32], None).unwrap().get_address("8e");

    let mint = |to: &str, amt: &str| {
        Some(PayloadEnvelope::Plain(PlainPayload::Mint {
            outputs: vec![TxOutput::new(to, amt, None)],
        }))
    };
    let forge = |parents: Vec<String>, nonce: u64, payload: Option<PayloadEnvelope>| {
        forge_signed_wire_block_for_test(parents, meta, &admin, nonce, payload)
    };

    // 3 mints chaînés (single-parent : enforce_single_writer dev rejette 2+).
    let m1 = forge(vec![genesis_id.to_string()], 1, mint(&addr_a, "1000"));
    let m2 = forge(vec![m1.id.clone()], 2, mint(&addr_b, "2500"));
    let m3 = forge(vec![m2.id.clone()], 3, mint(&addr_c, "777"));

    // Transfert : A dépense l'UTXO du mint #1 (out 0 de m1) → 1000 vers D, fee 0.
    let spend = OutputId { txid: m1.id.clone(), index: 0 };
    let tx_unsigned = Transaction {
        inputs: vec![TxInput { out: spend }],
        outputs: vec![TxOutput::new(&addr_d, "1000", None)],
        fee: "0".into(),
        unlocks: vec![],
    };
    let tx = sign_tx_inputs(&admin, &tx_unsigned, &meta.network_id);
    let t1 = forge(vec![m3.id.clone()], 4, Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx))));

    Scenario { blocks: vec![m1, m2, m3, t1], addr_a, addr_b, addr_c, addr_d }
}

/// Persiste toute la séquence via l'adapter ; chaque persist doit Inserted.
async fn persist_all(adapter: &ConcreteAdapter, blocks: &[WireBlock]) -> anyhow::Result<()> {
    for (i, wb) in blocks.iter().enumerate() {
        let r = adapter.persist_block(wb).await?;
        assert!(
            matches!(r, PutResult::Inserted),
            "bloc #{i} doit être Inserted (séquence neuve), got {r:?}"
        );
    }
    Ok(())
}

/// Attend (poll, pas de sleep fixe) que le background persist ait écrit les
/// `expected` blocs dans le store, puis flush le WAL. Échoue si timeout.
async fn wait_persisted(store: &RocksStore, expected: usize) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let n = store.all_block_ids().await?.len();
        if n >= expected {
            break;
        }
        if Instant::now() > deadline {
            anyhow::bail!("timeout: seulement {n}/{expected} blocs persistés sur disque");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    store.flush_wal().await?;
    Ok(())
}

/// Reconstruit un adapter depuis `store` via LE VRAI chemin de redémarrage prod
/// (cf. `pms-ledger/src/instance.rs`) : bootstrap DAG + rehydrate UTXO set depuis
/// la CF `utxo` (autoritaire, jamais prunée) + rebuild_indexes.
async fn restart_adapter(store: Arc<RocksStore>) -> anyhow::Result<ConcreteAdapter> {
    let dag = Arc::new(ConcurrentDag::bootstrap_from_store::<RocksStore>(&store).await?);
    let store_for_fallback = store.clone();
    let fallback: UtxoFetcher = Arc::new(move |txid: &str, index: u32| {
        store_for_fallback.get_utxo(txid, index).ok().flatten().map(|uv| uv.into_tx_output())
    });
    let adapter = CoreAdapter::new(dag, store.clone(), 0, Some(fallback));
    // Rehydrate le UTXO set RAM depuis la CF utxo persistée.
    for (txid, idx, uv) in store.iter_all_utxos()? {
        adapter
            .utxos
            .add(OutputId { txid, index: idx }, uv.into_tx_output())
            .await;
    }
    adapter.utxos.rebuild_indexes().await;
    Ok(adapter)
}

/// Snapshot (A,B,C,D, supply native) des soldes d'un adapter.
async fn balances(
    a: &ConcreteAdapter,
    s: &Scenario,
) -> (String, String, String, String, String) {
    (
        a.balance_by_address(&s.addr_a).await.to_string(),
        a.balance_by_address(&s.addr_b).await.to_string(),
        a.balance_by_address(&s.addr_c).await.to_string(),
        a.balance_by_address(&s.addr_d).await.to_string(),
        a.circulating_supply().await.0.to_string(),
    )
}

/// S9 — rejouer depuis le store reconstruit exactement les mêmes soldes.
#[tokio::test]
async fn replay_from_store_reconstructs_identical_balances() -> anyhow::Result<()> {
    let admin = Wallet::from_seed(&[99u8; 32], None).unwrap();
    unsafe { std::env::set_var("PMS_TEST_ADMIN_PUBKEY", admin.encoded_public_key()) };

    // `tr` (guard tempdir) DOIT rester vivant tant que les adapters A et B
    // relisent ce store — sinon le répertoire RocksDB serait supprimé.
    let tr = test_rocks_store("replay").await?;
    let store = tr.store.clone();
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);
    let genesis_id = persist_genesis(&store, &meta).await?;

    // ── Adapter A : applique la séquence (état "vivant" d'avant crash) ──
    let (_dag_a, adapter_a) = adapter_on(store.clone());
    let scenario = build_scenario(&genesis_id, &meta);
    persist_all(&adapter_a, &scenario.blocks).await?;

    let live = balances(&adapter_a, &scenario).await;
    println!(
        "LIVE  (adapter A) : A={} B={} C={} D={} supply={}",
        live.0, live.1, live.2, live.3, live.4
    );

    // ── Crash + redémarrage : adapter B reconstruit depuis le disque ──
    // genesis (sync) + 4 blocs (background persist) = 5 blocs attendus.
    wait_persisted(&store, 5).await?;
    let adapter_b = restart_adapter(store.clone()).await?;

    let replayed = balances(&adapter_b, &scenario).await;
    println!(
        "REPLAY(adapter B) : A={} B={} C={} D={} supply={}",
        replayed.0, replayed.1, replayed.2, replayed.3, replayed.4
    );

    // 1) Déterminisme total : B == A, champ par champ.
    assert_eq!(replayed, live, "le replay depuis le store doit reconstruire EXACTEMENT les soldes live");

    // 2) Valeurs golden indépendantes (anti-tautologie #4) :
    //    A a dépensé son unique UTXO → 0 (PAS de fantôme du mint dépensé) ;
    //    B/C reçus par mint ; D reçu par transfert ; supply = 1000+2500+777.
    assert_eq!(replayed.0, "0", "A : UTXO dépensé ne doit PAS ré-apparaître (no ghost)");
    assert_eq!(replayed.1, "2500", "B : mint reçu");
    assert_eq!(replayed.2, "777", "C : mint reçu");
    assert_eq!(replayed.3, "1000", "D : transfert reçu (no lost utxo)");
    assert_eq!(replayed.4, "4277", "supply native = somme des mints (transfert fee 0 ⇒ inchangée)");

    drop(tr); // garde explicite : le store a vécu jusqu'ici.
    Ok(())
}

/// S9 — la même séquence de blocs appliquée à deux adapters indépendants donne
/// des soldes identiques (déterminisme d'application, sans disque ni timing).
#[tokio::test]
async fn same_block_sequence_is_application_deterministic() -> anyhow::Result<()> {
    let admin = Wallet::from_seed(&[99u8; 32], None).unwrap();
    unsafe { std::env::set_var("PMS_TEST_ADMIN_PUBKEY", admin.encoded_public_key()) };

    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    // Même genesis_id des deux côtés → mêmes ids de blocs (content-addressing).
    let genesis_id = Block::genesis(compute_block_id).id;
    let scenario = build_scenario(&genesis_id, &meta);

    let tr1 = test_rocks_store("determ-1").await?;
    let tr2 = test_rocks_store("determ-2").await?;
    let (_d1, a1) = adapter_on(tr1.store.clone());
    let (_d2, a2) = adapter_on(tr2.store.clone());
    persist_all(&a1, &scenario.blocks).await?;
    persist_all(&a2, &scenario.blocks).await?;

    let b1 = balances(&a1, &scenario).await;
    let b2 = balances(&a2, &scenario).await;
    println!("adapter#1 : A={} B={} C={} D={} supply={}", b1.0, b1.1, b1.2, b1.3, b1.4);
    println!("adapter#2 : A={} B={} C={} D={} supply={}", b2.0, b2.1, b2.2, b2.3, b2.4);

    assert_eq!(b1, b2, "deux applications indépendantes de la même séquence doivent coïncider");
    // Ancre golden (sinon b1==b2 passerait même si les deux régressaient à 0).
    assert_eq!(b1.3, "1000", "D reçoit 1000");
    assert_eq!(b1.4, "4277", "supply native = 4277");
    Ok(())
}
