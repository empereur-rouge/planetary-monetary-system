use crate::SignerBackend;
use crate::utxo_store::gather_wallet_utxos_dec;
use anyhow::Result;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bech32::{FromBase32, Variant, decode};
use bip39::rand_core::OsRng;
use bip39::{Language, Mnemonic};
use hkdf::Hkdf;
use pms_storage::rocks_store::store::RocksStore;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::fs;
use std::path::Path;
use x25519_dalek::{PublicKey as XPublic, StaticSecret};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wallet {
    pub private_key_b64: String, // clé ECDSA (signatures)
    pub public_key_hex: String,  // pub ECDSA
    pub x25519_pub_hex: String,  // pub pour chiffrement (NOUVEAU)
    pub mnemonic_words: Option<Vec<String>>,
}

impl Wallet {
    pub fn generate() -> Self {
        let mnemonic = Mnemonic::generate_in_with(&mut OsRng, Language::English, 24).unwrap();
        let seed = mnemonic.to_seed_normalized("");
        let mnemonic_words = Some(mnemonic.words().map(|w| w.to_string()).collect());

        let backend = Self::from_seed(&seed, mnemonic_words.clone()).unwrap();

        let mut wallet = Self {
            private_key_b64: backend.encoded_private_key(),
            public_key_hex: backend.encoded_public_key(),
            x25519_pub_hex: String::new(),
            mnemonic_words,
        };

        if let Some((_sk, pk)) = wallet.derive_x25519_pair_from_private_key_b64() {
            wallet.x25519_pub_hex = pk;
        }

        wallet
    }

    pub fn generate_with_entropy(entropy: [u8; 32]) -> Result<(Self, String), String> {
        let mnemonic = Mnemonic::from_entropy_in(Language::English, &entropy)
            .map_err(|e| format!("Failed to generate mnemonic: {e}"))?;
        let seed = mnemonic.to_seed_normalized("");
        let mnemonic_words = Some(mnemonic.words().map(|w| w.to_string()).collect());

        let backend = Self::from_seed(&seed, mnemonic_words.clone())?;

        let mut wallet = Self {
            private_key_b64: backend.encoded_private_key(),
            public_key_hex: backend.encoded_public_key(),
            x25519_pub_hex: String::new(),
            mnemonic_words,
        };

        let (_sk, pk) = wallet
            .derive_x25519_pair_from_private_key_b64()
            .expect("x25519 derivation must work");

        wallet.x25519_pub_hex = pk;

        Ok((wallet, mnemonic.to_string()))
    }

    pub fn from_word_list(words: &[&str]) -> Result<Self, String> {
        if words.len() != 24 {
            return Err("You must provide exactly 24 words".into());
        }

        let phrase = words.join(" ");
        let mnemonic = Mnemonic::parse_in_normalized(Language::English, &phrase)
            .map_err(|e| format!("Invalid mnemonic: {e}"))?;
        let seed = mnemonic.to_seed_normalized("");
        let mnemonic_words = Some(words.iter().map(|w| w.to_string()).collect());

        let backend = Self::from_seed(&seed, mnemonic_words.clone())?;

        let mut wallet = Self {
            private_key_b64: backend.encoded_private_key(),
            public_key_hex: backend.encoded_public_key(),
            x25519_pub_hex: String::new(),
            mnemonic_words,
        };

        let (_sk, pk) = wallet
            .derive_x25519_pair_from_private_key_b64()
            .expect("x25519 derivation must work");

        wallet.x25519_pub_hex = pk;
        Ok(wallet)
    }

