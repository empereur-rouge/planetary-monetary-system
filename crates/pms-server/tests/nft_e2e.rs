//! Tests E2E pour les NFT.
//!
//! Ce test vérifie le flux complet :
//! 1. Soumettre un bloc avec PlainPayload::Nft(Mint)
//! 2. Vérifier que le NFT est persisté via NftStorage
//! 3. (Optionnel) Query via API REST

use std::sync::Arc;

use anyhow::Result;
use pms_config::load_config;
use pms_core::{ConcurrentDag, CoreAdapter};
use pms_interface::NetDagAdapter;
use pms_storage::rocks_store::store::RocksStore;
use pms_storage::{NftStorage, PutResult};
use pms_testkit::forge_signed_wire_block_for_test;
use pms_types::Block;
use pms_types_nft::{NftAction, NftMetadata};
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use pms_utils::compute_block_id;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireMeta;

type DagRef = Arc<ConcurrentDag>;

/// Test: Mint NFT et vérifier ownership
#[tokio::test]
async fn nft_mint_and_verify_ownership() -> Result<()> {
    // 1) Setup: Store + DAG + Adapter
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("rocks-nft-e2e");

    let store = Arc::new(
        RocksStore::new(
            db_path.to_string_lossy().as_ref(),
            256,
            "pms:nft-test",
            None,
        )
        .await?,
    );

    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    let genesis = Block::genesis(compute_block_id);
    let dag: DagRef = Arc::new(ConcurrentDag::new_with_genesis(genesis));

    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone());

    // 2) Wallet de test (sera le creator du NFT)
    let wallet = Wallet::from_seed(&[42u8; 32], None).expect("wallet");
    let creator_pk = wallet.encoded_public_key();

    // 3) Créer un NftAction::Mint
    let token_id = "nft-e2e-001".to_string();
    let mint_action = NftAction::Mint {
        token_id: token_id.clone(),
        creator: creator_pk.clone(),
        metadata: NftMetadata {
            name: Some("Test NFT".into()),
            description: Some("E2E test token".into()),
            uri: None,
            nft_type: Some("test".into()),
            extra: None,
        },
    };

    // 4) Payload NFT
    let payload = Some(PayloadEnvelope::Plain(PlainPayload::Nft(mint_action)));

    // 5) Forger le bloc avec le payload NFT
    let block = dag.forge_block(payload.clone(), 0, compute_block_id)?;

    // 6) Signer et créer le WireBlock
    let wb = forge_signed_wire_block_for_test(
        block.parents.clone(),
        &meta,
        &wallet,
        block.nonce,
        payload,
    );

    // 7) Persister via l'adapter (valide + applique NFT)
    let res = adapter.persist_block(&wb).await?;

    assert!(
        matches!(res, PutResult::Inserted | PutResult::AlreadyExists),
        "NFT mint devrait être accepté, obtenu: {:?}",
        res
    );

    // 8) Vérifier l'ownership dans le store
    let owner = store.get_owner(&token_id)?;
    assert!(owner.is_some(), "Le NFT devrait exister après mint");
    assert_eq!(
        owner.unwrap(),
        creator_pk,
        "Le owner devrait être le creator"
    );

    println!("✅ NFT Mint E2E test passed: token_id={}", token_id);

    Ok(())
}

