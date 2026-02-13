// ═══════════════════════════════════════════════════════════════════════════════
// compliance_test.rs — Integration tests for Compliance features
// ═══════════════════════════════════════════════════════════════════════════════
//
// Tests the compliance storage layer, freeze enforcement in prepare_tx,
// and read-only compliance endpoints (frozen list, audit log, shadow balance).
//
// Note: Full e2e tests of admin_freeze/seize/reverse endpoints require a
// running coordinator node (persist_block loads config from file with the
// coordinator public key). See docker_e2e tests for full coverage.
//
// Run:
//   cargo test --package pms-server --test compliance_test -- --nocapture
//
// ═══════════════════════════════════════════════════════════════════════════════

use axum::body::Body;
use axum::extract::connect_info::ConnectInfo;
use http::Request;
use pms_storage::ComplianceStorage;
use pms_testkit::make_test_ctx;
use serde_json::{json, Value};
use std::net::SocketAddr;
use tower::ServiceExt;

// ── HTTP helpers ──────────────────────────────────────────────────────────

fn local_addr() -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 12345)))
}

async fn post_json(app: &axum::Router, uri: &str, body: &Value) -> (u16, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(body).unwrap()))
        .unwrap();
    req.extensions_mut().insert(local_addr());

    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(json!({}));
    (status, json)
}

async fn get_json(app: &axum::Router, uri: &str) -> (u16, Value) {
    let mut req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    req.extensions_mut().insert(local_addr());

    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1_000_000)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(json!({}));
    (status, json)
}

// ═══════════════════════════════════════════════════════════════════════════════
// 1. Compliance Storage Layer Tests
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_freeze_and_unfreeze_storage() {
    let ctx = make_test_ctx().await.unwrap();
    let addr = "8e1test_freeze_addr_123";

    // Initially not frozen
    assert!(!ctx.store.is_frozen(addr).unwrap());
    assert!(ctx.store.get_freeze_entry(addr).unwrap().is_none());

    // Freeze
    ctx.store
        .freeze_address(addr, "block_001", "suspicious activity")
        .unwrap();

    // Now frozen
    assert!(ctx.store.is_frozen(addr).unwrap());

    let entry = ctx.store.get_freeze_entry(addr).unwrap().unwrap();
    assert_eq!(entry.address, addr);
    assert_eq!(entry.block_id, "block_001");
    assert_eq!(entry.reason, "suspicious activity");

    // Unfreeze
    ctx.store.unfreeze_address(addr).unwrap();
    assert!(!ctx.store.is_frozen(addr).unwrap());
    assert!(ctx.store.get_freeze_entry(addr).unwrap().is_none());
}

#[tokio::test]
async fn test_list_frozen_storage() {
    let ctx = make_test_ctx().await.unwrap();

    // Freeze 3 addresses
    ctx.store
        .freeze_address("addr_a", "block_a", "reason_a")
        .unwrap();
    ctx.store
        .freeze_address("addr_b", "block_b", "reason_b")
        .unwrap();
    ctx.store
        .freeze_address("addr_c", "block_c", "reason_c")
        .unwrap();

    let frozen = ctx.store.list_frozen().unwrap();
    assert_eq!(frozen.len(), 3);

    // Unfreeze one
    ctx.store.unfreeze_address("addr_b").unwrap();
    let frozen = ctx.store.list_frozen().unwrap();
    assert_eq!(frozen.len(), 2);
}

#[tokio::test]
async fn test_compliance_log_storage() {
    let ctx = make_test_ctx().await.unwrap();

    ctx.store
        .log_compliance_action(
            "freeze",
            "block_001",
            Some("addr_target"),
            &json!({"reason": "test freeze"}),
        )
        .unwrap();

    ctx.store
        .log_compliance_action(
            "unfreeze",
            "block_002",
            Some("addr_target"),
            &json!({"reason": "cleared"}),
        )
        .unwrap();

    ctx.store
        .log_compliance_action(
            "seize",
            "block_003",
            Some("addr_seized"),
            &json!({"amount": "500.0", "reason": "court order"}),
        )
        .unwrap();

    let log = ctx.store.list_compliance_log().unwrap();
    assert_eq!(log.len(), 3);
    assert_eq!(log[0].action, "freeze");
    assert_eq!(log[1].action, "unfreeze");
    assert_eq!(log[2].action, "seize");
}

