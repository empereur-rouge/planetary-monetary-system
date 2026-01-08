use pms_wallet::Wallet;

fn main() {
    let w = Wallet::generate();
    let json = serde_json::json!({
        "address": w.get_address("8e"),
        "private_key_b64": w.private_key_b64,
        "public_key_hex": w.public_key_hex,
        "x25519_pub_hex": w.x25519_pub_hex,
        "mnemonic_words": w.mnemonic_words
    });
    println!("{}", serde_json::to_string_pretty(&json).unwrap());
}
