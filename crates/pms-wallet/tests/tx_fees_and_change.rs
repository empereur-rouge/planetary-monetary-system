// crates/pms-wallet/tests/tx_fees_and_change.rs

use pms_token::{Amount, FeePolicy};
use pms_types_transaction::OutputId;
use pms_wallet::{Payment, SelectedInput, WalletTxError, build_utxo_tx_with_fee_checked};
use rust_decimal::Decimal;

#[test]
fn tx_build_with_fee_policy_happy_path() {
    // FeePolicy réelle : base = 0.1 PMS, ratio = 1%
    let fee_policy = FeePolicy::new("0.1", "0.01");

    let from = "PMS_FROM";
    let fee_recipient = "FEE_RECIPIENT";

    let payments = vec![
        Payment {
            to: "A1".into(),
            amount: "10".into(),
            asset_id: None,
        },
        Payment {
            to: "A2".into(),
            amount: "5".into(),
            asset_id: None,
        },
    ];

    let inputs = vec![
        SelectedInput {
            id: OutputId {
                txid: "T1".into(),
                index: 0,
            },
            amount: "10".into(),
        },
        SelectedInput {
            id: OutputId {
                txid: "T2".into(),
                index: 0,
            },
            amount: "10".into(),
        },
    ];

    let tx = build_utxo_tx_with_fee_checked(from, inputs, payments, &fee_policy, fee_recipient)
        .expect("tx should succeed");

    // Fee calculée : base 0.1 + ratio 1% * amount_out(=15)
    // fee = 0.1 + 0.15 = 0.25 PMS
    let expected_fee = Amount::parse_pms("0.25").unwrap().inner();
    let fee = Amount::parse_pms(&tx.fee).unwrap().inner();
    assert_eq!(fee, expected_fee);

    // Conservation des montants
    let sum_in = Amount::parse_pms("20").unwrap().inner();

    let mut sum_out = Decimal::ZERO;
    for o in &tx.outputs {
        sum_out += Amount::parse_pms(&o.amount).unwrap().inner();
    }

    assert_eq!(sum_in, sum_out, "inputs == outputs");

    // Fee sortie doit exister
    let fee_out = tx
        .outputs
        .iter()
        .find(|o| o.address == fee_recipient)
        .expect("fee output present");
    assert_eq!(Amount::parse_pms(&fee_out.amount).unwrap().inner(), fee);

    // Change doit aller à from
    assert_eq!(tx.outputs.last().unwrap().address, from);
}

#[test]
fn tx_build_insufficient_inputs() {
    let fee_policy = FeePolicy::new("0.1", "0.01");

    let from = "PMS_FROM";
    let fee_recipient = "FEE_RECIPIENT";

    let payments = vec![Payment {
        to: "A1".into(),
        amount: "100".into(),
        asset_id: None,
    }];

    let inputs = vec![SelectedInput {
        id: OutputId {
            txid: "T1".into(),
            index: 0,
        },
        amount: "10".into(),
    }];

    let r = build_utxo_tx_with_fee_checked(from, inputs, payments, &fee_policy, fee_recipient);

    match r {
        Err(WalletTxError::InsufficientInputs) => {} // ok
        other => panic!("expected InsufficientInputs, got {other:?}"),
    }
}
