use crate::PlainPayload;
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

pub const SCHEME_AES256GCM: &str = "x25519+aes256gcm";
/// Key-version 1 bound only `len_hint` into the AAD. Version 2 additionally
/// binds `scheme`, `key_version`, the shared ephemeral pubkey, and the sorted
/// set of recipient `kid`s so that tampering with any of those fields in a
/// stored or transported `EncryptedPayload` invalidates the AES-GCM tag.
/// Version 1 is still accepted on decryption for backward compatibility with
/// blocks produced before v0.7.2.
pub const KEY_VERSION_CURRENT: u32 = 2;
const BINDING_DOMAIN: &[u8] = b"pms-aead-binding-v1";

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
    pub len_hint: u32, // taille (ou padding) pour heuristiques
    /// Envelope binding for key-version ≥ 2: `hex(sha256(...))` over
    /// `scheme`, `key_version`, `ephem_pub`, and the recipients' sorted
    /// `kid`s. `None` when absent (key-version 1, old blocks).
    ///
    /// The `#[serde(skip_serializing_if)]` is what keeps v1 wire format
    /// byte-identical: `AAD { len_hint: 42, binding: None }` serialises to
    /// `{"len_hint":42}`, exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<String>,
}

/// Compute the envelope binding hex for key-version 2.
///
/// The binding covers every public field an attacker could rewrite without
/// the DEK: the ciphersuite, the key-version, the shared ephemeral pubkey,
/// and the sorted set of recipient kids. Because this hash is embedded in
/// the AES-GCM AAD of the body ciphertext, any post-hoc substitution of
/// those fields causes decryption to fail with an authentication error.
fn compute_binding(scheme: &str, key_version: u32, ephem_pub_hex: &str, kids: &[&str]) -> String {
    let mut sorted: Vec<&str> = kids.to_vec();
    sorted.sort_unstable();

    let mut h = Sha256::new();
    h.update(BINDING_DOMAIN);
    h.update([0u8]);
    h.update(scheme.as_bytes());
    h.update([0u8]);
    h.update(key_version.to_le_bytes());
    h.update([0u8]);
    h.update(ephem_pub_hex.as_bytes());
    h.update([0u8]);
    for kid in &sorted {
        h.update(kid.as_bytes());
        h.update([0u8]);
    }
    hex::encode(h.finalize())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KeyWrap {
    // ⚠️ plus de recipient_pub : identifiant opaque
    pub kid: String,             // 16 bytes dérivés du shared, hex
    pub ephem_pub: String,       // pk éphémère (hex)
    pub wrapped_key_b64: String, // DEK chiffrée via KEK(shared)
    pub kw_nonce_b64: String,    // nonce GCM du wrap
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

        // 2) Clé éphémère unique pour tous les wraps (per‑message)
        let mut eph_sk_bytes = [0u8; 32];
        rng.fill_bytes(&mut eph_sk_bytes);
        let eph_sk = StaticSecret::from(eph_sk_bytes);
        let eph_pk = PublicKey::from(&eph_sk);
        let eph_pk_hex = hex::encode(eph_pk.to_bytes());

        // 3) Enveloppe DEK pour chaque destinataire via X25519 + HKDF -> KEK,
        //    puis AES-GCM. This has to run BEFORE the body encryption because
        //    the recipients' kids feed the envelope binding that the AES-GCM
        //    AAD on the body ciphertext commits to.
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
            hk.expand(b"kek-v1", &mut kek)
                .map_err(|_| "hkdf expand kek")?;

            // NEW: kid (opaque, 16 bytes)
            let mut kid16 = [0u8; 16];
            hk.expand(b"kid-v1", &mut kid16)
                .map_err(|_| "hkdf expand kid")?;
            let kid = hex::encode(kid16);

            // Wrap DEK avec AAD = kid (pas la pubkey)
            let mut kw_nonce = [0u8; 12];
            rng.fill_bytes(&mut kw_nonce);
            let kw_cipher = Aes256Gcm::new_from_slice(&kek).map_err(|e| e.to_string())?;
            let wrapped = kw_cipher
                .encrypt(
                    Nonce::from_slice(&kw_nonce),
                    Payload {
                        msg: &dek,
                        aad: kid.as_bytes(),
                    },
                )
                .map_err(|e| format!("wrap enc: {e}"))?;

            recipients.push(KeyWrap {
                kid,
                ephem_pub: eph_pk_hex.clone(),
                wrapped_key_b64: b64(&wrapped),
                kw_nonce_b64: b64(&kw_nonce),
            });

            kek.zeroize();
        }

        // 4) Envelope binding — hash every public field an attacker could
        //    substitute without the DEK (scheme, key_version, ephem_pub, the
        //    sorted set of kids). Embedded in the AAD of the body ciphertext
        //    so AES-GCM's tag turns any post-hoc edit into a decrypt failure.
        let kid_refs: Vec<&str> = recipients.iter().map(|r| r.kid.as_str()).collect();
        let binding = compute_binding(
            SCHEME_AES256GCM,
            KEY_VERSION_CURRENT,
            &eph_pk_hex,
            &kid_refs,
        );

        // 5) Chiffre le payload avec l'AAD étendue
        let aad_struct = AAD {
            len_hint,
            binding: Some(binding),
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

        // 6) Assemble l’enveloppe
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

        // Envelope-binding check for key-version ≥ 2. Recomputing the hash
        // from the envelope we received catches most tampering cases with a
        // crisp error; the AES-GCM tag on the body ciphertext is still the
        // ultimate authority — we rely on it for any field the binding
        // doesn't cover. v1 blocks (`binding = None`) skip this step for
        // backward compatibility.
        if let Some(ref wire_binding) = self.aad.binding {
            let Some(first) = self.recipients.first() else {
                return Err("no recipients in v2 envelope".into());
            };
            // v2 requires every recipient to share the same ephem_pub, which
            // is enforced at encrypt time. Reject mismatched envelopes
            // before we burn a decrypt on a mangled payload.
            for r in &self.recipients {
                if r.ephem_pub != first.ephem_pub {
                    return Err("recipients have mismatched ephem_pub".into());
                }
            }
            let kid_refs: Vec<&str> = self.recipients.iter().map(|r| r.kid.as_str()).collect();
            let expected =
                compute_binding(&self.scheme, self.key_version, &first.ephem_pub, &kid_refs);
            if &expected != wire_binding {
                return Err("envelope binding mismatch — recipients or ephem_pub tampered with".into());
            }
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
            hk.expand(b"kek-v1", &mut kek)
                .map_err(|_| "hkdf expand kek")?;

            let mut kid16 = [0u8; 16];
            hk.expand(b"kid-v1", &mut kid16)
                .map_err(|_| "hkdf expand kid")?;
            let expect_kid = hex::encode(kid16);

            // si le kid ne matche pas, continue
            if expect_kid != w.kid {
                kek.zeroize();
                continue;
            }

            let kw_nonce = b64dec(&w.kw_nonce_b64)?;
            let wrapped = b64dec(&w.wrapped_key_b64)?;
            let kw_cipher = Aes256Gcm::new_from_slice(&kek).map_err(|e| e.to_string())?;

            if let Ok(d) = kw_cipher.decrypt(
                Nonce::from_slice(&kw_nonce),
                Payload {
                    msg: &wrapped,
                    aad: w.kid.as_bytes(),
                },
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
    pub fn decrypt_plain_with(&self, recipient_sk_hex: &str) -> Result<PlainPayload, String> {
        let pt = self.decrypt_with(recipient_sk_hex)?;
        serde_json::from_slice(&pt).map_err(|e| e.to_string())
    }

    // Déchiffre et désérialise directement en `PlainPayload`.
    pub fn decrypt_as_payload(&self, recipient_sk_hex: &str) -> Result<PlainPayload, String> {
        let pt = self.decrypt_with(recipient_sk_hex)?;
        let payload: PlainPayload =
            serde_json::from_slice(&pt).map_err(|e| format!("serde decode: {e}"))?;
        Ok(payload)
    }
}
