use anyhow::Result;
use pms_network::messages::NetMsg;
use pms_storage::DagStorage;
use pms_testkit::{
    ephemeral_addr, spawn_node_generic_rocks, test_meta_and_wallet, wait_for_listen,
};
use pms_wallet::SignerBackend;
use pms_wire::WireBlock;
use tempfile::tempdir;
use tokio::time::{Duration, sleep};
// pour get_block()

/// Envoie un bloc invalide à un nœud : il ne doit ni persister ni re-diffuser (RocksDB).
#[tokio::test]
async fn validate_before_relay_rocks() -> Result<()> {
    // DB éphémères
    let dir_a = tempdir()?;
    let dir_b = tempdir()?;
    let db_a = dir_a.path().join("rocks-vbr-a");
    let db_b = dir_b.path().join("rocks-vbr-b");
    let a_addr = ephemeral_addr();
    let b_addr = ephemeral_addr();

    // Helpers réseau + wallet
    let (meta, wallet) = test_meta_and_wallet();

    // Spawn A et B (Rocks)
    let tip_limit = 128usize;
    let (_store_a, _dag_a, adapter_a, srv_a, _ha) = spawn_node_generic_rocks(
        db_a.to_str().unwrap(),
        "it:vbr:A",
        a_addr.as_str(),
        "127.0.0.1:8201",
        tip_limit,
        None,
    )
    .await?;
    let (store_b, _dag_b, _adapter_b, srv_b, _hb) = spawn_node_generic_rocks(
        db_b.to_str().unwrap(),
        "it:vbr:B",
        b_addr.as_str(),
        "127.0.0.1:8202",
        tip_limit,
        None,
    )
    .await?;

    wait_for_listen(a_addr.as_str(), 1_000).await?;
    wait_for_listen(b_addr.as_str(), 1_000).await?;

    // Connecte B -> A
    srv_b.connect(a_addr.as_str()).await?;

    // ❌ Bloc invalide: self-parent
    let bad = WireBlock {
        id: "bad1".into(),
        parents: vec!["bad1".into()],
        payload_json: None,
        nonce: 1,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: wallet.encoded_public_key(),
        signature_hex: String::new(),
        metadata: None, // signature volontairement vide → invalide
    };

    // B tente d’injecter vers A → doit être rejeté
    srv_b
        .broadcast(&NetMsg::Block {
            id: bad.id.clone(),
            parents: bad.parents.clone(),
            payload_json: bad.payload_json.clone(),
            nonce: bad.nonce,
            network_id: bad.network_id,
            protocol_version: bad.protocol_version,
            signature_hex: bad.signature_hex,
            signer_pk_hex: bad.signer_pk_hex,
            metadata: bad.metadata.map(Box::new),
        })
        .await?;

    // On laisse le pipeline valider/rejeter
    sleep(Duration::from_millis(300)).await;

    // A NE doit PAS posséder le bloc
    assert!(
        !adapter_a.have_block("bad1").await,
        "bloc invalide ne doit pas être présent côté A"
    );

    // B non plus
    assert!(
        store_b.get_block("bad1").await?.is_none(),
        "bloc invalide ne doit pas se propager/persister côté B"
    );

    Ok(())
}
