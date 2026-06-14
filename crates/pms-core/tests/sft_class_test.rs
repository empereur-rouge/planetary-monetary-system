//! Semi-fongibles — validation protocole de `SftClassCreate` (spec semi-fungibles §5).
//!
//! Forge directement des blocs `SftClassCreate` via `persist_block` (mode dev :
//! `coordinator_public_key = None` → autorité coordinator court-circuitée, on isole
//! la validation de classe, pas l'auth). Couvre S2 (validation) + l'unicité.
//!
//! Run : `cargo test -p pms-core --test sft_class_test -- --nocapture`

use std::sync::Arc;

use pms_config::load_config;
use pms_core::{ConcurrentDag, CoreAdapter};
use pms_interface::NetDagAdapter;
use pms_storage::rocks_store::store::{RocksMemoryConfig, RocksStore};
use pms_storage::{PutResult, SftClassStorage};
use pms_testkit::forge_signed_wire_block_for_test;
use pms_types::{Block, PayloadEnvelope, PlainPayload, SftClass};
use pms_utils::compute_block_id;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireMeta;

type DagRef = Arc<ConcurrentDag>;

async fn setup(tag: &str) -> anyhow::Result<(DagRef, Arc<dyn NetDagAdapter>, Arc<RocksStore>, WireMeta)>
{
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join(format!("rocks-sft-{tag}"));
    let store = Arc::new(
        RocksStore::new(
            db_path.to_string_lossy().as_ref(),
            256,
            "pms:test",
            None,
            &RocksMemoryConfig::default(),
        )
        .await?,
    );
    std::mem::forget(dir);
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);
    let dag: DagRef = Arc::new(ConcurrentDag::new_with_genesis(Block::genesis(compute_block_id)));
    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone(), 0, None);
    Ok((dag, adapter, store, meta))
}

fn valid_class() -> SftClass {
    SftClass {
        asset_id: "edenite-game:iron-sword".into(),
        collection_id: "edenite-game".into(),
        class_id: "iron-sword".into(),
        name: "Épée de fer".into(),
        uri: Some("https://example/sword.png".into()),
        attributes: Some("{\"atk\":5}".into()),
        decimals: 0,
        max_supply: Some("500".into()),
        demurrage_bps_per_day: None,
        creator: "creator_pk".into(),
        mint_authority: "minter_pk".into(),
    }
}

async fn persist_class(
    adapter: &Arc<dyn NetDagAdapter>,
    meta: &WireMeta,
    wallet: &Wallet,
    parent: String,
    nonce: u64,
    class: SftClass,
) -> anyhow::Result<PutResult> {
    let wb = forge_signed_wire_block_for_test(
        vec![parent],
        meta,
        wallet,
        nonce,
        Some(PayloadEnvelope::Plain(PlainPayload::SftClassCreate(class))),
    );
    Ok(adapter.persist_block(&wb).await?)
}