/// Test: Transfer NFT et vérifier nouveau owner
#[tokio::test]
async fn nft_transfer_changes_ownership() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("rocks-nft-transfer");

    let store = Arc::new(
        RocksStore::new(
            db_path.to_string_lossy().as_ref(),
            256,
            "pms:nft-transfer",
            None,
        )
        .await?,
    );

    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    let genesis = Block::genesis(compute_block_id);
    let dag: DagRef = Arc::new(ConcurrentDag::new_with_genesis(genesis));

    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone());

    // 2 wallets: owner initial et nouveau owner
    let wallet_a = Wallet::from_seed(&[43u8; 32], None).map_err(|e| anyhow::anyhow!(e))?;
    let wallet_b = Wallet::from_seed(&[44u8; 32], None).map_err(|e| anyhow::anyhow!(e))?;

    let owner_a_pk = wallet_a.encoded_public_key();
    let owner_b_pk = wallet_b.encoded_public_key();

    let token_id = "nft-transfer-001".to_string();

    // ---------- Phase 1: Mint par wallet_a ----------

    let mint_action = NftAction::Mint {
        token_id: token_id.clone(),
        creator: owner_a_pk.clone(),
        metadata: NftMetadata::default(),
    };

    let payload_mint = Some(PayloadEnvelope::Plain(PlainPayload::Nft(mint_action)));
    let block_mint = dag.forge_block(payload_mint.clone(), 0, compute_block_id)?;

    let wb_mint = forge_signed_wire_block_for_test(
        block_mint.parents.clone(),
        &meta,
        &wallet_a,
        block_mint.nonce,
        payload_mint,
    );

    let res_mint = adapter.persist_block(&wb_mint).await?;
    assert!(matches!(
        res_mint,
        PutResult::Inserted | PutResult::AlreadyExists
    ));

    // Vérifier que wallet_a est owner
    assert_eq!(store.get_owner(&token_id)?, Some(owner_a_pk.clone()));

    // ---------- Phase 2: Transfer vers wallet_b ----------

    let transfer_action = NftAction::Transfer {
        token_id: token_id.clone(),
        from: owner_a_pk.clone(),
        to: owner_b_pk.clone(),
        new_owner_x25519_pubkey: None,
        encrypted_metadata: None,
    };

    let payload_transfer = Some(PayloadEnvelope::Plain(PlainPayload::Nft(transfer_action)));
    let block_transfer = dag.forge_block(payload_transfer.clone(), 0, compute_block_id)?;

    // Le transfer doit être signé par le owner actuel (wallet_a)
    let wb_transfer = forge_signed_wire_block_for_test(
        block_transfer.parents.clone(),
        &meta,
        &wallet_a, // Signé par le owner actuel
        block_transfer.nonce,
        payload_transfer,
    );

    let res_transfer = adapter.persist_block(&wb_transfer).await?;
    assert!(matches!(
        res_transfer,
        PutResult::Inserted | PutResult::AlreadyExists
    ));

    // Vérifier que wallet_b est maintenant owner
    let new_owner = store.get_owner(&token_id)?;
    assert_eq!(new_owner, Some(owner_b_pk.clone()));

    println!(
        "✅ NFT Transfer E2E test passed: {} -> {}",
        owner_a_pk[..8].to_string(),
        owner_b_pk[..8].to_string()
    );

    Ok(())
}

/// Test: Burn NFT supprime l'ownership
#[tokio::test]
async fn nft_burn_removes_token() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("rocks-nft-burn");

    let store = Arc::new(
        RocksStore::new(
            db_path.to_string_lossy().as_ref(),
            256,
            "pms:nft-burn",
            None,
        )
        .await?,
    );

    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    let genesis = Block::genesis(compute_block_id);
    let dag: DagRef = Arc::new(ConcurrentDag::new_with_genesis(genesis));
    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone());

    let wallet = Wallet::from_seed(&[45u8; 32], None).map_err(|e| anyhow::anyhow!(e))?;
    let owner_pk = wallet.encoded_public_key();
    let token_id = "nft-burn-001".to_string();

    // Phase 1: Mint
    let mint = NftAction::Mint {
        token_id: token_id.clone(),
        creator: owner_pk.clone(),
        metadata: NftMetadata::default(),
    };
    let payload_mint = Some(PayloadEnvelope::Plain(PlainPayload::Nft(mint)));
    let block_mint = dag.forge_block(payload_mint.clone(), 0, compute_block_id)?;
    let wb_mint = forge_signed_wire_block_for_test(
        block_mint.parents.clone(),
        &meta,
        &wallet,
        block_mint.nonce,
        payload_mint,
    );
    adapter.persist_block(&wb_mint).await?;

    // Vérifier existence
    assert!(store.get_owner(&token_id)?.is_some());

    // Phase 2: Burn
    let burn = NftAction::Burn {
        token_id: token_id.clone(),
        burner: owner_pk.clone(),
    };
    let payload_burn = Some(PayloadEnvelope::Plain(PlainPayload::Nft(burn)));
    let block_burn = dag.forge_block(payload_burn.clone(), 0, compute_block_id)?;
    let wb_burn = forge_signed_wire_block_for_test(
        block_burn.parents.clone(),
        &meta,
        &wallet,
        block_burn.nonce,
        payload_burn,
    );
    adapter.persist_block(&wb_burn).await?;

    // Vérifier non-existence
    assert!(
        store.get_owner(&token_id)?.is_none(),
        "Token devrait être supprimé après burn"
    );

    println!("✅ NFT Burn E2E test passed: token {} burned", token_id);

    Ok(())
}

