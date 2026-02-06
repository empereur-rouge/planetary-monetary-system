//! Tests pour pms-wire
//!
//! Couverture:
//! - WireBlock sérialisation/désérialisation
//! - WireBlock.canonical_bytes() déterminisme
//! - Conversion WireBlock -> Block

use pms_types_block::BlockMetadata;
use pms_wire::{WireBlock, WireMeta};

// ============================================================================
// Tests WireBlock serialization
// ============================================================================

#[test]
fn test_wireblock_serialize_deserialize() {
    let wb = WireBlock {
        id: "abc123".to_string(),
        parents: vec!["parent1".to_string(), "parent2".to_string()],
        payload_json: Some(r#"{"type":"test"}"#.to_string()),
        nonce: 42,
        network_id: "pms-dev".to_string(),
        protocol_version: 1,
        signer_pk_hex: "03abcd".to_string(),
        signature_hex: "sig123".to_string(),
        metadata: None,
    };

    let json = serde_json::to_string(&wb).unwrap();
    let parsed: WireBlock = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.id, wb.id);
    assert_eq!(parsed.parents, wb.parents);
    assert_eq!(parsed.payload_json, wb.payload_json);
    assert_eq!(parsed.nonce, wb.nonce);
    assert_eq!(parsed.network_id, wb.network_id);
    assert_eq!(parsed.protocol_version, wb.protocol_version);
    assert_eq!(parsed.signer_pk_hex, wb.signer_pk_hex);
    assert_eq!(parsed.signature_hex, wb.signature_hex);
}

#[test]
fn test_wireblock_with_metadata() {
    let meta = BlockMetadata {
        description: Some("test-block".to_string()),
        tags: vec!["tag1".to_string()],
        extra: None,
        signer_x25519_hex: None,
    };

    let wb = WireBlock {
        id: "xyz789".to_string(),
        parents: vec![],
        payload_json: None,
        nonce: 0,
        network_id: "testnet".to_string(),
        protocol_version: 2,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: Some(meta),
    };

    let json = serde_json::to_string(&wb).unwrap();
    let parsed: WireBlock = serde_json::from_str(&json).unwrap();

    assert!(parsed.metadata.is_some());
    let m = parsed.metadata.unwrap();
    assert_eq!(m.description, Some("test-block".to_string()));
    assert_eq!(m.tags, vec!["tag1".to_string()]);
}

#[test]
fn test_wireblock_metadata_skipped_if_none() {
    let wb = WireBlock {
        id: "id1".to_string(),
        parents: vec![],
        payload_json: None,
        nonce: 0,
        network_id: "dev".to_string(),
        protocol_version: 1,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    };

    let json = serde_json::to_string(&wb).unwrap();
    // metadata should not appear in JSON when None
    assert!(!json.contains("metadata"));
}

// ============================================================================
// Tests canonical_bytes
// ============================================================================

