use super::*;
use pms_types::{Transaction, TxOutput};

fn out(addr: &str, amount: &str) -> TxOutput {
    TxOutput::new(addr, amount, None)
}

// -- is_fee_output_only --

#[test]
fn is_fee_output_only_basic() {
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("receiver", "99"), out("admin", "1")],
        fee: "1".into(),
        unlocks: vec![],
    };
    println!("admin (amount=1, fee=1): {}", is_fee_output_only(&tx, "admin"));
    println!("receiver (amount=99, fee=1): {}", is_fee_output_only(&tx, "receiver"));
    assert!(is_fee_output_only(&tx, "admin"));
    assert!(!is_fee_output_only(&tx, "receiver"));
}

#[test]
fn is_fee_output_only_zero_fee() {
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("alice", "100")],
        fee: "0".into(),
        unlocks: vec![],
    };
    println!("zero fee -> {}", is_fee_output_only(&tx, "alice"));
    assert!(!is_fee_output_only(&tx, "alice"));
}

#[test]
fn is_fee_output_only_decimal() {
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("bob", "9.9996999"), out("treasury", "0.0003001")],
        fee: "0.0003001".into(),
        unlocks: vec![],
    };
    println!("treasury (0.0003001 == fee): {}", is_fee_output_only(&tx, "treasury"));
    println!("bob (9.9996999 != fee): {}", is_fee_output_only(&tx, "bob"));
    assert!(is_fee_output_only(&tx, "treasury"));
    assert!(!is_fee_output_only(&tx, "bob"));
}

#[test]
fn is_fee_output_only_addr_not_in_outputs() {
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("bob", "99"), out("admin", "1")],
        fee: "1".into(),
        unlocks: vec![],
    };
    // Address not in outputs -> sum=0 != fee -> false
    println!("unknown addr: {}", is_fee_output_only(&tx, "unknown"));
    assert!(!is_fee_output_only(&tx, "unknown"));
}

// -- classify_for_storage: fee detection --

#[test]
fn classify_for_storage_fee_receiver() {
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("receiver", "99"), out("admin", "1")],
        fee: "1".into(),
        unlocks: vec![],
    };
    let plain = pms_types_payload::PlainPayload::TxUtxo(tx);

    // admin is fee-only receiver -> fee_received
    let items = classify_for_storage(&plain, "admin", None);
    println!("admin items: {:?}", items.iter().map(|i| (&i.activity_type, &i.amount)).collect::<Vec<_>>());
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "fee_received");
    assert_eq!(items[0].amount.as_deref(), Some("1"));

    // receiver is NOT fee -> transfer_in
    let items = classify_for_storage(&plain, "receiver", None);
    println!("receiver items: {:?}", items.iter().map(|i| (&i.activity_type, &i.amount)).collect::<Vec<_>>());
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "transfer_in");
}

#[test]
fn classify_for_storage_sender_with_fee_output() {
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("bob", "99"), out("admin", "1")],
        fee: "1".into(),
        unlocks: vec![],
    };
    let plain = pms_types_payload::PlainPayload::TxUtxo(tx);

    // sender sees transfer_out (fee detection only applies to receivers)
    let items = classify_for_storage(&plain, "sender", Some("sender"));
    println!("sender items: {:?}", items.iter().map(|i| (&i.activity_type, &i.amount)).collect::<Vec<_>>());
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].activity_type, "transfer_out");
    // total sent to non-sender outputs: bob(99) + admin(1) = 100
    assert_eq!(items[0].amount.as_deref(), Some("100"));
}

// -- extract_involved_with_category: fee detection --

#[test]
fn extract_category_txutxo_with_fee_output() {
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("receiver", "99"), out("admin", "1")],
        fee: "1".into(),
        unlocks: vec![],
    };
    let plain = pms_types_payload::PlainPayload::TxUtxo(tx);
    let cats = extract_involved_with_category(&plain);
    println!("categories: {:?}", cats.iter().map(|(a, c)| (a.as_str(), *c)).collect::<Vec<_>>());

    // receiver -> Transfer, admin -> Fee
    assert_eq!(cats.len(), 2);
    let admin_cat = cats.iter().find(|(a, _)| a == "admin").unwrap();
    let recv_cat = cats.iter().find(|(a, _)| a == "receiver").unwrap();
    assert_eq!(admin_cat.1, ActivityCategory::Fee);
    assert_eq!(recv_cat.1, ActivityCategory::Transfer);
}

#[test]
fn extract_category_txutxo_no_fee_output() {
    // fee is 1 but nobody receives exactly 1 -> all Transfer
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("alice", "50"), out("bob", "49")],
        fee: "1".into(),
        unlocks: vec![],
    };
    let plain = pms_types_payload::PlainPayload::TxUtxo(tx);
    let cats = extract_involved_with_category(&plain);
    println!("categories (no fee output): {:?}", cats.iter().map(|(a, c)| (a.as_str(), *c)).collect::<Vec<_>>());

    assert!(cats.iter().all(|(_, c)| *c == ActivityCategory::Transfer));
}

#[test]
fn extract_category_txutxo_decimal_fee() {
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("bob", "9.9996999"), out("treasury", "0.0003001")],
        fee: "0.0003001".into(),
        unlocks: vec![],
    };
    let plain = pms_types_payload::PlainPayload::TxUtxo(tx);
    let cats = extract_involved_with_category(&plain);
    println!("decimal fee categories: {:?}", cats.iter().map(|(a, c)| (a.as_str(), *c)).collect::<Vec<_>>());

    let treasury_cat = cats.iter().find(|(a, _)| a == "treasury").unwrap();
    let bob_cat = cats.iter().find(|(a, _)| a == "bob").unwrap();
    assert_eq!(treasury_cat.1, ActivityCategory::Fee);
    assert_eq!(bob_cat.1, ActivityCategory::Transfer);
}

// -- precompute_all_items: fee detection --

#[test]
fn precompute_all_items_fee_output() {
    let tx = Transaction {
        inputs: vec![],
        outputs: vec![out("receiver", "99"), out("admin", "1")],
        fee: "1".into(),
        unlocks: vec![],
    };
    let plain = pms_types_payload::PlainPayload::TxUtxo(tx);
    let addrs = extract_involved_addresses(&plain);
    let items_map = precompute_all_items(&plain, &addrs, None);

    println!("precompute keys: {:?}", items_map.keys().collect::<Vec<_>>());
    for (addr, items) in &items_map {
        println!("  {}: {:?}", addr, items.iter().map(|i| &i.activity_type).collect::<Vec<_>>());
    }

    let admin_items = items_map.get("admin").unwrap();
    assert_eq!(admin_items[0].activity_type, "fee_received");

    let recv_items = items_map.get("receiver").unwrap();
    assert_eq!(recv_items[0].activity_type, "transfer_in");
}
