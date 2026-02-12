//! ============================================================
//! Test de régression : Vérification que les transactions ENCRYPTED
//! mettent correctement à jour le cache UTXO.
//!
//! Ce test aurait attrapé le bug où `persist_block` ne gérait que
//! les payloads `Plain`, laissant les UTXOs des transactions chiffrées
//! dans un état incohérent (inputs non consommés, outputs non créés).
//!
//! # Scénario testé
//! 1. Mint initial vers Alice (Plain payload → crée UTXO)
//! 2. Alice envoie à Bob via `/wallet/tx/send` (Encrypted payload)
//! 3. ✅ Vérifier que l'UTXO d'Alice est **consommé** (removed from cache)
//! 4. ✅ Vérifier que le nouvel output de Bob **existe** (added to cache)
//! 5. ✅ Vérifier qu'une 2e TX utilisant les mêmes inputs échoue (double-spend)
//! ============================================================

use pms_storage::ConfigStorage;
use pms_testkit::{make_test_ctx_with_admin, mint_to_wallet_and_get_inputs, post_json};
use pms_wallet::{SignerBackend, Wallet};
use rust_decimal::Decimal;
use serde_json::json;

/// Helper pour nettoyer les variables d'environnement admin qui pourraient interférer
fn clear_admin_env_conflicts() {
    unsafe {
        for k in [
            "PMS__ADMIN__SIGNER_PUBKEYS",
            "PMS__ADMIN__SIGNER_PUBKEYS__0",
            "PMS__ADMIN__SIGNER_PUBKEYS__1",
            "PMS__ADMIN__SIGNER_PUBKEYS__2",
            "PMS__ADMIN",
        ] {
            std::env::remove_var(k);
        }
    }
}

