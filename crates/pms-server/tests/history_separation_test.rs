//! Tests des handlers d'historique RÉELS (`get_encrypted_history` /
//! `get_plain_history` dans `api_fn/history.rs`), pas une ré-implémentation.
//!
//! Couvre :
//! 1. **Séparation** chiffré ↔ plain (un bloc d'un type n'apparaît pas dans l'autre flux).
//! 2. **Pagination** par curseur (`limit` + `next_after_id`/`next_after_ts`) à travers
//!    le vrai handler — ce que l'ancien `history_core.rs` (supprimé v0.9.3) prétendait
//!    tester via un `FakeStore` + une copie locale `get_encrypted_history_core`.

use axum::extract::{Query, State};
use pms_server::api::AppState;
use pms_server::api_fn::history::{PageQ, get_encrypted_history, get_plain_history};
use pms_storage::StoredBlock;
use pms_storage::rocks_store::store::RocksStore;
use pms_types::TxOutput;
use pms_types_payload::{AAD, EncryptedPayload, PayloadEnvelope, PlainPayload};
use pms_wire::WireMeta;
use std::sync::Arc;

/// `AppState` complet + store + meta, via le helper testkit partagé
/// `make_test_state` (évite de ré-écrire le literal AppState à 30 champs — il
/// vit désormais en un seul endroit dans `pms-testkit`).
async fn build_test_state() -> anyhow::Result<(AppState, Arc<RocksStore>, WireMeta)> {
    pms_testkit::make_test_state().await
}

/// Insère un bloc Mint (plain) avec un id donné, parenté au genesis logique.
async fn insert_plain_mint(
    store: &Arc<RocksStore>,
    meta: &WireMeta,
    id: &str,
    nonce: u64,
    addr: &str,
) -> anyhow::Result<()> {
    let payload = PlainPayload::Mint {
        outputs: vec![TxOutput::new(addr, "100", None)],
    };
    let sb = StoredBlock {
        id: id.into(),
        parents: vec!["genesis".into()],
        payload_json: Some(serde_json::to_string(&PayloadEnvelope::Plain(payload)).unwrap()),
        nonce,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    };
    store.append_block_atomic(&sb).await?;
    Ok(())
}

#[tokio::test]
async fn history_separation_test() -> anyhow::Result<()> {
    let (state, store, meta) = build_test_state().await?;

    // A) Bloc chiffré
    let enc_payload = EncryptedPayload {
        scheme: "x25519+aes256gcm".into(),
        key_version: 1,
        aad: AAD {
            len_hint: 0,
            binding: None,
        },
        commitment: "c".into(),
        ciphertext_b64: "AA==".into(),
        recipients: vec![],
        nonce_b64: "AA==".into(),
    };
    let sb_enc = StoredBlock {
        id: "ENC-1".into(),
        parents: vec!["genesis".into()],
        payload_json: Some(serde_json::to_string(&PayloadEnvelope::Encrypted(enc_payload)).unwrap()),
        nonce: 1,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    };
    store.append_block_atomic(&sb_enc).await?;

    // B) Bloc plain (Mint)
    insert_plain_mint(&store, &meta, "PLAIN-1", 2, "addr1").await?;

    // Flux chiffré : contient ENC-1, pas PLAIN-1.
    let resp_enc = get_encrypted_history(
        State(state.clone()),
        Query(PageQ {
            after_ts: None,
            after_id: None,
            limit: Some(10),
        }),
    )
    .await
    .unwrap();
    let items_enc = resp_enc.0.items;
    println!("encrypted history: {} items", items_enc.len());
    assert!(items_enc.iter().any(|b| b.id == "ENC-1"));
    assert!(!items_enc.iter().any(|b| b.id == "PLAIN-1"));

    // Flux plain : contient PLAIN-1, pas ENC-1.
    let resp_plain = get_plain_history(
        State(state),
        Query(PageQ {
            after_ts: None,
            after_id: None,
            limit: Some(10),
        }),
    )
    .await
    .unwrap();
    let items_plain = resp_plain.0.items;
    println!("plain history: {} items", items_plain.len());
    assert!(items_plain.iter().any(|b| b.id == "PLAIN-1"));
    assert!(!items_plain.iter().any(|b| b.id == "ENC-1"));

    Ok(())
}

/// Pagination RÉELLE à travers `get_plain_history` : 5 blocs, pages de 2, on suit
/// le curseur `next_after_ts`/`next_after_id` jusqu'à épuisement et on vérifie que
/// l'union des pages = les 5 ids, sans doublon, avec curseur final nul.
#[tokio::test]
async fn plain_history_paginates_via_cursor() -> anyhow::Result<()> {
    let (state, store, meta) = build_test_state().await?;

    // 5 blocs Mint distincts. Petite pause pour des timestamps croissants stables.
    for i in 0..5 {
        insert_plain_mint(&store, &meta, &format!("P{i}"), i as u64 + 1, &format!("addr{i}")).await?;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let mut seen: Vec<String> = Vec::new();
    let mut after_ts: Option<i64> = None;
    let mut after_id: Option<String> = None;
    let mut pages = 0;

    loop {
        pages += 1;
        assert!(pages <= 10, "pagination did not terminate (cursor loop?)");
        let resp = get_plain_history(
            State(state.clone()),
            Query(PageQ {
                after_ts,
                after_id: after_id.clone(),
                limit: Some(2),
            }),
        )
        .await
        .unwrap();
        let page = resp.0;
        let ids: Vec<String> = page.items.iter().map(|b| b.id.clone()).collect();
        println!(
            "page {pages}: ids={ids:?} next_after_id={:?}",
            page.next_after_id
        );
        assert!(page.items.len() <= 2, "page must respect limit=2");
        for id in &ids {
            seen.push(id.clone());
        }
        match page.next_after_id {
            Some(_) => {
                after_ts = page.next_after_ts;
                after_id = page.next_after_id;
            }
            None => break, // dernière page
        }
    }

    println!("paginated {pages} pages, seen={seen:?}");
    // Aucun id (le genesis persisté par make_test_state inclus) ne doit
    // apparaître sur deux pages.
    let mut unique: Vec<String> = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(seen.len(), unique.len(), "no id should appear on two pages");

    // Les 5 blocs P0..P4 doivent TOUS être paginés exactement une fois (le store
    // contient aussi le bloc genesis, hors de notre jeu — on filtre nos ids).
    let p_seen: std::collections::HashSet<String> =
        seen.into_iter().filter(|id| id.starts_with('P')).collect();
    let expected: std::collections::HashSet<String> =
        (0..5).map(|i| format!("P{i}")).collect();
    assert_eq!(p_seen, expected, "every inserted P-block must be paged exactly once");
    assert!(pages >= 3, "5+ items at 2/page require at least 3 pages, got {pages}");
    Ok(())
}
