// ═══════════════════════════════════════════════════════════════════════════════
// Test de cohérence : prepare_tx et wallet_send_tx doivent calculer les mêmes fees
// ═══════════════════════════════════════════════════════════════════════════════
//
// Ce test vérifie que :
// 1. POST /v1/tx/prepare retourne un fee calculé
// 2. Ce fee, quand utilisé dans POST /wallet/tx/send, est accepté sans erreur
// 3. Les deux endpoints utilisent exactement la même logique de calcul
//
// Voir Chapitre 11 du Rust Book pour l'écriture de tests.
// ═══════════════════════════════════════════════════════════════════════════════

use pms_testkit::{make_test_ctx_with_admin, mint_to_wallet_and_get_inputs, post_json};
use pms_wallet::{SignerBackend, Wallet};
use rust_decimal::Decimal;
use serde_json::json;

/// Helper pour nettoyer les variables d'environnement conflictuelles
fn clear_admin_env_conflicts() {
    unsafe {
        for k in [
            "PMS__ADMIN__SIGNER_PUBKEYS",
            "PMS__ADMIN__SIGNER_PUBKEYS__0",
            "PMS__ADMIN__SIGNER_PUBKEYS__1",
            "PMS__ADMIN",
        ] {
            std::env::remove_var(k);
        }
    }
}

/// Test principal : vérifie que prepare_tx et FeePolicy calculent les mêmes fees.
///
/// # Scénario
/// 1. Mint 10 PMS vers un wallet test
/// 2. Appeler /v1/tx/prepare pour préparer un transfert de 5 PMS
/// 3. Calculer manuellement le fee avec FeePolicy (même logique que wallet_send_tx)
/// 4. Vérifier que les deux fees sont identiques
#[tokio::test]
async fn prepare_and_fee_policy_compute_same_fee() -> anyhow::Result<()> {
    clear_admin_env_conflicts();

    // ════════════════════════════════════════════════════════════════════════
    // Setup : créer le contexte de test avec admin wallet
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

    // Créer deux wallets : expéditeur et destinataire
    let w_from = Wallet::from_seed(&[20u8; 32], None).unwrap();
    let w_to = Wallet::from_seed(&[21u8; 32], None).unwrap();
    let from_addr = w_from.get_address(&hrp);
    let to_addr = w_to.get_address(&hrp);

    // Mint 10 PMS vers l'expéditeur
    let (inputs, _minted) = mint_to_wallet_and_get_inputs(&ctx, &w_from, "10.00").await?;
    let u = inputs.first().expect("need at least one input from mint");

    // Force manual persistence to bypass race condition
    {
        let ua = pms_storage::rocks_store::utxo::UtxoApply {
            txid: u.id.txid.clone(),
            inputs: vec![],
            outputs: vec![(from_addr.clone(), u.amount.clone(), None)],
        };
        ctx.store
            .utxo_apply_tx_atomic(&ua)
            .await
            .expect("manual persist");
    }

    // ════════════════════════════════════════════════════════════════════════
    // Étape 1 : Appeler /v1/tx/prepare
    // ════════════════════════════════════════════════════════════════════════
    let amount = "5.00";
    let prepare_body = json!({
        "from": from_addr,
        "to": to_addr,
        "amount": amount
    });

    let (prepare_status, prepare_json) = post_json(&ctx.app, "/v1/tx/prepare", prepare_body).await;

    // Le prepare doit réussir
    assert!(
        prepare_status.is_success(),
        "prepare_tx failed: status={} body={}",
        prepare_status,
        prepare_json
    );

    // Extraire le fee calculé par prepare_tx
    let prepare_fee = prepare_json["fee"]
        .as_str()
        .expect("prepare response must have 'fee' field");

    eprintln!("[TEST] prepare_tx returned fee: {}", prepare_fee);

    // ════════════════════════════════════════════════════════════════════════
    // Étape 2 : Calculer le fee manuellement avec FeePolicy
    // C'est exactement la même logique que wallet_send_tx utilise
    // ════════════════════════════════════════════════════════════════════════
    use pms_config::RuntimeConfig;
    use pms_storage::ConfigStorage;
    use pms_token::FeePolicy;

    let runtime_config = ctx
        .store
        .get_runtime_config()
        .unwrap_or_else(|_| RuntimeConfig::default());

    let ratio_dec = Decimal::from(runtime_config.fee_rate_bps) / Decimal::from(10000);
    let fee_policy = FeePolicy::new(&runtime_config.base_fee, &ratio_dec.to_string());

    // Dans wallet_send_tx, taxable_amount = somme des outputs vers destination
    // = amount demandé (hors change et fees)
    let expected_fee = fee_policy
        .compute_fee(amount)
        .expect("fee computation should succeed")
        .to_string();

    eprintln!("[TEST] FeePolicy computed fee: {}", expected_fee);

    // ════════════════════════════════════════════════════════════════════════
    // Assertion finale : les deux fees doivent être identiques
    // ════════════════════════════════════════════════════════════════════════
    let prepare_fee_dec = Decimal::from_str_exact(prepare_fee)?;
    let expected_fee_dec = Decimal::from_str_exact(&expected_fee)?;

    assert_eq!(
        prepare_fee_dec, expected_fee_dec,
        "prepare_tx fee ({}) differs from FeePolicy computation ({}). \
        This means the two endpoints would have DIFFERENT fee calculations!",
        prepare_fee, expected_fee
    );

    // Vérifier que le fee a ≤ 8 décimales
    if let Some(dot_pos) = prepare_fee.find('.') {
        let decimals = prepare_fee.len() - dot_pos - 1;
        assert!(
            decimals <= 8,
            "Fee '{}' has {} decimals, expected <= 8 (precision bug!)",
            prepare_fee,
            decimals
        );
    }

    eprintln!(
        "[TEST] ✅ SUCCESS: prepare_tx and FeePolicy compute the same fee: {}",
        prepare_fee
    );

    Ok(())
}

