//! Messages “sur le fil” (wire format) — JSON ligne par ligne.
//! MVP: on diffuse directement le bloc complet (pas de INV/GET).
//! Pourquoi ? Moins d’allers-retours, plus simple à intégrer au début.

use serde::{Serialize, Deserialize};
use pms_wire::WireBlock;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum NetMsg {
    // Handshake
    Hello {
        proto: u16,          // version protocole
        node_id: String,     // ident peer (aléatoire au boot)
        nonce: u64,          // anti-rejeu sur connexion
        ping_ms: u32,        // intervalle ping
    },
    HelloAck {
        ok: bool,
        reason: Option<String>,
    },

    /// Ping/Pong pour santé de connexion (facultatif en MVP).
    Ping,
    Pong,

    /// Diffusion d’un bloc complet (forme persistable).
    /// On évite d’exposer des types internes du core ici.
    Block {
        id: String,
        parents: Vec<String>,
        payload_json: Option<String>,
        nonce: u64,
        network_id: String,
        protocol_version: u16,
        signer_pk_hex: String,
        signature_hex: String,
    },
    // --- rattrapage / sync ciblé ---
    /// Demande les tips connues du pair (bornées)
    GetTips { limit: usize },
    /// Réponse avec une liste d’ids (pas de payload)
    Tips { ids: Vec<String> },

    /// Annonce légère d’un inventaire de blocs (ids)
    Inv { ids: Vec<String> },

    /// Demande le bloc complet par id
    GetBlock { id: String },
    /// Réponse avec un lot (borné) de blocs complets
    Blocks { blocks: Vec<WireBlock> },
}
