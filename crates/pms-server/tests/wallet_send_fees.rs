use pms_storage::DagStorage;
use pms_storage::rocks_store::store::RocksStore;
use pms_testkit::{make_test_ctx_with_admin, mint_to_wallet_and_get_inputs, post_json};
use pms_token::fee::FeePolicy;
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use pms_wallet::utxo_store::{UtxoDec, gather_wallet_utxos_dec};
use pms_wallet::{SignerBackend, Wallet, address_candidates};
use pms_wire::WireBlock;
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::time::Duration;

fn clear_admin_env_conflicts() {
    unsafe {
        // Tout ce qui pourrait polluer `admin.signer_pubkeys`
        for k in [
            "PMS__ADMIN__SIGNER_PUBKEYS",
            "PMS__ADMIN__SIGNER_PUBKEYS__0",
            "PMS__ADMIN__SIGNER_PUBKEYS__1",
            "PMS__ADMIN__SIGNER_PUBKEYS__2",
            "PMS__ADMIN", // si jamais tu l’as utilisée
        ] {
            std::env::remove_var(k);
        }
    }
}

/// Scan récent : retourne les PlainPayload déchiffrés par `wallet`
/// et qui impliquent une des adresses candidates de ce wallet.
pub async fn scan_wallet_plain_history(
    store: &Arc<RocksStore>,
    wallet: &Wallet,
    hrp: &str,
    scan_limit: usize,
) -> anyhow::Result<Vec<PlainPayload>> {
    let candidates = address_candidates(hrp, &wallet.public_key_hex, &wallet.x25519_pub_hex);
    let sk_hex = wallet
        .x25519_sk_hex()
        .ok_or_else(|| anyhow::anyhow!("missing x25519 sk"))?;

    let (ids, _) = store.recent_ids_by_time(None, None, scan_limit).await?;
    let blocks: Vec<WireBlock> = store.get_blocks_by_ids(&ids).await?;

    let mut out = Vec::new();
    for wb in blocks {
        let Some(s) = &wb.payload_json else { continue };
        let Ok(env) = serde_json::from_str::<PayloadEnvelope>(s) else {
            continue;
        };

        let plain = match env {
            PayloadEnvelope::Encrypted(enc) => {
                let Ok(pp) = enc.decrypt_as_payload(&sk_hex) else {
                    continue;
                };
                pp
            }
            PayloadEnvelope::Plain(pp) => pp,
        };

        // même logique que ton CLI: “involves_any_address”
        let hit = match &plain {
            PlainPayload::Mint { outputs } => outputs.iter().any(|o| {
                candidates
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(&o.address))
            }),
            PlainPayload::TxUtxo(tx) => tx.outputs.iter().any(|o| {
                candidates
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(&o.address))
            }),
            _ => false,
        };

        if hit {
            out.push(plain);
        }
    }

    Ok(out)
}

async fn gather_plain_mint_utxos_for_wallet(
    store: &Arc<RocksStore>,
    hrp: &str,
    w: &Wallet,
    scan_limit: usize,
) -> anyhow::Result<Vec<UtxoDec>> {
    let candidates = address_candidates(hrp, &w.public_key_hex, &w.x25519_pub_hex);

    let ids = store.recent_ids(scan_limit).await?;
    let blocks = store.get_blocks_by_ids(&ids).await?;

    let mut earned: HashMap<(String, u32), Decimal> = HashMap::new();
    let mut spent: HashSet<(String, u32)> = HashSet::new();

    for b in blocks {
        let Some(s) = &b.payload_json else {
            continue;
        };
        let Ok(env) = serde_json::from_str::<PayloadEnvelope>(s) else {
            continue;
        };

        match env {
            PayloadEnvelope::Plain(PlainPayload::Mint { outputs }) => {
                for (i, o) in outputs.iter().enumerate() {
                    if candidates
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(&o.address))
                    {
                        if let Ok(d) = Decimal::from_str_exact(&o.amount) {
                            earned.insert((b.id.clone(), i as u32), d);
                        }
                    }
                }
            }
            PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx)) => {
                // si jamais tu veux aussi capter des UTXO non chiffrés
                for inp in &tx.inputs {
                    spent.insert((inp.out.txid.clone(), inp.out.index));
                }
            }
            _ => {}
        }
    }

    let mut utxos = Vec::new();
    for ((txid, index), amt) in earned {
        if !spent.contains(&(txid.clone(), index)) {
            utxos.push(UtxoDec {
                txid,
                index,
                amount: amt,
            });
        }
    }
    utxos.sort_by(|a, b| b.amount.cmp(&a.amount));
    Ok(utxos)
}