    pub fn save_to_file(&self, path: &str) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, json).map_err(|e| e.to_string())
    }

    pub fn load_from_file(path: &str) -> Result<Self, String> {
        if !Path::new(path).exists() {
            return Err("Wallet file not found".into());
        }
        let json = fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&json).map_err(|e| e.to_string())
    }

    pub fn mnemonic_words(&self) -> Option<Vec<&str>> {
        self.mnemonic_words
            .as_ref()
            .map(|v| v.iter().map(String::as_str).collect())
    }

    pub fn get_address(&self, hrp: &str) -> String {
        use bech32::{ToBase32, Variant, encode};
        use sha2::{Digest, Sha256};

        let pub_bytes = hex::decode(&self.public_key_hex).unwrap();
        let hash = Sha256::digest(&pub_bytes); // 32 bytes
        let h20 = &hash[..20]; // 20 bytes
        let xpk = hex::decode(&self.x25519_pub_hex).unwrap();

        let mut payload = Vec::with_capacity(52);
        payload.extend_from_slice(h20);
        payload.extend_from_slice(&xpk);

        encode(hrp, payload.to_base32(), Variant::Bech32m).unwrap()
    }

    /// Fingerprint court pour affichage rapide (8 premiers chars).
    pub fn short_address(&self, hrp: &str) -> String {
        let addr = self.get_address(hrp);
        addr.chars().take(12).collect()
    }

    /// Recalcule (sk_hex, pk_hex) X25519 depuis private_key_b64.
    pub fn derive_x25519_pair_from_private_key_b64(&self) -> Option<(String, String)> {
        let ecdsa_priv = STANDARD.decode(&self.private_key_b64).ok()?;

        let hk = Hkdf::<Sha256>::new(None, &ecdsa_priv);
        let mut sk_bytes = [0u8; 32];
        hk.expand(b"pms/x25519-sk/v1", &mut sk_bytes).ok()?;

        let sk = StaticSecret::from(sk_bytes);
        let pk = XPublic::from(&sk);

        Some((hex::encode(sk.to_bytes()), hex::encode(pk.as_bytes())))
    }

    /// Clé secrète X25519 hex (runtime).
    pub fn x25519_sk_hex(&self) -> Option<String> {
        self.derive_x25519_pair_from_private_key_b64()
            .map(|(sk, _pk)| sk)
    }

    /// (Optionnel) check invariant : pk stockée == pk dérivée.
    pub fn assert_x25519_consistent(&self) -> bool {
        match self.derive_x25519_pair_from_private_key_b64() {
            Some((_sk, pk)) => pk.eq_ignore_ascii_case(&self.x25519_pub_hex),
            None => false,
        }
    }

    pub async fn balance(
        &self,
        store: &RocksStore,
        hrp: &str,
        scan_limit: usize,
    ) -> anyhow::Result<Decimal> {
        // On récupère la clé secrète x25519 pour déchiffrer
        let sk_hex = self
            .x25519_sk_hex()
            .ok_or_else(|| anyhow::anyhow!("Wallet sans mnemonic → pas de clé X25519"))?;

        // On appelle ton scanner UTXO déchiffré
        let utxos = gather_wallet_utxos_dec(
            store,
            &self.public_key_hex,
            &self.x25519_pub_hex,
            &sk_hex,
            hrp,
            scan_limit,
        )
        .await?;

        // Somme des UTXOs
        let mut total = Decimal::ZERO;
        for u in utxos {
            total += u.amount;
        }
        Ok(total)
    }

    /// Charge le wallet "node identity" depuis un fichier de clé.
    /// Le format exact dépend de ce que tu as choisi (JSON, binaire, etc.).
    pub fn load_from_node_key_file(path: &str) -> Result<Self> {
        let p = Path::new(path);

        // 1) Lire le fichier (trim whitespace)
        let raw_data =
            fs::read(p).map_err(|e| anyhow::anyhow!("read node key file {}: {e}", p.display()))?;
        let content_str = String::from_utf8(raw_data.clone()).unwrap_or_default();
        let trimmed = content_str.trim();

        // CAS A: Clé Privée Hexadécimale (64 chars) - Format 'keygen'
        // C'est ce qu'on attend pour le Coordinateur.
        if trimmed.len() == 64 {
            if let Ok(priv_bytes) = hex::decode(trimmed) {
                // On a une clé privée brute.
                // On reconstruit le wallet SANS dérivation BIP39.
                // Note: On encode en Base64 car c'est le format interne de Wallet.
                let priv_b64 = STANDARD.encode(&priv_bytes);

                // Dérive la PubKey ECDSA
                let signing_key = k256::ecdsa::SigningKey::from_slice(&priv_bytes)
                    .map_err(|e| anyhow::anyhow!("Invalid ECDSA private key from file: {e}"))?;
                let verify_key = signing_key.verifying_key();
                let pub_hex = hex::encode(verify_key.to_sec1_bytes());

                let mut w = Wallet {
                    private_key_b64: priv_b64,
                    public_key_hex: pub_hex,
                    x25519_pub_hex: String::new(),
                    mnemonic_words: None,
                };

                // Dérive X25519 (toujours utile pour le chifrrement)
                if let Some((_, pk)) = w.derive_x25519_pair_from_private_key_b64() {
                    w.x25519_pub_hex = pk;
                }

                return Ok(w);
            }
        }

        // CAS B: Seed Binaire (32 octets) - Format historique
        if raw_data.len() == 32 {
            let mut seed = [0u8; 32];
            seed.copy_from_slice(&raw_data);
            let wallet = Wallet::from_seed(&seed, None)
                .map_err(|e| anyhow::anyhow!("Wallet::from_seed failed: {e}"))?;
            return Ok(wallet);
        }

        // CAS C: Erreur
        Err(anyhow::anyhow!(
            "Invalid key file format. Expected 64 hex chars (Private Key) or 32 raw bytes (Seed). Got size {}",
            raw_data.len()
        ))
    }

    pub fn x25519_pub_hex(&self) -> &str {
        &self.x25519_pub_hex
    }
}