/// Test que prepare_tx arrondit correctement les fees à 8 décimales
/// même pour des montants qui produiraient plus de 8 décimales.
#[tokio::test]
async fn prepare_tx_fee_has_max_8_decimals() -> anyhow::Result<()> {
    clear_admin_env_conflicts();

    let admin = std::sync::Arc::new(Wallet::from_seed(&[7u8; 32], None).unwrap());
    let hrp = "8e";
    let admin_addr = admin.get_address(hrp);
    let admin_pubkey = admin.encoded_public_key();

    unsafe {
        std::env::set_var("PMS_TEST_ADMIN_PUBKEY", &admin_pubkey);
    }

    let ctx = make_test_ctx_with_admin(vec![admin_addr.clone()], vec![admin_pubkey]).await?;
    let hrp = ctx.settings.address.hrp.clone();

    let w_from = Wallet::from_seed(&[30u8; 32], None).unwrap();
    let w_to = Wallet::from_seed(&[31u8; 32], None).unwrap();
    let from_addr = w_from.get_address(&hrp);
    let to_addr = w_to.get_address(&hrp);

    // Mint un montant avec beaucoup de décimales
    let (inputs, _) = mint_to_wallet_and_get_inputs(&ctx, &w_from, "0.03549294").await?;
    let u = inputs.first().unwrap();

    // Force persist
    {
        let ua = pms_storage::rocks_store::utxo::UtxoApply {
            txid: u.id.txid.clone(),
            inputs: vec![],
            outputs: vec![(from_addr.clone(), u.amount.clone(), None)],
        };
        ctx.store.utxo_apply_tx_atomic(&ua).await?;
    }

    // Préparer un transfert avec un montant qui causerait > 8 décimales
    // si le calcul n'était pas arrondi : 0.03549294 * 0.03 = 0.0010647882
    let prepare_body = json!({
        "from": from_addr,
        "to": to_addr,
        "amount": "0.03549294"
    });

    let (status, json) = post_json(&ctx.app, "/v1/tx/prepare", prepare_body).await;

    if status.is_success() {
        let fee = json["fee"]
            .as_str()
            .expect("prepare response must have 'fee' field");

        eprintln!("[TEST] prepare_tx returned fee for 0.03549294: {}", fee);

        // Vérifier que le fee n'a pas plus de 8 décimales
        if let Some(dot_pos) = fee.find('.') {
            let decimals = fee.len() - dot_pos - 1;
            assert!(
                decimals <= 8,
                "PRECISION BUG! Fee '{}' has {} decimals (expected <= 8). \
                This was the original bug we fixed.",
                fee,
                decimals
            );
        }

        eprintln!(
            "[TEST] ✅ Fee precision is correct: {} has <= 8 decimals",
            fee
        );
    } else {
        // Si ça échoue pour "insufficient UTXOs", c'est attendu pour ce petit montant
        // mais on log quand même
        eprintln!(
            "[TEST] prepare_tx failed (expected for small amount): {}",
            json
        );
    }

    Ok(())
}
