use pms_wallet::Wallet;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

fn main() {
    let expected_address = "8e1ahpltzjauwev6lf0jql9szau3gl44u9rye4u6vat95sd9dzx23yvfmlt25q9avx7wxmc2qvcgwpaypegmq9s6u5fr9";
    let expected_pubkey = "04593037fa9d4f6ea21b56cdd0cd504a718e3a8e2502df863ee9ee2fbd431f1c3642b309dcab07c63baa32232b643e86b433045cf31859f9279e7dc6703df02a84";
    let priv_hex = "da5334ad57455d74e5150e4eb06398ce9cf27762b6f29eac7f82f178bee07406";
    let mnemonic = "gain space color filter buzz bind side before sauce twist slam history chief patch desk chunk way oblige output turtle purchase scare token rapid";

    println!("=== FROM PRIVATE KEY HEX ===");
    let w_hex = Wallet::from_hex(priv_hex).expect("from_hex");
    let addr_hex = w_hex.get_address("8e");
    println!("  address:     {}", addr_hex);
    println!("  public_key:  {}", w_hex.public_key_hex);
    println!("  x25519_pub:  {}", w_hex.x25519_pub_hex);
    println!("  priv_b64:    {}", w_hex.private_key_b64);
    println!("  match addr:  {}", addr_hex == expected_address);
    println!("  match pub:   {}", w_hex.public_key_hex == expected_pubkey);

    println!();
    println!("=== FROM MNEMONIC ===");
    let words: Vec<&str> = mnemonic.split_whitespace().collect();
    let w_mne = Wallet::from_word_list(&words).expect("from_word_list");
    let addr_mne = w_mne.get_address("8e");
    println!("  address:     {}", addr_mne);
    println!("  public_key:  {}", w_mne.public_key_hex);
    println!("  x25519_pub:  {}", w_mne.x25519_pub_hex);
    println!("  priv_b64:    {}", w_mne.private_key_b64);

    // Decode b64 to hex for comparison
    let mne_priv_bytes = STANDARD.decode(&w_mne.private_key_b64).unwrap();
    let mne_priv_hex = hex::encode(&mne_priv_bytes);
    println!("  priv_hex:    {}", mne_priv_hex);
    println!("  match addr:  {}", addr_mne == expected_address);
    println!("  match pub:   {}", w_mne.public_key_hex == expected_pubkey);

    println!();
    println!("=== CROSS-CHECK ===");
    println!("  hex vs mne addr match:    {}", addr_hex == addr_mne);
    println!("  hex vs mne pubkey match:  {}", w_hex.public_key_hex == w_mne.public_key_hex);
    println!("  hex vs mne x25519 match:  {}", w_hex.x25519_pub_hex == w_mne.x25519_pub_hex);
    println!("  hex vs mne privkey match: {}", w_hex.private_key_b64 == w_mne.private_key_b64);
    println!("  mne priv_hex == input:    {}", mne_priv_hex == priv_hex);

    println!();
    println!("=== EXPECTED VS ACTUAL ===");
    println!("  expected addr: {}", expected_address);
    println!("  hex addr:      {}", addr_hex);
    println!("  mne addr:      {}", addr_mne);
    if addr_hex != expected_address {
        println!();
        println!("  !!! ADDRESS MISMATCH — the stored address was computed differently !!!");
        println!("  Likely cause: the coordinator.json was generated with an older/buggy X25519 derivation");
    }
}
