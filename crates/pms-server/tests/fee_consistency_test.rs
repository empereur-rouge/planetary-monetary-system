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
    // Étape 2 : Vérifier le fee contre une valeur GOLDEN calculée À LA MAIN.
    //
    // NB (v0.9.3) : l'ancienne version reconstruisait `FeePolicy::new(...)` avec la
    // MÊME config que prepare_tx puis assertait l'égalité → `f(x) == f(x)`, une
    // tautologie qui ne détectait AUCUN bug de FeePolicy (les deux côtés bougent
    // ensemble). On pinne maintenant la config du contexte de test et on compare à
    // un montant calculé en arithmétique Decimal brute, SANS passer par FeePolicy.
    // ════════════════════════════════════════════════════════════════════════
    use pms_config::RuntimeConfig;
    use pms_storage::ConfigStorage;

    let runtime_config = ctx
        .store
        .get_runtime_config()
        .unwrap_or_else(|_| RuntimeConfig::default());

    // Pin la config : si elle change, le golden ci-dessous doit être recalculé
    // (échec explicite plutôt qu'un faux positif silencieux).
    assert_eq!(
        runtime_config.fee_rate_bps, 300,
        "test golden assumes 3% (300 bps)"
    );
    assert_eq!(
        runtime_config.base_fee, "0.0000001",
        "test golden assumes base_fee 1e-7"
    );

    // Calcul indépendant (Decimal brut) : fee = base_fee + amount * (bps/10_000)
    //                                         = 0.0000001 + 5.00 * 0.03 = 0.1500001
    let amount_dec = Decimal::from_str_exact(amount)?;
    let independent_fee = (Decimal::from_str_exact("0.0000001")?
        + amount_dec * (Decimal::from(300) / Decimal::from(10_000)))
    .round_dp(8);

    let prepare_fee_dec = Decimal::from_str_exact(prepare_fee)?;
    eprintln!("[TEST] prepare fee={prepare_fee_dec}, independent golden={independent_fee}");

    // Golden littéral : prouve la VALEUR, pas seulement la cohérence interne.
    assert_eq!(
        prepare_fee_dec,
        Decimal::from_str_exact("0.1500001")?,
        "prepare_tx fee must equal golden 0.1500001 for 5.00 @ 3% + 1e-7 base"
    );
    // Et il doit coïncider avec le calcul main (hors FeePolicy).
    assert_eq!(
        prepare_fee_dec, independent_fee,
        "prepare_tx fee must match the hand-computed (non-FeePolicy) value"
    );

    // ≤ 8 décimales (toujours).
    if let Some(dot_pos) = prepare_fee.find('.') {
        let decimals = prepare_fee.len() - dot_pos - 1;
        assert!(
            decimals <= 8,
            "Fee '{prepare_fee}' has {decimals} decimals, expected <= 8 (precision bug!)"
        );
    }

    eprintln!("[TEST] ✅ prepare_tx fee = golden {prepare_fee}");

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

    // Mint ASSEZ pour couvrir montant + fee. NB (v0.9.3) : l'ancienne version
    // mintait pile 0.03549294 puis envoyait 0.03549294 → "insufficient balance"
    // (pas de place pour le fee), donc tout le bloc d'assertions était sous un
    // `if status.is_success()` JAMAIS atteint → 0 assertion exécutée. On mint 1.00
    // pour garantir que prepare_tx réussit et que l'assertion tourne réellement.
    let (inputs, _) = mint_to_wallet_and_get_inputs(&ctx, &w_from, "1.00").await?;
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

    // Montant qui produirait > 8 décimales sans arrondi :
    //   0.0000001 (base) + 0.03549294 * 0.03 = 0.0010648882 → round8 = 0.00106489
    let prepare_body = json!({
        "from": from_addr,
        "to": to_addr,
        "amount": "0.03549294"
    });

    let (status, json) = post_json(&ctx.app, "/v1/tx/prepare", prepare_body).await;
    assert!(
        status.is_success(),
        "prepare_tx must succeed with sufficient balance: status={status} body={json}"
    );

    let fee = json["fee"]
        .as_str()
        .expect("prepare response must have 'fee' field");
    eprintln!("[TEST] prepare_tx fee for 0.03549294: {fee}");

    // Golden : prouve l'arrondi banker's à 8 décimales sur une valeur à 10 décimales.
    assert_eq!(
        fee, "0.00106489",
        "fee must be the 8-decimal rounded golden value (was the precision bug)"
    );
    let decimals = fee.find('.').map(|p| fee.len() - p - 1).unwrap_or(0);
    assert!(decimals <= 8, "Fee '{fee}' has {decimals} decimals, expected <= 8");

    eprintln!("[TEST] ✅ Fee precision correct: {fee} (≤ 8 decimals)");
    Ok(())
}
