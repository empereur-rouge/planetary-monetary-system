// tests/handlers_unit.rs
use std::sync::Arc;
use axum::extract::{State, Query};
use serde::Deserialize;
use pms_config::{Network, NetworkMode, ServerConfig};
use pms_storage::{RedisStore, StoredBlock};
use pms_wire::{WireBlock};
use pms_network::messages::NetMsg::Block;
use pms_server::api::AppState;
use pms_server::api_fn::history::PageQ;
use pms_server::api_fn::stream_blocks::stream_blocks;
use pms_types::{OutputId, TxInput, TxOutput, Unlock};
use pms_types_payload::{EncryptedPayload, PayloadEnvelope, PlainPayload};
// === Types des handlers ===
// Mettez les bons paths vers vos handlers et AppState

async fn mk_store() -> Arc<RedisStore> {
    let ns = format!("unittests:{}", rand::random::<u64>());
    Arc::new(RedisStore::new("redis://127.0.0.1/", 1000, &ns).await.unwrap())
}

fn mk_sb(id: String, parents: Vec<String>, env: &PayloadEnvelope, nonce: u64) -> StoredBlock {
    StoredBlock {
        id,
        parents,
        payload_json: Some(serde_json::to_string(env).unwrap()),
        nonce,
    }
}

#[tokio::test]
async fn unit_get_encrypted_history_only_encrypted() {
    // Arrange
    let store = mk_store().await;

    // genesis
    let g = Block::genesis(compute_block_id);
    let gsb = StoredBlock {
        id: g.id.clone(),
        parents: vec![],
        payload_json: serde_json::to_string(&g.payload).ok(),
        nonce: g.nonce,
    };
    store.append_block_atomic(&gsb).await.unwrap();

    // wallet destinataire
    let w = pms_wallet::Wallet::generate();
    let my_addr = w.get_address();
    let my_xpk  = w.x25519_pub_hex.clone();
    let my_sk   = w.x25519_sk_hex().unwrap();

    // 1) Encrypted Reward (pour moi)
    let plain_reward = PlainPayload::Reward {
        outputs: vec![TxOutput { address: my_addr.clone(), amount: "42".into() }],
    };
    let enc_reward = EncryptedPayload::encrypt_for_plain(&plain_reward, &[my_xpk.clone()]).unwrap();
    let wb_r = WireBlock {
        id: String::new(),
        parents: vec![g.id.clone()],
        payload_json: Some(serde_json::to_string(&PayloadEnvelope::Encrypted(enc_reward.clone())).unwrap()),
        nonce: 1,
    };
    let id_r = compute_block_id(
        &wb_r.parents,
        &wb_r.payload_json.as_ref().and_then(|s| serde_json::from_str(s).ok()),
        wb_r.nonce,
    );
    let sb_r = mk_sb(id_r.clone(), vec![g.id.clone()], &PayloadEnvelope::Encrypted(enc_reward), 1);
    store.append_block_atomic(&sb_r).await.unwrap();

    // 2) Encrypted Tx (chiffré pour moi mais output ≠ moi)
    let other_addr = "8e1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
    let tx = PlainPayload::TxUtxo(Transaction {
        inputs: vec![TxInput { out: OutputId { txid: "prev".into(), index: 0 } }],
        outputs: vec![TxOutput { address: other_addr.into(), amount: "13".into() }],
        fee: "0".into(),
        unlocks: vec![Unlock { pubkey_hex: "00".into(), signature_b64: "AA==".into() }],
    });
    let enc_tx = EncryptedPayload::encrypt_for_plain(&tx, &[my_xpk]).unwrap();
    let wb_t = WireBlock {
        id: String::new(),
        parents: vec![g.id.clone()],
        payload_json: Some(serde_json::to_string(&PayloadEnvelope::Encrypted(enc_tx.clone())).unwrap()),
        nonce: 2,
    };
    let id_t = compute_block_id(
        &wb_t.parents,
        &wb_t.payload_json.as_ref().and_then(|s| serde_json::from_str(s).ok()),
        wb_t.nonce,
    );
    let sb_t = mk_sb(id_t.clone(), vec![g.id.clone()], &PayloadEnvelope::Encrypted(enc_tx), 2);
    store.append_block_atomic(&sb_t).await.unwrap();

    // 3) Un bloc non-encrypté (doit être filtré)
    let sb_clear = StoredBlock {
        id: "clear-id".into(),
        parents: vec![g.id.clone()],
        payload_json: Some(serde_json::to_string(&PayloadEnvelope::Genesis).unwrap()),
        nonce: 3,
    };
    store.append_block_atomic(&sb_clear).await.unwrap();

    // AppState & Query
    let app = AppState { srv: Arc::new(()), _cfg: Arc::new(ServerConfig {
        bind_addr: "".to_string(),
        api_addr: "".to_string(),
        tls: None,
        network: Network { mode: NetworkMode::Dev },
    }), _ready: Arc::new(Default::default()), stats: Arc::new(Stats {}), store: store.clone() };
    let q = PageQ { after_ts: None, after_id: None, limit: Some(50) };

    // Act: appel direct du handler
    let resp = get_encrypted_history(State(app), Query(q)).await.expect("handler ok");

    // Assert
    let PageResp { items, .. } = resp.0;
    // uniquement des Encrypted
    assert!(items.iter().all(|wb|
        wb.payload_json.as_ref()
            .and_then(|s| serde_json::from_str::<PayloadEnvelope>(s).ok())
            .map(|env| matches!(env, PayloadEnvelope::Encrypted(_)))
            .unwrap_or(false)
    ));
    // le bloc clair ne figure pas
    assert!(!items.iter().any(|w| w.id == "clear-id"));
    // notre Reward est dedans
    assert!(items.iter().any(|w| w.id == id_r));

    // Bonus: on re-déchiffre pour vérifier
    let reward = items.iter().find(|w| w.id == id_r).unwrap();
    let env: PayloadEnvelope = serde_json::from_str(reward.payload_json.as_ref().unwrap()).unwrap();
    if let PayloadEnvelope::Encrypted(enc) = env {
        let plain = enc.decrypt_as_payload(&my_sk).unwrap();
        match plain {
            PlainPayload::Reward { outputs } => {
                assert_eq!(outputs[0].address, my_addr);
                assert_eq!(outputs[0].amount, "42");
            }
            _ => panic!("expected Reward"),
        }
    } else {
        panic!("expected Encrypted");
    }
}

