use anyhow::Result;
use pms_storage::{RedisStore};
use pms_storage::utxo::UtxoApply;

async fn flushdb(url: &str) -> Result<()> {
    let client = redis::Client::open(url)?;
    let mut con = client.get_multiplexed_async_connection().await?;
    let _: () = redis::cmd("FLUSHDB").query_async(&mut con).await?;
    Ok(())
}
fn redis_url() -> String { std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1/".into()) }

#[tokio::test]
async fn apply_tx_atomic_ok_then_conflict() -> Result<()> {
    let url = redis_url();
    flushdb(&url).await?;
    let store = RedisStore::new(&url, 64, "it:utxo").await?;
    store.ensure_schema().await?;

    // Seed: coinbase UTXO existant
    {
        // Simule un coinbase: on insère une entrée UTXO "coinbase1:0"
        let mut con = store.con.clone();
        let _: () = redis::AsyncCommands::hset(
            &mut con,
            store.k_utxo(),
            "coinbase1:0",
            r#"{"addr":"A","amt":"1.0"}"#,
        ).await?;
    }

    // 1) t1 consomme coinbase1:0 -> OK
    let t1 = UtxoApply {
        txid: "t1".into(),
        inputs: vec![("coinbase1".into(), 0)],
        outputs: vec![("A".into(), "1.0".into())],
    };
    let ok1 = store.utxo_apply_tx_atomic(&t1).await?;
    assert!(ok1, "t1 doit passer");

    // 2) t2 re-consomme coinbase1:0 -> conflit
    let t2 = UtxoApply {
        txid: "t2".into(),
        inputs: vec![("coinbase1".into(), 0)],
        outputs: vec![("B".into(), "1.0".into())],
    };
    let ok2 = store.utxo_apply_tx_atomic(&t2).await?;
    assert!(!ok2, "t2 doit être rejetée (double spend)");

    // 3) idempotence: rejouer t1 -> false
    let again = store.utxo_apply_tx_atomic(&t1).await?;
    assert!(!again, "rejeu t1 doit être ignoré");

    Ok(())
}
