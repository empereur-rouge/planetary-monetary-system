use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use hex;
use k256::ecdsa::{signature::Verifier, Signature, VerifyingKey};
use sha2::{Digest, Sha256};
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::{WireBlock};

/// Vérifie la signature ECDSA secp256k1 d’un `WireBlock`.
///
/// Ce que ça garantit:
/// --------------------
/// 1) **Authenticité** : le bloc a été signé par le détenteur de la clé privée correspondante
///    à `signer_pk_hex`. Personne d’autre ne pouvait produire cette signature.
/// 2) **Intégrité** : aucune donnée utilisée pour calculer la signature (parents, payload, nonce,
///    network_id, protocol_version, etc.) n’a été modifiée en transit.
/// 3) **Non-répudiation** : le nœud qui signe ne peut pas nier avoir signé ce bloc.
///
///
/// Le principe général:
/// --------------------
/// - Lorsque tu mines un bloc, ton wallet calcule un message canonique (!)
///   `canonical_wireblock_message(wb)`
///   → c’est une string générée toujours de la même manière, sans ambiguïté.
///
/// - Ensuite le wallet calcule `signature = Sign(privkey, SHA256(message))`.
///
/// - Lorsqu’un autre nœud reçoit le bloc, il :
///
///   1) **reconstruit exactement le même message** à partir du bloc reçu.
///   2) **re-hash** ce message.
///   3) **vérifie** que `Verify(pubkey, signature, hash(message)) == OK`.
///
/// Si ça échoue → le bloc est fraudé / corrompu / falsifié.
///
///
/// Hypothèses techniques:
/// -----------------------
/// - `signer_pk_hex` : clé publique en hex, format SEC1 (33 ou 65 bytes)
///   → c’est le format standard des clés publiques secp256k1.
/// - `signature_hex` : signature encodée en Base64 (retournée par ton wallet).
/// - Le schéma utilisé est **ECDSA secp256k1**, le même que Bitcoin.
/// - Le message est hashé avec SHA-256 avant vérification (standard avec ECDSA).
///
///
/// En résumé non-technique :
/*
    Le processus revient à dire:

    “Est-ce que ce bloc a réellement été signé par le propriétaire de la clé publique indiquée,
     et est-ce que rien dans le bloc n’a été modifié après la signature ?”

    Si oui → bloc accepté.
    Sinon → bloc rejeté.
*/
///
/// Retourne Ok(()) si tout est bon, ou Err(...) si la signature est invalide.
pub fn verify_block_signature(wb: &WireBlock) -> Result<()> {
    // --- 1) Clé publique: hex -> bytes ------------------------------------
    //
    // signer_pk_hex est une string hexadécimale (ex: "02ab...").
    // On la convertit en tableau de bytes bruts pour la librairie k256.
    let pk_bytes = hex::decode(&wb.signer_pk_hex)
        .context("invalid signer_pk_hex (not valid hex)")?;

    // --- 2) Construire la clé publique secp256k1 ---------------------------
    //
    // La clé est au format SEC1 (33 ou 65 octets). Si ce n’est pas le cas,
    // k256 renverra une erreur.
    let vk = VerifyingKey::from_sec1_bytes(&pk_bytes)
        .context("invalid secp256k1 public key (SEC1 format)")?;

    // --- 3) Signature: base64 -> bytes --------------------------------------
    //
    // signature_hex est ici une base64 (ce que renvoie `Wallet::sign`).
    let sig_bytes = B64
        .decode(&wb.signature_hex)
        .context("invalid base64 signature")?;

    // --- 4) Interpréter la signature (DER ou raw 64 bytes) -----------------
    //
    // Selon comment le wallet encode la signature:
    //   - soit c’est une signature DER (format ASN.1),
    //   - soit c’est un tableau brut de 64 octets (r || s).
    //
    // On essaie d'abord DER (format classique), puis raw 64 bytes (r||s).
    let sig = Signature::from_der(&sig_bytes)
        .or_else(|_| {
            if sig_bytes.len() != 64 {
                anyhow::bail!("signature must be 64 bytes for raw ECDSA form");
            }
            let mut arr = [0u8; 64];
            arr.copy_from_slice(&sig_bytes);
            Ok(Signature::from_bytes((&arr).into())?)
        })
        .context("invalid ECDSA signature format")?;

    // --- 5) Message canonique signé ----------------------------------------
    //
    // C’est **exactement** ce que le wallet doit signer.
    // Important: on ne bidouille pas ce buffer derrière, sinon la signature
    // ne correspond plus.
    let msg = canonical_wireblock_message(wb);

    // --- 6) Vérification ----------------------------------------------------
    //
    // ATTENTION: `verify` va lui-même:
    //   - appliquer SHA-256 sur `msg`,
    //   - vérifier la signature sur ce hash.
    //
    // Donc on lui passe **directement `msg`**, sans re-hasher à la main.
    vk.verify(msg.as_bytes(), &sig)
        .context("signature verification failed")?;

    Ok(())
}