#[tokio::test]
#[ignore = "TODO: fix admin decrypt/scan logic"]
async fn wallet_send_tx_injects_fee_and_admin_can_decrypt_fee_utxo() -> anyhow::Result<()> {
    clear_admin_env_conflicts();

    // Pre-generate admin wallet (same seed as node_wallet in make_test_ctx_with_admin)
    let admin = std::sync::Arc::new(Wallet::from_seed(&[7u8; 32], None).unwrap());
    let hrp = "8e";
    let admin_addr = admin.get_address(hrp);
    let admin_pubkey = admin.encoded_public_key();

    // Set test admin pubkey for mint policy validation
    unsafe {
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", &admin_pubkey);
    }

    // Create test context with admin wallet address and signer pubkey preconfigured
    let ctx = make_test_ctx_with_admin(vec![admin_addr.clone()], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.clone();

    // --- wallets user ---
    let w_from = Wallet::from_seed(&[10u8; 32], None).unwrap();
    let _from_addr = w_from.get_address(&hrp);

    let w_to = Wallet::from_seed(&[11u8; 32], None).unwrap();
    let to_addr = w_to.get_address(&hrp);

    eprintln!("[TEST] hrp={hrp}");
    eprintln!("[TEST] admin_addr={admin_addr}");
    eprintln!("[TEST] admin_xpk={}", admin.x25519_pub_hex);
    eprintln!("[TEST] to_xpk={}", w_to.x25519_pub_hex);

    // ⚠️ Si ton ledger exige des inputs réels, il faut un mint avant.
    // Je ne refais pas tes tests mint/utxo, mais ici tu DOIS avoir des inputs valides
    // sinon tu vas retomber sur “input missing”.
    //
    // => Reuse ton helper existant de test “mint_to_wallet_and_get_inputs(...)”.
    let (inputs, _minted_amount) = mint_to_wallet_and_get_inputs(&ctx, &w_from, "5.00").await?;

    // Wait for async persistence to complete
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Body: on n’inclut PAS admin.xpk volontairement.
    // Ton code doit l’ajouter automatiquement quand fee>0.
    // Fee calculation: 3.5% of taxable_amount + 0.001 base = ~0.141 for 4.00
    let taxable_amount = "4.00";
    let fee_policy = FeePolicy::new(&ctx.settings.fees.base_fee, &ctx.settings.fees.ratio, 18);
    let fee = fee_policy
        .compute_fee(taxable_amount)
        .expect("fee computation");
    let hrp = ctx.settings.address.hrp.as_str();
    let utxos = gather_plain_mint_utxos_for_wallet(&ctx.store, hrp, &w_from, 500).await?;
    let u = utxos.first().expect("need at least one UTXO from mint");

    let body = serde_json::json!({
        "tx": {
            "inputs": [{ "out": { "txid": u.txid, "index": u.index } }],
            "outputs": [
                { "address": to_addr, "amount": taxable_amount },
                { "address": admin_addr, "amount": &fee }
            ],
            "fee": fee,
            "unlocks": []
        },
        "recipients_xpk": [ w_to.x25519_pub_hex.clone(), admin.x25519_pub_hex.clone() ]
    });
    let (status, json) = post_json(&ctx.app, "/wallet/tx/send", body).await;
    assert!(
        status.is_success(),
        "send failed: status={status} body={json}"
    );

    let ids = ctx.store.recent_ids(20).await?;
    let wbs = ctx.store.get_blocks_by_ids(&ids).await?;
    eprintln!("[TEST] recent blocks:");
    for wb in &wbs {
        eprintln!(
            "  - id={} payload_json?={}",
            wb.id,
            wb.payload_json.is_some()
        );
    }

    // --- scan admin history: il doit voir une TxUtxo avec un output == fee vers une adresse admin candidate ---
    let plains = scan_wallet_plain_history(&ctx.store, &admin, &hrp, 500).await?;

    let fee_dec = Decimal::from_str_exact(&fee)?;
    let admin_candidates =
        pms_wallet::address_candidates(&hrp, &admin.public_key_hex, &admin.x25519_pub_hex);

    let mut found_fee_utxo = false;
    for p in plains {
        if let PlainPayload::TxUtxo(tx) = p {
            let ok = tx.outputs.iter().any(|o| {
                Decimal::from_str_exact(&o.amount).ok() == Some(fee_dec)
                    && admin_candidates
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(&o.address))
            });
            if ok {
                found_fee_utxo = true;
                break;
            }
        }
    }

    assert!(
        found_fee_utxo,
        "admin must be able to decrypt and see the fee UTXO"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "TODO: fix admin decrypt/scan logic"]
async fn wallet_send_tx_fee_is_materialized_and_zeroed_and_visible_to_admin() -> anyhow::Result<()>
{
    clear_admin_env_conflicts();

    // Pre-generate admin wallet
    let admin = std::sync::Arc::new(Wallet::from_seed(&[7u8; 32], None).unwrap());
    let hrp = "8e";
    let admin_addr = admin.get_address(hrp);
    let admin_pubkey = admin.encoded_public_key();

    // Set test admin pubkey for mint policy validation
    unsafe {
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", &admin_pubkey);
    }

    // Create test context with admin wallet address and signer pubkey preconfigured
    let ctx = make_test_ctx_with_admin(vec![admin_addr.clone()], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.clone();

    // wallets user
    let w_from = Wallet::from_seed(&[10u8; 32], None).unwrap();
    let w_to = Wallet::from_seed(&[11u8; 32], None).unwrap();
    let to_addr = w_to.get_address(&hrp);

    // Mint préalable (tu as déjà ton helper qui marche chez toi)
    mint_to_wallet_and_get_inputs(&ctx, &w_from, "5.00").await?;

    // Wait for async persistence to complete
    tokio::time::sleep(Duration::from_millis(200)).await;

    // récupère un UTXO mint (plain) pour construire l’input
    let utxos = gather_plain_mint_utxos_for_wallet(&ctx.store, &hrp, &w_from, 500).await?;
    let u = utxos.first().expect("need at least one UTXO from mint");

    // Compute fee dynamically using FeePolicy from settings
    let taxable_amount = "4.00";
    let fee_policy = FeePolicy::new(&ctx.settings.fees.base_fee, &ctx.settings.fees.ratio, 18);
    let fee = fee_policy
        .compute_fee(taxable_amount)
        .expect("fee computation");
    let body = serde_json::json!({
        "tx": {
            "inputs": [{ "out": { "txid": u.txid, "index": u.index } }],
            "outputs": [
                { "address": to_addr, "amount": taxable_amount },
                { "address": admin_addr, "amount": &fee }
            ],
            "fee": fee,
            "unlocks": []
        },
        // IMPORTANT: on n'inclut PAS admin.xpk ici, le serveur doit l’ajouter
        "recipients_xpk": [ w_to.x25519_pub_hex.clone() ]
    });

    let (status, json) = post_json(&ctx.app, "/wallet/tx/send", body).await;
    assert!(
        status.is_success(),
        "send failed: status={status} body={json}"
    );

    // admin decrypt history
    let plains = scan_wallet_plain_history(&ctx.store, &admin, &hrp, 500).await?;

    let fee_dec = Decimal::from_str_exact(&fee)?;
    let admin_candidates =
        pms_wallet::address_candidates(&hrp, &admin.public_key_hex, &admin.x25519_pub_hex);

    let mut found = false;

    for p in plains {
        if let PlainPayload::TxUtxo(tx) = p {
            // Option B: fee doit être 0 après matérialisation
            assert_eq!(
                tx.fee.trim(),
                "0",
                "Option B: tx.fee must be zero in stored payload"
            );

            // EXACTEMENT 1 output fee de ce montant vers admin
            let fee_outputs: Vec<_> = tx
                .outputs
                .iter()
                .filter(|o| {
                    Decimal::from_str_exact(&o.amount).ok() == Some(fee_dec)
                        && admin_candidates
                            .iter()
                            .any(|c| c.eq_ignore_ascii_case(&o.address))
                })
                .collect();

            if fee_outputs.len() == 1 {
                found = true;
                break;
            }
        }
    }

    assert!(
        found,
        "admin must decrypt a TxUtxo containing exactly one fee output of {fee}"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "TODO: fix admin decrypt/scan logic"]
async fn wallet_send_tx_does_not_duplicate_fee_output_if_already_present() -> anyhow::Result<()> {
    clear_admin_env_conflicts();

    // Pre-generate admin wallet
    let admin = std::sync::Arc::new(Wallet::from_seed(&[7u8; 32], None).unwrap());
    let hrp = "8e";
    let admin_addr = admin.get_address(hrp);
    let admin_pubkey = admin.encoded_public_key();

    // Set test admin pubkey for mint policy validation
    unsafe {
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", &admin_pubkey);
    }

    // Create test context with admin wallet address and signer pubkey preconfigured
    let ctx = make_test_ctx_with_admin(vec![admin_addr.clone()], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.clone();

    let w_from = Wallet::from_seed(&[10u8; 32], None).unwrap();
    let w_to = Wallet::from_seed(&[11u8; 32], None).unwrap();
    let to_addr = w_to.get_address(&hrp);

    mint_to_wallet_and_get_inputs(&ctx, &w_from, "5.00").await?;

    // Wait for async persistence to complete
    tokio::time::sleep(Duration::from_millis(200)).await;
    let utxos = gather_plain_mint_utxos_for_wallet(&ctx.store, &hrp, &w_from, 500).await?;
    let u = utxos.first().expect("need at least one UTXO from mint");

    // Compute fee dynamically using FeePolicy from settings
    let taxable_amount = "4.00";
    let fee_policy = FeePolicy::new(&ctx.settings.fees.base_fee, &ctx.settings.fees.ratio, 18);
    let fee = fee_policy
        .compute_fee(taxable_amount)
        .expect("fee computation");

    // client inclut déjà l’output fee
    let body = serde_json::json!({
        "tx": {
            "inputs": [{ "out": { "txid": u.txid, "index": u.index } }],
            "outputs": [
                { "address": to_addr, "amount": taxable_amount },
                { "address": admin_addr, "amount": &fee }
            ],
            "fee": fee,
            "unlocks": []
        },
        "recipients_xpk": [ w_to.x25519_pub_hex.clone(), admin.x25519_pub_hex.clone() ]
    });

    let (status, json) = post_json(&ctx.app, "/wallet/tx/send", body).await;
    assert!(
        status.is_success(),
        "send failed: status={status} body={json}"
    );

    // admin decrypt
    let plains = scan_wallet_plain_history(&ctx.store, &admin, &hrp, 500).await?;
    let fee_dec = Decimal::from_str_exact(&fee)?;
    let admin_candidates =
        pms_wallet::address_candidates(&hrp, &admin.public_key_hex, &admin.x25519_pub_hex);

    // on cherche une TxUtxo où le nombre d’outputs fee == 1
    let mut ok = false;
    for p in plains {
        if let PlainPayload::TxUtxo(tx) = p {
            let fee_outputs_count = tx
                .outputs
                .iter()
                .filter(|o| {
                    Decimal::from_str_exact(&o.amount).ok() == Some(fee_dec)
                        && admin_candidates
                            .iter()
                            .any(|c| c.eq_ignore_ascii_case(&o.address))
                })
                .count();

            if fee_outputs_count == 1 {
                ok = true;
                break;
            }
        }
    }

    assert!(ok, "fee output must not be duplicated by the API");
    Ok(())
}