/// Parse une adresse Bech32m et renvoie (hash20_hex, x25519_pub_hex)
pub fn decode_address(addr: &str) -> Result<(String, String), String> {
    let (hrp, data, variant) = decode(addr).map_err(|e| e.to_string())?;
    if variant != Variant::Bech32m {
        return Err("invalid bech32 variant".into());
    }
    // HRP optionnel: tu peux vérifier qu'il correspond à Wallet::bech32_hrp()
    let _ = hrp;

    let bytes: Vec<u8> = Vec::<u8>::from_base32(&data).map_err(|e| e.to_string())?;
    if bytes.len() != 52 {
        return Err(format!("invalid payload len {}, expected 52", bytes.len()));
    }
    let h20 = hex::encode(&bytes[..20]);
    let xpk = hex::encode(&bytes[20..]);
    Ok((h20, xpk))
}

pub fn make_address(hrp: &str, ecdsa_pub_hex: &str, x25519_pub_hex: &str) -> String {
    use bech32::{ToBase32, Variant, encode};
    use sha2::{Digest, Sha256};

    let pub_bytes = hex::decode(ecdsa_pub_hex).expect("pub hex");
    let hash = Sha256::digest(&pub_bytes); // 32 bytes
    let h20 = &hash[..20]; // prends 20 octets

    let xpk = hex::decode(x25519_pub_hex).expect("x25519 hex");

    let mut payload = Vec::with_capacity(52);
    payload.extend_from_slice(h20);
    payload.extend_from_slice(&xpk);

    encode(hrp, payload.to_base32(), Variant::Bech32m).expect("bech32m")
}

pub fn address_candidates(hrp: &str, ecdsa_pub_hex: &str, x25519_pub_hex: &str) -> Vec<String> {
    let mut out = Vec::new();

    // Normalise (enlève "0x", lowercase)
    let pk = ecdsa_pub_hex.trim_start_matches("0x").to_lowercase();
    let xpk = x25519_pub_hex.trim_start_matches("0x").to_lowercase();

    // Formes brutes possibles
    out.push(pk.clone());
    out.push(format!("0x{}", pk));
    if !xpk.is_empty() {
        out.push(xpk.clone());
        out.push(format!("0x{}", xpk));
    }

    // Bech32m = hash20(ECDSA pub) || x25519_pub
    if !pk.is_empty() && !xpk.is_empty() && hex::decode(&pk).is_ok() && hex::decode(&xpk).is_ok() {
        // make_address panique sur mauvais hex → on garde la garde au-dessus
        out.push(make_address(hrp, &pk, &xpk));
    }

    out
}
