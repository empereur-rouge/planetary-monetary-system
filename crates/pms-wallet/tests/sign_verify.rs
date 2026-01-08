use pms_wallet::{SignerBackend, Wallet};

#[test]
fn sign_verify_ok() {
    let ent = [7u8; 32];
    let (w, _) = Wallet::generate_with_entropy(ent).unwrap();
    let msg = "hello";
    let sig_b64 = w.sign(msg).unwrap();
    assert!(w.verify(msg, &sig_b64).unwrap());
    assert!(!w.verify("tampered", &sig_b64).unwrap());
}
