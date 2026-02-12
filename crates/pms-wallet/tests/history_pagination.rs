// crates/pms-wallet/tests/rocks_history_pagination.rs

use anyhow::Result;

use tokio::time::{Duration, sleep};

use pms_storage::{DagStorage, models::StoredBlock};
use pms_testkit::test_rocks_store;
use pms_types_block::Block;
use pms_types_payload::{EncryptedPayload, PayloadEnvelope, PlainPayload};
use pms_types_transaction::TxOutput;
use pms_utils::compute_block_id;
use pms_wallet::Wallet;
use pms_wire::WireMeta;

fn ns() -> String {
    format!("it:hist:{}", rand::random::<u32>())
}

/// Construit un StoredBlock complet (avec meta réseau)
fn mk_sb(
    parents: Vec<String>,
    payload: &PayloadEnvelope,
    nonce: u64,
    meta: &WireMeta,
) -> StoredBlock {
    let payload_json = serde_json::to_string(payload).ok();
    let id = compute_block_id(
        &parents,
        &payload_json
            .as_ref()
            .and_then(|s| serde_json::from_str(s).ok()),
        nonce,
    );
    StoredBlock {
        id,
        parents,
        payload_json,
        nonce,

        // Champs obligatoires ajoutés
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    }
}

#[tokio::test]
async fn history_pagination_by_time_and_id_rocks() -> Result<()> {
    // ----- Arrange (RocksDB éphémère) -----
    let tr = test_rocks_store("history-pagi").await?;
    let store = tr.store.clone();

    let settings = pms_config::load_config()?;
    let meta = WireMeta::from(&settings);

    // Genesis (idempotent)
    let g = Block::genesis(pms_utils::compute_block_id);
    let gsb = StoredBlock {
        id: g.id.clone(),
        parents: vec![],
        payload_json: serde_json::to_string(&g.payload).ok(),
        nonce: g.nonce,

        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(), // genesis non signé
        signature_hex: String::new(),
        metadata: None,
    };
    let _ = store.append_block_atomic(&gsb).await?;

    // Wallet cible
    let w = Wallet::generate();
    let my_addr = w.get_address(&settings.address.hrp);
    let my_xpk = w.x25519_pub_hex.clone();
    let my_sk = w.x25519_sk_hex().expect("wallet has mnemonic");

    // 5 mints (R1..R5) pour moi + 5 bruits (pour autre adresse), avec petits sleeps
    let mut my_ids = Vec::new();
    for i in 1..=5u64 {
        // mint ciblé
        let plain = PlainPayload::Mint {
            outputs: vec![TxOutput {
                address: my_addr.clone(),
                amount: format!("{}", 100 + i),
                asset_id: None,
            }],
        };
        let enc = EncryptedPayload::encrypt_for_plain(&plain, &[my_xpk.clone()])
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        let sb = mk_sb(
            vec![g.id.clone()],
            &PayloadEnvelope::Encrypted(enc),
            i,
            &meta,
        );
        let _ = store.append_block_atomic(&sb).await?;
        my_ids.push(sb.id.clone());

        // bruit: mint pour autre adresse (mais *aussi* encrypté pour moi)
        let other = PlainPayload::Mint {
            outputs: vec![TxOutput {
                address: "8e1_other_addr_____".into(),
                amount: "7".into(),
                asset_id: None,
            }],
        };
        let enc_o = EncryptedPayload::encrypt_for_plain(&other, &[my_xpk.clone()])
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        let sb_o = mk_sb(
            vec![g.id.clone()],
            &PayloadEnvelope::Encrypted(enc_o),
            i + 10000,
            &meta,
        );
        let _ = store.append_block_atomic(&sb_o).await?;

        sleep(Duration::from_millis(5)).await; // timestamps strictement ordonnés
    }

    // ----- Act & Assert -----
    // Page 1 (limit=2)
    let page1 =
        pms_wallet::history::history_page_for_address(&*store, &my_sk, &my_addr, None, 2).await?;
    assert_eq!(page1.len(), 2, "page1 doit contenir 2 éléments");
    assert!(
        page1[0].ts_ms >= page1[1].ts_ms,
        "ordre décroissant attendu"
    );
    for e in &page1 {
        assert!(
            matches!(e.plain, PlainPayload::Mint { .. }),
            "filtre par adresse"
        );
    }

    // Cursor pour la page 2
    let after = Some((
        page1.last().unwrap().ts_ms,
        page1.last().unwrap().id.clone(),
    ));
    let page2 =
        pms_wallet::history::history_page_for_address(&*store, &my_sk, &my_addr, after, 2).await?;
    assert_eq!(page2.len(), 2, "page2 doit contenir 2 éléments");
    assert!(page2[0].ts_ms >= page2[1].ts_ms);

    // Cursor pour la page 3 (reste 1 élément)
    let after2 = Some((
        page2.last().unwrap().ts_ms,
        page2.last().unwrap().id.clone(),
    ));
    let page3 =
        pms_wallet::history::history_page_for_address(&*store, &my_sk, &my_addr, after2, 2).await?;
    assert_eq!(page3.len(), 1, "page3 doit contenir le dernier élément");

    // Total récupéré = 5 mints pour moi
    let total = page1.len() + page2.len() + page3.len();
    assert_eq!(total, 5, "on doit paginer au total mes 5 mints");

    Ok(())
}