/// Test: Unauthorized transfer is rejected
#[tokio::test]
async fn nft_unauthorized_transfer_rejected() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("rocks-nft-unauth");

    let store = Arc::new(
        RocksStore::new(
            db_path.to_string_lossy().as_ref(),
            256,
            "pms:nft-unauth",
            None,
        )
        .await?,
    );

    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    let genesis = Block::genesis(compute_block_id);
    let dag: DagRef = Arc::new(ConcurrentDag::new_with_genesis(genesis));
    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone());

    let wallet_owner = Wallet::from_seed(&[50u8; 32], None).map_err(|e| anyhow::anyhow!(e))?;
    let wallet_attacker = Wallet::from_seed(&[51u8; 32], None).map_err(|e| anyhow::anyhow!(e))?;

    let owner_pk = wallet_owner.encoded_public_key();
    let attacker_pk = wallet_attacker.encoded_public_key();
    let token_id = "nft-unauth-001".to_string();

    // Phase 1: Mint par owner
    let mint = NftAction::Mint {
        token_id: token_id.clone(),
        creator: owner_pk.clone(),
        metadata: NftMetadata::default(),
    };
    let payload_mint = Some(PayloadEnvelope::Plain(PlainPayload::Nft(mint)));
    let block_mint = dag.forge_block(payload_mint.clone(), 0, compute_block_id)?;
    let wb_mint = forge_signed_wire_block_for_test(
        block_mint.parents.clone(),
        &meta,
        &wallet_owner,
        block_mint.nonce,
        payload_mint,
    );
    adapter.persist_block(&wb_mint).await?;

    // Phase 2: Attaquant tente de transférer (signé par attaquant, pas owner)
    let transfer = NftAction::Transfer {
        token_id: token_id.clone(),
        from: owner_pk.clone(), // Prétend être le owner
        to: attacker_pk.clone(),
        new_owner_x25519_pubkey: None,
        encrypted_metadata: None,
    };
    let payload_transfer = Some(PayloadEnvelope::Plain(PlainPayload::Nft(transfer)));
    let block_transfer = dag.forge_block(payload_transfer.clone(), 0, compute_block_id)?;
    let wb_transfer = forge_signed_wire_block_for_test(
        block_transfer.parents.clone(),
        &meta,
        &wallet_attacker, // Signé par l'attaquant, pas le owner!
        block_transfer.nonce,
        payload_transfer,
    );

    let res = adapter.persist_block(&wb_transfer).await?;

    // Doit être rejeté
    assert!(
        matches!(res, PutResult::Rejected(_)),
        "Transfer non autorisé devrait être rejeté, obtenu: {:?}",
        res
    );

    // Owner n'a pas changé
    assert_eq!(store.get_owner(&token_id)?, Some(owner_pk));

    println!("✅ NFT Unauthorized transfer rejected as expected");

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests pour get_by_owner (indexation par propriétaire)
// ═══════════════════════════════════════════════════════════════════════════

/// Test: get_by_owner retourne les NFTs après mint
#[tokio::test]
async fn nft_get_by_owner_after_mint() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("rocks-nft-byowner");

    let store = Arc::new(
        RocksStore::new(
            db_path.to_string_lossy().as_ref(),
            256,
            "pms:nft-byowner",
            None,
        )
        .await?,
    );

    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    let genesis = Block::genesis(compute_block_id);
    let dag: DagRef = Arc::new(ConcurrentDag::new_with_genesis(genesis));
    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone());

    let wallet = Wallet::from_seed(&[60u8; 32], None).map_err(|e| anyhow::anyhow!(e))?;
    let owner_pk = wallet.encoded_public_key();

    // Vérifier que la liste est vide au départ
    let initial = store.get_by_owner(&owner_pk)?;
    assert!(initial.is_empty(), "Liste devrait être vide initialement");

    // Mint 2 NFTs pour le même owner
    for i in 1..=2 {
        let token_id = format!("nft-byowner-{:03}", i);
        let mint = NftAction::Mint {
            token_id: token_id.clone(),
            creator: owner_pk.clone(),
            metadata: NftMetadata::default(),
        };
        let payload = Some(PayloadEnvelope::Plain(PlainPayload::Nft(mint)));
        let block = dag.forge_block(payload.clone(), 0, compute_block_id)?;
        let wb = forge_signed_wire_block_for_test(
            block.parents.clone(),
            &meta,
            &wallet,
            block.nonce,
            payload,
        );
        adapter.persist_block(&wb).await?;
    }

    // Vérifier que get_by_owner retourne les 2 NFTs
    let nfts = store.get_by_owner(&owner_pk)?;
    assert_eq!(nfts.len(), 2, "Owner devrait avoir 2 NFTs");
    assert!(nfts.contains(&"nft-byowner-001".to_string()));
    assert!(nfts.contains(&"nft-byowner-002".to_string()));

    println!("✅ NFT get_by_owner after mint: {} NFTs found", nfts.len());

    Ok(())
}