// ═══════════════════════════════════════════════════════════════════════════════
// 2. Freeze Enforcement in prepare_tx
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_freeze_blocks_outgoing_tx() {
    let ctx = make_test_ctx().await.unwrap();

    let sender_addr = "8e1sender_frozen_test";
    let receiver_addr = "8e1receiver_test";

    // Add UTXOs for the sender so prepare_tx would normally succeed
    let adapter = ctx.srv.adapter_arc();
    adapter
        .add_utxo(
            "mint_block_001".into(),
            0,
            sender_addr.into(),
            "100.0".into(),
            None,
        )
        .await;

    // Freeze the sender
    ctx.store
        .freeze_address(sender_addr, "freeze_block", "under investigation")
        .unwrap();

    // prepare_tx from frozen address should return 403
    let (status, body) = post_json(
        &ctx.app,
        "/v1/tx/prepare",
        &json!({
            "from": sender_addr,
            "to": receiver_addr,
            "amount": "10.0"
        }),
    )
    .await;
    assert_eq!(
        status, 403,
        "prepare_tx from frozen sender should be 403: {body}"
    );
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("frozen"),
        "error should mention frozen: {body}"
    );
}

#[tokio::test]
async fn test_freeze_blocks_incoming_tx() {
    let ctx = make_test_ctx().await.unwrap();

    let sender_addr = "8e1sender_ok_test";
    let receiver_addr = "8e1receiver_frozen_test";

    // Add UTXOs for sender
    let adapter = ctx.srv.adapter_arc();
    adapter
        .add_utxo(
            "mint_block_002".into(),
            0,
            sender_addr.into(),
            "100.0".into(),
            None,
        )
        .await;

    // Freeze the receiver
    ctx.store
        .freeze_address(receiver_addr, "freeze_block_2", "recipient under investigation")
        .unwrap();

    // prepare_tx to frozen address should return 403
    let (status, body) = post_json(
        &ctx.app,
        "/v1/tx/prepare",
        &json!({
            "from": sender_addr,
            "to": receiver_addr,
            "amount": "10.0"
        }),
    )
    .await;
    assert_eq!(
        status, 403,
        "prepare_tx to frozen receiver should be 403: {body}"
    );
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("frozen"),
        "error should mention frozen: {body}"
    );
}

