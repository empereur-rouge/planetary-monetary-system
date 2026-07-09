//! Transfert wallet-à-wallet **distant** (sans DB locale) : flux non-custodial
//! `/v1/tx/prepare` → signature locale (pms-wallet) → `/wallet/tx/send`.
//!
//! Réplique exactement le chemin prouvé par `wallet_send_tx_e2e.rs` : le serveur
//! sélectionne les UTXOs et renvoie une tx non signée + `tx_hash` ; on signe
//! `tx_hash` avec la clé de l'émetteur ; le coordinateur vérifie les unlocks,
//! chiffre les outputs pour [sender, dest], et emballe la tx dans un bloc qu'IL
//! signe. La clé privée de l'émetteur ne quitte pas ce process.
//!
//! Auth : en-tête `X-API-Key` (clé SDK) sur prepare et send.
//!
//! Usage :
//!   PMS_API_KEY=pk_... cargo run -p tools-cli --example remote_tx -- \
//!     <base_url> <from_privkey_hex> <from_addr> <to_addr> <to_xpk_hex> <amount>

use pms_types::{Transaction, Unlock};
use pms_wallet::{SignerBackend, Wallet};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 7 {
        eprintln!(
            "usage: remote_tx <base_url> <from_privkey_hex> <from_addr> <to_addr> <to_xpk_hex> <amount>"
        );
        std::process::exit(2);
    }
    let (base, priv_hex, from_addr, to_addr, to_xpk, amount) =
        (&a[1], &a[2], &a[3], &a[4], &a[5], &a[6]);

    let w = Wallet::from_hex(priv_hex).map_err(|e| anyhow::anyhow!("clé invalide: {e}"))?;
    let api_key = std::env::var("PMS_API_KEY").unwrap_or_default();
    let http = reqwest::Client::builder().build()?;

    // 1) /v1/tx/prepare — le serveur sélectionne les UTXOs et calcule les frais.
    println!("→ POST {base}/v1/tx/prepare  (from={from_addr} to={to_addr} amount={amount})");
    let prep_resp = http
        .post(format!("{base}/v1/tx/prepare"))
        .header("X-API-Key", &api_key)
        .json(&serde_json::json!({
            "from": from_addr, "to": to_addr, "amount": amount, "asset_id": serde_json::Value::Null
        }))
        .send()
        .await?;
    let prep_status = prep_resp.status();
    let prep: serde_json::Value = prep_resp.json().await?;
    if !prep_status.is_success() {
        anyhow::bail!("prepare a échoué ({prep_status}): {prep}");
    }

    // 2) Signature locale du tx_hash renvoyé par le serveur (tx non modifiée).
    let mut tx: Transaction = serde_json::from_value(prep["unsigned_tx"].clone())
        .map_err(|e| anyhow::anyhow!("unsigned_tx illisible: {e}"))?;
    let tx_hash = prep["tx_hash"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("tx_hash absent"))?
        .to_string();
    println!(
        "← prepare ok: {} input(s), {} output(s), fee={}",
        tx.inputs.len(),
        tx.outputs.len(),
        prep["fee"].as_str().unwrap_or("?")
    );
    let sig_b64 = w
        .sign(&tx_hash)
        .map_err(|e| anyhow::anyhow!("échec signature: {e:?}"))?;
    tx.unlocks = tx
        .inputs
        .iter()
        .map(|_| Unlock::new(w.public_key_hex.clone(), sig_b64.clone()))
        .collect();

    // 3) /wallet/tx/send — le coordinateur emballe la tx dans un bloc qu'il signe.
    let recipients_xpk = vec![w.x25519_pub_hex.clone(), to_xpk.clone()];
    let send_resp = http
        .post(format!("{base}/wallet/tx/send"))
        .header("X-API-Key", &api_key)
        .json(&serde_json::json!({ "tx": tx, "recipients_xpk": recipients_xpk }))
        .send()
        .await?;
    let status = send_resp.status();
    let body: serde_json::Value = send_resp.json().await.unwrap_or(serde_json::Value::Null);
    println!("← send [{status}]: {body}");
    if !status.is_success() {
        anyhow::bail!("le coordinateur a refusé la tx ({status})");
    }
    let block_id = body
        .get("id")
        .or_else(|| body.get("block_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    println!("✅ transfert accepté, bloc coordinateur id={block_id}");
    Ok(())
}