#[test]
fn test_canonical_bytes_deterministic() {
    let wb = WireBlock {
        id: "test123".to_string(),
        parents: vec!["p1".to_string(), "p2".to_string()],
        payload_json: Some(r#"{"data":"test"}"#.to_string()),
        nonce: 999,
        network_id: "pms-main".to_string(),
        protocol_version: 1,
        signer_pk_hex: "pubkey".to_string(),
        signature_hex: "sig".to_string(),
        metadata: None,
    };

    let bytes1 = wb.canonical_bytes();
    let bytes2 = wb.canonical_bytes();

    assert_eq!(bytes1, bytes2, "canonical_bytes doit etre deterministe");
}

#[test]
fn test_canonical_bytes_excludes_signature() {
    let wb1 = WireBlock {
        id: "same".to_string(),
        parents: vec![],
        payload_json: None,
        nonce: 1,
        network_id: "dev".to_string(),
        protocol_version: 1,
        signer_pk_hex: String::new(),
        signature_hex: "signature1".to_string(),
        metadata: None,
    };

    let wb2 = WireBlock {
        id: "same".to_string(),
        parents: vec![],
        payload_json: None,
        nonce: 1,
        network_id: "dev".to_string(),
        protocol_version: 1,
        signer_pk_hex: String::new(),
        signature_hex: "different_signature".to_string(),
        metadata: None,
    };

    // canonical_bytes should be same because signature is excluded
    assert_eq!(
        wb1.canonical_bytes(),
        wb2.canonical_bytes(),
        "Signature ne doit pas affecter canonical_bytes"
    );
}

#[test]
fn test_canonical_bytes_includes_nonce() {
    let wb1 = WireBlock {
        id: "id".to_string(),
        parents: vec![],
        payload_json: None,
        nonce: 1,
        network_id: "dev".to_string(),
        protocol_version: 1,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    };

    let wb2 = WireBlock {
        id: "id".to_string(),
        parents: vec![],
        payload_json: None,
        nonce: 2, // Different nonce
        network_id: "dev".to_string(),
        protocol_version: 1,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    };

    assert_ne!(
        wb1.canonical_bytes(),
        wb2.canonical_bytes(),
        "Nonce different doit changer canonical_bytes"
    );
}

// ============================================================================
// Tests WireBlock -> Block conversion
// ============================================================================

#[test]
fn test_wireblock_to_block_basic() {
    use pms_types_block::Block;

    let wb = WireBlock {
        id: "block_id".to_string(),
        parents: vec!["parent".to_string()],
        payload_json: None,
        nonce: 123,
        network_id: "pms-dev".to_string(),
        protocol_version: 1,
        signer_pk_hex: "pubkey123".to_string(),
        signature_hex: "sig456".to_string(),
        metadata: None,
    };

    let block: Block = wb.into();

    assert_eq!(block.id, "block_id");
    assert_eq!(block.parents, vec!["parent".to_string()]);
    assert_eq!(block.nonce, 123);
    assert_eq!(block.signer_pk, Some("pubkey123".to_string()));
    assert_eq!(block.signature, Some("sig456".to_string()));
    assert!(block.payload.is_none());
}

#[test]
fn test_wireblock_to_block_empty_signer_becomes_none() {
    use pms_types_block::Block;

    let wb = WireBlock {
        id: "id".to_string(),
        parents: vec![],
        payload_json: None,
        nonce: 0,
        network_id: "dev".to_string(),
        protocol_version: 1,
        signer_pk_hex: String::new(), // Empty
        signature_hex: String::new(), // Empty
        metadata: None,
    };

    let block: Block = wb.into();

    // Empty strings become None
    assert!(block.signer_pk.is_none());
    assert!(block.signature.is_none());
}

// ============================================================================
// Tests WireMeta
// ============================================================================

#[test]
fn test_wiremeta_clone() {
    let meta = WireMeta {
        network_id: "pms-testnet".to_string(),
        protocol_version: 42,
    };

    let cloned = meta.clone();

    assert_eq!(cloned.network_id, meta.network_id);
    assert_eq!(cloned.protocol_version, meta.protocol_version);
}

// ============================================================================
// Tests WireBlock equality
// ============================================================================

#[test]
fn test_wireblock_equality() {
    let wb1 = WireBlock {
        id: "same".to_string(),
        parents: vec!["p".to_string()],
        payload_json: Some("data".to_string()),
        nonce: 10,
        network_id: "net".to_string(),
        protocol_version: 1,
        signer_pk_hex: "pk".to_string(),
        signature_hex: "sig".to_string(),
        metadata: None,
    };

    let wb2 = wb1.clone();

    assert_eq!(wb1, wb2);
}

#[test]
fn test_wireblock_inequality() {
    let wb1 = WireBlock {
        id: "id1".to_string(),
        parents: vec![],
        payload_json: None,
        nonce: 1,
        network_id: "dev".to_string(),
        protocol_version: 1,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    };

    let wb2 = WireBlock {
        id: "id2".to_string(), // Different ID
        parents: vec![],
        payload_json: None,
        nonce: 1,
        network_id: "dev".to_string(),
        protocol_version: 1,
        signer_pk_hex: String::new(),
        signature_hex: String::new(),
        metadata: None,
    };

    assert_ne!(wb1, wb2);
}