/// ============================================================
/// TEST: Les transactions Encrypted mettent à jour le cache UTXO
/// ============================================================
///
/// Ce test vérifie le comportement corrigé par le fix:
/// ```rust
/// // Spend inputs (remove from UTXO cache)
/// for input in &tx.inputs {
///     adapter.utxos.remove(&output_id).await;
/// }
/// // Create outputs (add to UTXO cache)
/// for (idx, output) in tx.outputs.iter().enumerate() {
///     adapter.utxos.add(output_id, output.clone()).await;
/// }
/// ```
#[tokio::test]
async fn encrypted_tx_updates_utxo_cache_correctly() -> anyhow::Result<()> {
    clear_admin_env_conflicts();

    // ════════════════════════════════════════════════════════════════════════
    // 1) Setup: Créer le contexte de test avec admin configuré
    // ════════════════════════════════════════════════════════════════════════
    let admin = std::sync::Arc::new(Wallet::from_seed(&[7u8; 32], None).unwrap());
    let hrp = "8e";
    let admin_addr = admin.get_address(hrp);
    let admin_pubkey = admin.encoded_public_key();

    unsafe {
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", &admin_pubkey);
    }

    let ctx = make_test_ctx_with_admin(vec![admin_addr.clone()], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.clone();

    // ════════════════════════════════════════════════════════════════════════
    // 2) Créer 2 wallets: Alice (expéditeur) et Bob (destinataire)
    // ════════════════════════════════════════════════════════════════════════
    let alice = Wallet::from_seed(&[42u8; 32], None).unwrap();
    let alice_addr = alice.get_address(&hrp);
    let bob = Wallet::from_seed(&[43u8; 32], None).unwrap();
    let bob_addr = bob.get_address(&hrp);

    eprintln!("[TEST] Alice: {}", alice_addr);
    eprintln!("[TEST] Bob: {}", bob_addr);

    // ════════════════════════════════════════════════════════════════════════
    // 3) Mint initial: 5.00 PMS vers Alice (Plain payload)
    // ════════════════════════════════════════════════════════════════════════
    let (inputs, minted_amount) = mint_to_wallet_and_get_inputs(&ctx, &alice, "5.00").await?;

    // Force manual UTXO registration (bypass async persistence race)
    let u = inputs.first().expect("need at least one input from mint");
    {
        let ua = pms_storage::rocks_store::utxo::UtxoApply {
            txid: u.id.txid.clone(),
            inputs: vec![],
            outputs: vec![(alice_addr.clone(), u.amount.clone(), None)],
        };
        ctx.store.utxo_apply_tx_atomic(&ua).await?;
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    eprintln!("[TEST] Minted {} to Alice", minted_amount);
    eprintln!("[TEST] Input UTXO: {}:{}", u.id.txid, u.id.index);

    // ════════════════════════════════════════════════════════════════════════
    // 4) Vérifier que l'UTXO d'Alice EXISTE dans le cache RAM
    // ════════════════════════════════════════════════════════════════════════
    let adapter = ctx.srv.adapter_arc();
    let alice_utxos_before = adapter.utxos_by_address(&alice_addr).await;

    assert!(
        !alice_utxos_before.is_empty(),
        "❌ AVANT TX: Alice doit avoir au moins 1 UTXO après le mint"
    );
    eprintln!(
        "[TEST] ✅ AVANT TX: Alice a {} UTXO(s)",
        alice_utxos_before.len()
    );

    // Vérifier que Bob n'a PAS d'UTXO
    let bob_utxos_before = adapter.utxos_by_address(&bob_addr).await;
    assert!(
        bob_utxos_before.is_empty(),
        "❌ AVANT TX: Bob ne doit PAS avoir d'UTXO"
    );
    eprintln!("[TEST] ✅ AVANT TX: Bob a 0 UTXO");

    // ════════════════════════════════════════════════════════════════════════
    // 5) Préparer et envoyer la transaction Encrypted vers Bob
    // ════════════════════════════════════════════════════════════════════════
    let send_amount = "2.00";

    // Calcul des frais dynamiques
    let runtime_config = ctx.store.get_runtime_config().unwrap_or_default();
    let ratio_dec = Decimal::from(runtime_config.fee_rate_bps) / Decimal::from(10000);
    let fee_policy = pms_token::FeePolicy::new(&runtime_config.base_fee, &ratio_dec.to_string());
    let fee = fee_policy.compute_fee(send_amount).unwrap().to_string();

    // Calcul du change: input - send - fee
    let input_amt = Decimal::from_str_exact(&u.amount)?;
    let send_dec = Decimal::from_str_exact(send_amount)?;
    let fee_dec = Decimal::from_str_exact(&fee)?;
    let change_dec = input_amt - send_dec - fee_dec;
    let change = change_dec.to_string();

    eprintln!(
        "[TEST] TX: {} -> {} (send {})",
        alice_addr, bob_addr, send_amount
    );
    eprintln!("[TEST] Fee: {}, Change: {}", fee, change);

    let body = json!({
        "tx": {
            "inputs": [{ "out": { "txid": u.id.txid, "index": u.id.index } }],
            "outputs": [
                { "address": bob_addr, "amount": send_amount },
                { "address": admin_addr, "amount": &fee },
                { "address": alice_addr, "amount": &change }  // Change retourne vers Alice
            ],
            "fee": fee,
            "unlocks": []
        },
        // Encrypted: X25519 keys pour tous les participants
        "recipients_xpk": [
            alice.x25519_pub_hex.clone(),
            bob.x25519_pub_hex.clone()
        ]
    });

    let (status, json_resp) = post_json(&ctx.app, "/wallet/tx/send", body.clone()).await;
    assert!(
        status.is_success(),
        "❌ La TX encrypted a échoué: status={} body={}",
        status,
        json_resp
    );
    eprintln!("[TEST] ✅ TX Encrypted soumise avec succès");

    // Attendre la persistence async
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // ════════════════════════════════════════════════════════════════════════
    // 6) ASSERTION CLÉ: Vérifier que l'input d'Alice est CONSOMMÉ
    // ════════════════════════════════════════════════════════════════════════
    let consumed_output_id = pms_types::OutputId {
        txid: u.id.txid.clone(),
        index: u.id.index,
    };
    let input_still_exists = adapter.get_utxo(&consumed_output_id).await;

    assert!(
        input_still_exists.is_none(),
        "❌ BUG REGRESSION: L'input {}:{} n'a PAS été consommé du cache UTXO! \
         Cela signifie que la transaction Encrypted n'a pas appliqué le delta UTXO.",
        u.id.txid,
        u.id.index
    );
    eprintln!(
        "[TEST] ✅ L'input {}:{} a été correctement consommé (removed from cache)",
        u.id.txid, u.id.index
    );

    // ════════════════════════════════════════════════════════════════════════
    // 7) ASSERTION CLÉ: Vérifier que Bob a reçu son output
    // ════════════════════════════════════════════════════════════════════════
    let bob_utxos_after = adapter.utxos_by_address(&bob_addr).await;

    assert!(
        !bob_utxos_after.is_empty(),
        "❌ BUG REGRESSION: Bob n'a PAS reçu d'UTXO après la TX! \
         Cela signifie que les outputs Encrypted n'ont pas été ajoutés au cache."
    );

    // Vérifier le montant
    let bob_total: Decimal = bob_utxos_after
        .iter()
        .filter_map(|(_, out)| Decimal::from_str_exact(&out.amount).ok())
        .sum();

    assert_eq!(
        bob_total, send_dec,
        "❌ Le solde de Bob ({}) ne correspond pas au montant envoyé ({})",
        bob_total, send_dec
    );
    eprintln!(
        "[TEST] ✅ Bob a reçu {} PMS ({} UTXO(s))",
        bob_total,
        bob_utxos_after.len()
    );

    // ════════════════════════════════════════════════════════════════════════
    // 8) BONUS: Vérifier qu'un double-spend échoue
    // ════════════════════════════════════════════════════════════════════════
    let (status_double, _) = post_json(&ctx.app, "/wallet/tx/send", body).await;

    assert!(
        !status_double.is_success(),
        "❌ BUG REGRESSION: Le double-spend aurait dû échouer mais a réussi! \
         L'input devrait être marqué comme consommé."
    );
    eprintln!(
        "[TEST] ✅ Double-spend correctement rejeté (status={})",
        status_double
    );

    Ok(())
}

/// ============================================================
/// TEST: Chaîne de transactions Encrypted (TX1 → TX2)
/// ============================================================
///
/// Ce test vérifie qu'un output créé par une TX Encrypted peut être
/// dépensé par une autre TX Encrypted sans erreur "input not found".
#[tokio::test]
async fn encrypted_tx_chain_works_correctly() -> anyhow::Result<()> {
    clear_admin_env_conflicts();

    // Setup
    let admin = std::sync::Arc::new(Wallet::from_seed(&[7u8; 32], None).unwrap());
    let hrp = "8e";
    let admin_addr = admin.get_address(hrp);
    let admin_pubkey = admin.encoded_public_key();

    unsafe {
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", &admin_pubkey);
    }

    let ctx = make_test_ctx_with_admin(vec![admin_addr.clone()], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.clone();

    // 3 wallets: Alice → Bob → Charlie
    let alice = Wallet::from_seed(&[50u8; 32], None).unwrap();
    let bob = Wallet::from_seed(&[51u8; 32], None).unwrap();
    let charlie = Wallet::from_seed(&[52u8; 32], None).unwrap();

    let alice_addr = alice.get_address(&hrp);
    let bob_addr = bob.get_address(&hrp);
    let charlie_addr = charlie.get_address(&hrp);

    eprintln!("[TEST CHAIN] Alice: {}", alice_addr);
    eprintln!("[TEST CHAIN] Bob: {}", bob_addr);
    eprintln!("[TEST CHAIN] Charlie: {}", charlie_addr);

    // Mint vers Alice
    let (inputs, _) = mint_to_wallet_and_get_inputs(&ctx, &alice, "10.00").await?;
    let u = inputs.first().expect("need input");
    {
        let ua = pms_storage::rocks_store::utxo::UtxoApply {
            txid: u.id.txid.clone(),
            inputs: vec![],
            outputs: vec![(alice_addr.clone(), u.amount.clone(), None)],
        };
        ctx.store.utxo_apply_tx_atomic(&ua).await?;
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // ════════════════════════════════════════════════════════════════════════
    // TX1: Alice → Bob (4.00 PMS)
    // ════════════════════════════════════════════════════════════════════════
    let runtime_config = ctx.store.get_runtime_config().unwrap_or_default();
    let ratio_dec = Decimal::from(runtime_config.fee_rate_bps) / Decimal::from(10000);
    let fee_policy = pms_token::FeePolicy::new(&runtime_config.base_fee, &ratio_dec.to_string());

    let send1 = "4.00";
    let fee1 = fee_policy.compute_fee(send1).unwrap().to_string();
    let input_amt = Decimal::from_str_exact(&u.amount)?;
    let send1_dec = Decimal::from_str_exact(send1)?;
    let fee1_dec = Decimal::from_str_exact(&fee1)?;
    let change1 = (input_amt - send1_dec - fee1_dec).to_string();

    let body1 = json!({
        "tx": {
            "inputs": [{ "out": { "txid": u.id.txid, "index": u.id.index } }],
            "outputs": [
                { "address": bob_addr, "amount": send1 },
                { "address": admin_addr, "amount": &fee1 },
                { "address": alice_addr, "amount": &change1 }
            ],
            "fee": fee1,
            "unlocks": []
        },
        "recipients_xpk": [
            alice.x25519_pub_hex.clone(),
            bob.x25519_pub_hex.clone()
        ]
    });

    let (status1, resp1) = post_json(&ctx.app, "/wallet/tx/send", body1).await;
    assert!(status1.is_success(), "TX1 failed: {}", resp1);

    // Récupérer l'ID du bloc TX1 pour construire les inputs de TX2
    let tx1_id: String = resp1
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .expect("TX1 should return block id");

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    eprintln!("[TEST CHAIN] ✅ TX1 (Alice→Bob) réussie: {}", &tx1_id[..16]);

    // ════════════════════════════════════════════════════════════════════════
    // TX2: Bob → Charlie (2.00 PMS) en utilisant l'output de TX1
    // ════════════════════════════════════════════════════════════════════════
    // L'output de Bob (index 0) créé par TX1 devient l'input
    let send2 = "2.00";
    let fee2 = fee_policy.compute_fee(send2).unwrap().to_string();
    let bob_input_amt = send1_dec; // Bob a reçu 4.00
    let send2_dec = Decimal::from_str_exact(send2)?;
    let fee2_dec = Decimal::from_str_exact(&fee2)?;
    let change2 = (bob_input_amt - send2_dec - fee2_dec).to_string();

    let body2 = json!({
        "tx": {
            // Input: l'output index 0 de TX1 (celui vers Bob)
            "inputs": [{ "out": { "txid": tx1_id, "index": 0 } }],
            "outputs": [
                { "address": charlie_addr, "amount": send2 },
                { "address": admin_addr, "amount": &fee2 },
                { "address": bob_addr, "amount": &change2 }
            ],
            "fee": fee2,
            "unlocks": []
        },
        "recipients_xpk": [
            bob.x25519_pub_hex.clone(),
            charlie.x25519_pub_hex.clone()
        ]
    });

    let (status2, resp2) = post_json(&ctx.app, "/wallet/tx/send", body2).await;

    // ════════════════════════════════════════════════════════════════════════
    // ASSERTION CLÉ: TX2 doit réussir car l'output de TX1 est dans le cache
    // ════════════════════════════════════════════════════════════════════════
    assert!(
        status2.is_success(),
        "❌ BUG REGRESSION: TX2 (Bob→Charlie) a échoué alors que l'output de TX1 \
         devrait être disponible dans le cache UTXO!\n\
         Erreur: {}",
        resp2
    );

    eprintln!("[TEST CHAIN] ✅ TX2 (Bob→Charlie) réussie!");

    // Vérifier que Charlie a bien reçu
    let adapter = ctx.srv.adapter_arc();
    let charlie_utxos = adapter.utxos_by_address(&charlie_addr).await;
    assert!(
        !charlie_utxos.is_empty(),
        "❌ Charlie doit avoir reçu un UTXO de TX2"
    );
    eprintln!(
        "[TEST CHAIN] ✅ Charlie a {} UTXO(s) après la chaîne de TX",
        charlie_utxos.len()
    );

    Ok(())
}