/// Test: get_by_owner est mis à jour après transfer
#[tokio::test]
async fn nft_get_by_owner_updates_on_transfer() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("rocks-nft-transfer-list");

    let store = Arc::new(
        RocksStore::new(
            db_path.to_string_lossy().as_ref(),
            256,
            "pms:nft-transfer-list",
            None,
        )
        .await?,
    );

    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    let genesis = Block::genesis(compute_block_id);
    let dag: DagRef = Arc::new(ConcurrentDag::new_with_genesis(genesis));
    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone());

    let wallet_a = Wallet::from_seed(&[61u8; 32], None).map_err(|e| anyhow::anyhow!(e))?;
    let wallet_b = Wallet::from_seed(&[62u8; 32], None).map_err(|e| anyhow::anyhow!(e))?;
    let owner_a_pk = wallet_a.encoded_public_key();
    let owner_b_pk = wallet_b.encoded_public_key();

    let token_id = "nft-transfer-list-001".to_string();

    // Phase 1: Mint pour wallet_a
    let mint = NftAction::Mint {
        token_id: token_id.clone(),
        creator: owner_a_pk.clone(),
        metadata: NftMetadata::default(),
    };
    let payload = Some(PayloadEnvelope::Plain(PlainPayload::Nft(mint)));
    let block = dag.forge_block(payload.clone(), 0, compute_block_id)?;
    let wb = forge_signed_wire_block_for_test(
        block.parents.clone(),
        &meta,
        &wallet_a,
        block.nonce,
        payload,
    );
    adapter.persist_block(&wb).await?;

    // Vérifier que A a le NFT, B n'a rien
    assert_eq!(store.get_by_owner(&owner_a_pk)?, vec![token_id.clone()]);
    assert!(store.get_by_owner(&owner_b_pk)?.is_empty());

    // Phase 2: Transfer de A vers B
    let transfer = NftAction::Transfer {
        token_id: token_id.clone(),
        from: owner_a_pk.clone(),
        to: owner_b_pk.clone(),
        new_owner_x25519_pubkey: None,
        encrypted_metadata: None,
    };
    let payload = Some(PayloadEnvelope::Plain(PlainPayload::Nft(transfer)));
    let block = dag.forge_block(payload.clone(), 0, compute_block_id)?;
    let wb = forge_signed_wire_block_for_test(
        block.parents.clone(),
        &meta,
        &wallet_a,
        block.nonce,
        payload,
    );
    adapter.persist_block(&wb).await?;

    // Vérifier que A n'a plus rien, B a le NFT
    assert!(
        store.get_by_owner(&owner_a_pk)?.is_empty(),
        "A ne devrait plus avoir de NFTs après transfer"
    );
    assert_eq!(
        store.get_by_owner(&owner_b_pk)?,
        vec![token_id.clone()],
        "B devrait avoir le NFT après transfer"
    );

    println!("✅ NFT get_by_owner updates correctly on transfer");

    Ok(())
}

/// Test: get_by_owner est vidé après burn
#[tokio::test]
async fn nft_get_by_owner_clears_on_burn() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("rocks-nft-burn-list");

    let store = Arc::new(
        RocksStore::new(
            db_path.to_string_lossy().as_ref(),
            256,
            "pms:nft-burn-list",
            None,
        )
        .await?,
    );

    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    let genesis = Block::genesis(compute_block_id);
    let dag: DagRef = Arc::new(ConcurrentDag::new_with_genesis(genesis));
    let adapter: Arc<dyn NetDagAdapter> = CoreAdapter::new(dag.clone(), store.clone());

    let wallet = Wallet::from_seed(&[63u8; 32], None).map_err(|e| anyhow::anyhow!(e))?;
    let owner_pk = wallet.encoded_public_key();

    let token_id = "nft-burn-list-001".to_string();

    // Phase 1: Mint
    let mint = NftAction::Mint {
        token_id: token_id.clone(),
        creator: owner_pk.clone(),
        metadata: NftMetadata::default(),
    };
    let payload = Some(PayloadEnvelope::Plain(PlainPayload::Nft(mint)));
    let block = dag.forge_block(payload.clone(), 0, compute_block_id)?;
    let wb = forge_signed_wire_block_for_test(
        block.parents.clone(),
        &meta,
        &wallet,
        block.nonce,
        payload,
    );
    adapter.persist_block(&wb).await?;

    // Vérifier présence
    assert_eq!(store.get_by_owner(&owner_pk)?, vec![token_id.clone()]);

    // Phase 2: Burn
    let burn = NftAction::Burn {
        token_id: token_id.clone(),
        burner: owner_pk.clone(),
    };
    let payload = Some(PayloadEnvelope::Plain(PlainPayload::Nft(burn)));
    let block = dag.forge_block(payload.clone(), 0, compute_block_id)?;
    let wb = forge_signed_wire_block_for_test(
        block.parents.clone(),
        &meta,
        &wallet,
        block.nonce,
        payload,
    );
    adapter.persist_block(&wb).await?;

    // Vérifier que la liste est vide
    assert!(
        store.get_by_owner(&owner_pk)?.is_empty(),
        "Liste devrait être vide après burn"
    );

    println!("✅ NFT get_by_owner clears correctly on burn");

    Ok(())
}
