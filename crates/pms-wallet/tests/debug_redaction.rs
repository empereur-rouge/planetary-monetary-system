//! Non-regression test for audit finding M1.
//!
//! A `dbg!(wallet)` or `println!("{wallet:?}")` must never reveal the
//! ECDSA private key or the BIP-39 mnemonic. `Wallet` used to derive
//! `Debug` automatically, which printed every field verbatim; v0.7.2 ships
//! a manual `impl Debug` that redacts the two secret fields.

use pms_wallet::Wallet;

#[test]
fn debug_output_never_leaks_private_key_or_mnemonic() {
    let wallet = Wallet::generate();

    let debug_str = format!("{:?}", wallet);

    println!("Debug output for Wallet:\n  {}", debug_str);
    println!(
        "  actual private_key_b64 (redacted in debug) length = {}",
        wallet.private_key_b64.len()
    );
    println!(
        "  actual mnemonic words count                       = {}",
        wallet.mnemonic_words.as_ref().map(|v| v.len()).unwrap_or(0)
    );

    // Hard invariants: the private key base64 and every mnemonic word MUST
    // be absent from the Debug output. Any accidental leak fails the test.
    assert!(
        !debug_str.contains(&wallet.private_key_b64),
        "private_key_b64 leaked in Debug output: {debug_str}"
    );

    if let Some(words) = wallet.mnemonic_words.as_ref() {
        for word in words {
            assert!(
                !debug_str.contains(word) || word.len() <= 3,
                "mnemonic word '{word}' leaked in Debug output: {debug_str}"
            );
        }
    }

    // Sanity check: the redacted markers are present so developers still
    // get useful hints about the hidden fields.
    assert!(
        debug_str.contains("redacted"),
        "Debug output should mark redacted fields: {debug_str}"
    );

    // Public fields are rendered as-is — still useful for debugging.
    assert!(
        debug_str.contains(&wallet.public_key_hex),
        "public_key_hex must still appear in Debug output: {debug_str}"
    );
    assert!(
        debug_str.contains(&wallet.x25519_pub_hex),
        "x25519_pub_hex must still appear in Debug output: {debug_str}"
    );
}