/// S2 — une classe valide est enregistrée ; les classes malformées sont rejetées
/// avec la BONNE raison ; un doublon d'`asset_id` est rejeté sans overwrite.
#[tokio::test]
async fn sft_class_create_validation_and_uniqueness() -> anyhow::Result<()> {
    let (_dag, adapter, store, meta) = setup("s2").await?;
    let wallet = Wallet::from_seed(&[71u8; 32], None).unwrap();
    let genesis = Block::genesis(compute_block_id).id;

    // 1) Classe valide → Inserted + enregistrée.
    let r = persist_class(&adapter, &meta, &wallet, genesis.clone(), 1, valid_class()).await?;
    println!("valid class persist → {r:?}");
    assert!(matches!(r, PutResult::Inserted), "valid class must be inserted, got {r:?}");
    let stored = store.get_sft_class("edenite-game:iron-sword")?.expect("class registered");
    assert_eq!(stored.name, "Épée de fer");
    assert_eq!(stored.max_supply.as_deref(), Some("500"));

    // 2) asset_id incohérent (≠ "collection:class") → rejet spécifique.
    let mut bad_id = valid_class();
    bad_id.class_id = "wood-shield".into(); // asset_id reste "...:iron-sword" → mismatch
    let r = persist_class(&adapter, &meta, &wallet, genesis.clone(), 2, bad_id).await?;
    println!("mismatched asset_id → {r:?}");
    match r {
        PutResult::Rejected(reason) => assert!(
            reason.contains("asset_id must equal"),
            "must reject for asset_id mismatch, got: {reason}"
        ),
        other => panic!("mismatched asset_id must be Rejected, got {other:?}"),
    }

    // 3) decimals > 18 → rejet.
    let mut bad_dec = valid_class();
    bad_dec.asset_id = "edenite-game:gold".into();
    bad_dec.class_id = "gold".into();
    bad_dec.decimals = 19;
    let r = persist_class(&adapter, &meta, &wallet, genesis.clone(), 3, bad_dec).await?;
    println!("decimals=19 → {r:?}");
    assert!(
        matches!(r, PutResult::Rejected(ref s) if s.contains("decimals")),
        "decimals>18 must be Rejected, got {r:?}"
    );

    // 4) collection_id avec caractère interdit (':') → rejet de format.
    let mut bad_seg = valid_class();
    bad_seg.collection_id = "bad:col".into();
    bad_seg.asset_id = "bad:col:iron-sword".into();
    let r = persist_class(&adapter, &meta, &wallet, genesis.clone(), 4, bad_seg).await?;
    println!("bad collection segment → {r:?}");
    assert!(
        matches!(r, PutResult::Rejected(ref s) if s.contains("[a-z0-9-]")),
        "invalid segment must be Rejected, got {r:?}"
    );

    // 4b) max_supply non strictement positif → rejet (parité avec les tokens).
    let mut bad_cap = valid_class();
    bad_cap.asset_id = "edenite-game:potion".into();
    bad_cap.class_id = "potion".into();
    bad_cap.max_supply = Some("0".into());
    let r = persist_class(&adapter, &meta, &wallet, genesis.clone(), 6, bad_cap).await?;
    println!("max_supply=0 → {r:?}");
    assert!(
        matches!(r, PutResult::Rejected(ref s) if s.contains("max_supply must be a positive")),
        "max_supply<=0 must be Rejected, got {r:?}"
    );

    // 4c) demurrage_bps_per_day > 10000 → rejet.
    let mut bad_dem = valid_class();
    bad_dem.asset_id = "edenite-game:elixir".into();
    bad_dem.class_id = "elixir".into();
    bad_dem.demurrage_bps_per_day = Some(10_001);
    let r = persist_class(&adapter, &meta, &wallet, genesis.clone(), 7, bad_dem).await?;
    println!("demurrage=10001 → {r:?}");
    assert!(
        matches!(r, PutResult::Rejected(ref s) if s.contains("demurrage_bps_per_day must be <= 10000")),
        "demurrage>10000 must be Rejected, got {r:?}"
    );

    // 5) Doublon d'asset_id (re-create de iron-sword) → rejet, record d'origine intact.
    let mut dup = valid_class();
    dup.name = "Tentative d'écrasement".into();
    let r = persist_class(&adapter, &meta, &wallet, genesis.clone(), 5, dup).await?;
    println!("duplicate class → {r:?}");
    assert!(
        matches!(r, PutResult::Rejected(ref s) if s.contains("already exists")),
        "duplicate asset_id must be Rejected, got {r:?}"
    );
    let still = store.get_sft_class("edenite-game:iron-sword")?.unwrap();
    assert_eq!(still.name, "Épée de fer", "le doublon ne doit PAS écraser la classe d'origine");

    println!("\n   S2 PASSED: validation (asset_id/decimals/segment) + unicité OK, record intact.");
    Ok(())
}

/// `to_token_metadata` reporte le demurrage de la classe → la résolution de taux
/// (`resolve_asset_metadata`, partagée avec la validation de mint déjà testée)
/// le voit. C'est ce qui fait qu'une classe SFT décote ses UTXO par le MÊME
/// mécanisme que les tokens (protocole 2.5). La décote elle-même est prouvée par
/// `tests/demurrage_validation.rs`.
#[tokio::test]
async fn sft_to_token_metadata_carries_demurrage() {
    let mut c = valid_class();
    c.demurrage_bps_per_day = Some(250); // 2.5%/jour
    let tm = c.to_token_metadata();
    println!("SftClass.demurrage=Some(250) → TokenMetadata.demurrage={:?}", tm.demurrage_bps_per_day);
    assert_eq!(tm.demurrage_bps_per_day, Some(250), "le demurrage de la classe doit passer dans la vue TokenMetadata");
    // sanity : asset_id + cap aussi reportés (même vue utilisée par la validation de mint).
    assert_eq!(tm.asset_id, c.asset_id);
    assert_eq!(tm.max_supply, c.max_supply);
    // sans demurrage → None (pas de décote).
    let mut c0 = valid_class();
    c0.demurrage_bps_per_day = None;
    assert_eq!(c0.to_token_metadata().demurrage_bps_per_day, None);
}
