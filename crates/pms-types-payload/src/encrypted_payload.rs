use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine;
use base64::engine::general_purpose;
use hkdf::Hkdf;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;
use crate::PlainPayload;

pub const SCHEME_AES256GCM: &str = "x25519+aes256gcm";
pub const KEY_VERSION_CURRENT: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncryptedPayload {
    pub scheme: String,           // "x25519+aes256gcm"
    pub key_version: u32,         // rotation de clé
    pub aad: AAD,                 // métadonnée publique authentifiée
    pub commitment: String,       // hex(sha256(plaintext))
    pub ciphertext_b64: String,   // AES-256-GCM(ct)
    pub recipients: Vec<KeyWrap>, // DEK enveloppée par destinataire
    pub nonce_b64: String,        // nonce 12 bytes pour AES-GCM
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AAD {
    pub len_hint: u32,        // taille (ou padding) pour heuristiques
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyWrap {
    // ⚠️ plus de recipient_pub : identifiant opaque
    pub kid: String,              // 16 bytes dérivés du shared, hex
    pub ephem_pub: String,        // pk éphémère (hex)
    pub wrapped_key_b64: String,  // DEK chiffrée via KEK(shared)
    pub kw_nonce_b64: String,     // nonce GCM du wrap
}

fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

fn hex_to_32(bytes_hex: &str) -> Result<[u8; 32], String> {
    let v = hex::decode(bytes_hex).map_err(|e| format!("hex decode: {e}"))?;
    if v.len() != 32 {
        return Err("expected 32 bytes".into());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&v);
    Ok(out)
}

fn b64(enc: &[u8]) -> String {
    general_purpose::STANDARD.encode(enc)
}

fn b64dec(s: &str) -> Result<Vec<u8>, String> {
    general_purpose::STANDARD
        .decode(s)
        .map_err(|e| e.to_string())
}

impl EncryptedPayload {
    /// Chiffre `plaintext` pour `recipients_pks_hex` (liste de pk X25519 hex).
    pub fn encrypt_for(
        plaintext: &[u8],
        recipients_pks_hex: &[String],
        len_hint: u32,
    ) -> Result<Self, String> {
        // 1) Génère DEK et nonce pour AES-GCM (12 bytes)
        let mut rng = rand::rng();
        let mut dek = [0u8; 32];
        rng.fill_bytes(&mut dek);
        let mut nonce = [0u8; 12];
        rng.fill_bytes(&mut nonce);

        // 2) Chiffre le payload
        let aad_struct = AAD {
            len_hint,
        };
        let aad_bytes = serde_json::to_vec(&aad_struct).map_err(|e| e.to_string())?;
        let cipher = Aes256Gcm::new_from_slice(&dek).map_err(|e| e.to_string())?;
        let ct = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad_bytes,
                },
            )
            .map_err(|e| format!("aes-gcm enc: {e}"))?;
        let commitment = sha256_hex(plaintext);

        // 3) Clé éphémère unique pour tous les wraps (per‑message)
        let mut eph_sk_bytes = [0u8; 32];
        rng.fill_bytes(&mut eph_sk_bytes);
        let eph_sk = StaticSecret::from(eph_sk_bytes);
        let eph_pk = PublicKey::from(&eph_sk);
        let eph_pk_hex = hex::encode(eph_pk.to_bytes());

        // 4) Enveloppe DEK pour chaque destinataire via X25519 + HKDF -> KEK, puis AES-GCM
        let mut recipients = Vec::with_capacity(recipients_pks_hex.len());
        for pk_hex in recipients_pks_hex {
            let recip_pk_bytes = hex_to_32(pk_hex)?;
            let recip_pk = PublicKey::from(recip_pk_bytes);

            // ECDH
            let shared = eph_sk.diffie_hellman(&recip_pk);

            // HKDF-SHA256(shared, "pms-dek-wrap")
            let hk = Hkdf::<Sha256>::new(Some(b"pms-dek-wrap"), shared.as_bytes());

            // KEK pour enrober la DEK
            let mut kek = [0u8; 32];
            hk.expand(b"kek-v1", &mut kek).map_err(|_| "hkdf expand kek")?;

            // NEW: kid (opaque, 16 bytes)
            let mut kid16 = [0u8; 16];
            hk.expand(b"kid-v1", &mut kid16).map_err(|_| "hkdf expand kid")?;
            let kid = hex::encode(kid16);

            // Wrap DEK avec AAD = kid (pas la pubkey)
            let mut kw_nonce = [0u8; 12];
            rng.fill_bytes(&mut kw_nonce);
            let kw_cipher = Aes256Gcm::new_from_slice(&kek).map_err(|e| e.to_string())?;
            let wrapped = kw_cipher.encrypt(
                Nonce::from_slice(&kw_nonce),
                Payload { msg: &dek, aad: kid.as_bytes() },
            ).map_err(|e| format!("wrap enc: {e}"))?;

            recipients.push(KeyWrap {
                kid,
                ephem_pub: eph_pk_hex.clone(),
                wrapped_key_b64: b64(&wrapped),
                kw_nonce_b64: b64(&kw_nonce),
            });

            kek.zeroize();
        }

        // 5) Assemble l’enveloppe
        let env = EncryptedPayload {
            scheme: SCHEME_AES256GCM.to_string(),
            key_version: KEY_VERSION_CURRENT,
            aad: aad_struct,
            commitment,
            ciphertext_b64: b64(&ct),
            recipients,
            nonce_b64: b64(&nonce),
        };

        // hygiène
        dek.zeroize();
        eph_sk_bytes.zeroize();

        Ok(env)
    }

    /// Déchiffre avec la **clé privée X25519** (hex) du destinataire.
    pub fn decrypt_with(&self, recipient_sk_hex: &str) -> Result<Vec<u8>, String> {
        if self.scheme.as_str() != SCHEME_AES256GCM {
            return Err("unsupported scheme".into());
        }
        // AAD
        let aad_bytes = serde_json::to_vec(&self.aad).map_err(|e| e.to_string())?;
        let nonce = b64dec(&self.nonce_b64)?;

        // 1) Retrouve le wrap qui me concerne (par compat, on teste tous)
        let sk_bytes = hex_to_32(recipient_sk_hex)?;
        let sk = StaticSecret::from(sk_bytes);
        let mut dek = None;

        for w in &self.recipients {
            // ECDH avec la clé éphémère de l’expéditeur
            let epk_bytes = hex_to_32(&w.ephem_pub)?;
            let epk = PublicKey::from(epk_bytes);
            let shared = sk.diffie_hellman(&epk);

            // HKDF pour KEK + kid attendu
            let hk = Hkdf::<Sha256>::new(Some(b"pms-dek-wrap"), shared.as_bytes());
            let mut kek = [0u8; 32];
            hk.expand(b"kek-v1", &mut kek).map_err(|_| "hkdf expand kek")?;

            let mut kid16 = [0u8; 16];
            hk.expand(b"kid-v1", &mut kid16).map_err(|_| "hkdf expand kid")?;
            let expect_kid = hex::encode(kid16);

            // si le kid ne matche pas, continue
            if expect_kid != w.kid { kek.zeroize(); continue; }

            let kw_nonce = b64dec(&w.kw_nonce_b64)?;
            let wrapped  = b64dec(&w.wrapped_key_b64)?;
            let kw_cipher = Aes256Gcm::new_from_slice(&kek).map_err(|e| e.to_string())?;

            if let Ok(d) = kw_cipher.decrypt(
                Nonce::from_slice(&kw_nonce),
                Payload { msg: &wrapped, aad: w.kid.as_bytes() },
            ) {
                dek = Some(d);
                kek.zeroize();
                break;
            }
            kek.zeroize();
        }

        let dek = dek.ok_or_else(|| "no matching recipient / unwrap failed".to_string())?;

        // 2) Déchiffre le corps
        let cipher = Aes256Gcm::new_from_slice(&dek).map_err(|e| e.to_string())?;
        let ct = b64dec(&self.ciphertext_b64)?;
        let pt = cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &ct,
                    aad: &aad_bytes,
                },
            )
            .map_err(|e| format!("aes-gcm dec: {e}"))?;

        // 3) Vérifie l’engagement
        let got = sha256_hex(&pt);
        if got != self.commitment {
            return Err("commitment mismatch".into());
        }

        Ok(pt)
    }

    /// Chiffre directement un `PlainPayload` pour une liste de destinataires (pk hex).
    pub fn encrypt_for_plain(
        payload: &PlainPayload,
        recipients_pks_hex: &[String],
    ) -> Result<Self, String> {
        // sérialise le payload clair
        let pt = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
        // encrypt_for attend un &[u8]
        Self::encrypt_for(&pt, recipients_pks_hex, pt.len() as u32)
    }

    /// Déchiffre et retourne directement un `PlainPayload`.
    pub fn decrypt_plain_with(
        &self,
        recipient_sk_hex: &str,
    ) -> Result<PlainPayload, String> {
        let pt = self.decrypt_with(recipient_sk_hex)?;
        serde_json::from_slice(&pt).map_err(|e| e.to_string())
    }

    // Déchiffre et désérialise directement en `PlainPayload`.
    pub fn decrypt_as_payload(&self, recipient_sk_hex: &str) -> Result<PlainPayload, String> {
        let pt = self.decrypt_with(recipient_sk_hex)?;
        let payload: PlainPayload = serde_json::from_slice(&pt)
            .map_err(|e| format!("serde decode: {e}"))?;
        Ok(payload)
    }
}