#[tokio::test]
async fn unit_stream_blocks_returns_recent_ndjson_or_array() {
    // Arrange
    let store = mk_store().await;

    let g = Block::genesis(compute_block_id);
    let gsb = StoredBlock {
        id: g.id.clone(),
        parents: vec![],
        payload_json: serde_json::to_string(&g.payload).ok(),
        nonce: g.nonce,
    };
    store.append_block_atomic(&gsb).await.unwrap();

    let mk = |nonce: u64| {
        let env = serde_json::json!({"dummy":"ok","n":nonce});
        let wb = WireBlock {
            id: String::new(),
            parents: vec![g.id.clone()],
            payload_json: Some(env.to_string()),
            nonce,
        };
        let id = compute_block_id(
            &wb.parents,
            &wb.payload_json.as_ref().and_then(|s| serde_json::from_str(s).ok()),
            wb.nonce,
        );
        let sb = StoredBlock { id: id.clone(), parents: vec![g.id.clone()], payload_json: wb.payload_json.clone(), nonce };
        (id, sb)
    };
    let (id1, sb1) = mk(1);
    let (id2, sb2) = mk(2);
    store.append_block_atomic(&sb1).await.unwrap();
    store.append_block_atomic(&sb2).await.unwrap();

    let app = AppState { store: store.clone() };
    let q = PageQ { after_ts: None, after_id: None, limit: Some(2) };

    // Act
    let body = stream_blocks(State(app), Query(q)).await.expect("handler ok");

    // Assert: supporte NDJSON ou JSON array selon votre impl
    let s = body;
    if s.trim_start().starts_with('[') {
        // JSON array
        let v: Vec<WireBlock> = serde_json::from_str(&s).unwrap();
        let ids: Vec<_> = v.iter().map(|w| w.id.as_str()).collect();
        assert!(ids.contains(&id1.as_str()));
        assert!(ids.contains(&id2.as_str()));
    } else {
        // NDJSON
        assert!(s.contains(&id1));
        assert!(s.contains(&id2));
        assert!(s.lines().count() >= 2);
    }
}