#[tokio::test]
async fn test_unfreeze_restores_tx() {
    let ctx = make_test_ctx().await.unwrap();

    let sender_addr = "8e1sender_unfreeze_test";
    let receiver_addr = "8e1receiver_unfreeze_test";

    // Add UTXOs for sender
    let adapter = ctx.srv.adapter_arc();
    adapter
        .add_utxo(
            "mint_block_003".into(),
            0,
            sender_addr.into(),
            "100.0".into(),
            None,
        )
        .await;

    // Freeze sender
    ctx.store
        .freeze_address(sender_addr, "freeze_block_3", "investigation")
        .unwrap();

    // Verify frozen
    let (status, _) = post_json(
        &ctx.app,
        "/v1/tx/prepare",
        &json!({ "from": sender_addr, "to": receiver_addr, "amount": "10.0" }),
    )
    .await;
    assert_eq!(status, 403);

    // Unfreeze
    ctx.store.unfreeze_address(sender_addr).unwrap();

    // Now prepare_tx should work (200 = success)
    let (status, body) = post_json(
        &ctx.app,
        "/v1/tx/prepare",
        &json!({ "from": sender_addr, "to": receiver_addr, "amount": "10.0" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "prepare_tx should succeed after unfreeze: {body}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 3. Read-only Compliance API Endpoints
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_list_frozen_endpoint() {
    let ctx = make_test_ctx().await.unwrap();

    // Freeze some addresses directly in store
    ctx.store
        .freeze_address("8e1addr_alpha", "block_a", "reason alpha")
        .unwrap();
    ctx.store
        .freeze_address("8e1addr_beta", "block_b", "reason beta")
        .unwrap();

    let (status, body) = get_json(&ctx.app, "/admin/compliance/frozen").await;
    assert_eq!(status, 200, "list frozen: {body}");
    assert_eq!(body["frozen_accounts"], 2);
    let accounts = body["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 2);
}

#[tokio::test]
async fn test_compliance_log_endpoint() {
    let ctx = make_test_ctx().await.unwrap();

    // Add some log entries
    ctx.store
        .log_compliance_action(
            "freeze",
            "block_001",
            Some("8e1addr_x"),
            &json!({"reason": "investigation"}),
        )
        .unwrap();
    ctx.store
        .log_compliance_action(
            "seize",
            "block_002",
            Some("8e1addr_y"),
            &json!({"amount": "1000.0"}),
        )
        .unwrap();

    let (status, body) = get_json(&ctx.app, "/admin/compliance/log").await;
    assert_eq!(status, 200, "compliance log: {body}");
    assert_eq!(body["total_entries"], 2);
    let log = body["log"].as_array().unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0]["action"], "freeze");
    assert_eq!(log[1]["action"], "seize");
}

#[tokio::test]
async fn test_shadow_balance_endpoint() {
    let ctx = make_test_ctx().await.unwrap();
    let adapter = ctx.srv.adapter_arc();

    let addr1 = "8e1frozen_balance_1";
    let addr2 = "8e1frozen_balance_2";

    // Add UTXOs to the addresses
    adapter
        .add_utxo("mint_001".into(), 0, addr1.into(), "200.0".into(), None)
        .await;
    adapter
        .add_utxo("mint_002".into(), 0, addr2.into(), "300.0".into(), None)
        .await;

    // Freeze both
    ctx.store
        .freeze_address(addr1, "freeze_1", "reason 1")
        .unwrap();
    ctx.store
        .freeze_address(addr2, "freeze_2", "reason 2")
        .unwrap();

    let (status, body) = get_json(&ctx.app, "/admin/compliance/shadow_balance").await;
    assert_eq!(status, 200, "shadow balance: {body}");
    assert_eq!(body["frozen_accounts"], 2);

    let total: f64 = body["total_pms_frozen"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        (total - 500.0).abs() < 0.01,
        "total PMS frozen should be ~500, got {total}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// 4. Edge Cases
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_double_freeze_returns_error() {
    let ctx = make_test_ctx().await.unwrap();
    let addr = "8e1double_freeze_test";

    ctx.store
        .freeze_address(addr, "block_1", "first freeze")
        .unwrap();
    assert!(ctx.store.is_frozen(addr).unwrap());

    // Second freeze returns an error (already frozen)
    let result = ctx.store.freeze_address(addr, "block_2", "second freeze");
    assert!(result.is_err(), "second freeze should fail");
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("already frozen"),
        "error should mention already frozen"
    );
}

#[tokio::test]
async fn test_unfreeze_non_frozen_returns_error() {
    let ctx = make_test_ctx().await.unwrap();
    let addr = "8e1never_frozen";

    // Unfreeze a non-frozen address should return an error
    let result = ctx.store.unfreeze_address(addr);
    assert!(result.is_err(), "unfreeze non-frozen should fail");
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("not frozen"),
        "error should mention not frozen"
    );
}

#[tokio::test]
async fn test_empty_frozen_list() {
    let ctx = make_test_ctx().await.unwrap();

    let (status, body) = get_json(&ctx.app, "/admin/compliance/frozen").await;
    assert_eq!(status, 200);
    assert_eq!(body["frozen_accounts"], 0);
}

#[tokio::test]
async fn test_empty_shadow_balance() {
    let ctx = make_test_ctx().await.unwrap();

    let (status, body) = get_json(&ctx.app, "/admin/compliance/shadow_balance").await;
    assert_eq!(status, 200);
    assert_eq!(body["frozen_accounts"], 0);
    assert_eq!(body["total_pms_frozen"], "0");
}

#[tokio::test]
async fn test_empty_compliance_log() {
    let ctx = make_test_ctx().await.unwrap();

    let (status, body) = get_json(&ctx.app, "/admin/compliance/log").await;
    assert_eq!(status, 200);
    assert_eq!(body["total_entries"], 0);
}
