//! Messages “sur le fil” (wire format) — JSON ligne par ligne.
//! MVP: on diffuse directement le bloc complet (pas de INV/GET).
//! Pourquoi ? Moins d’allers-retours, plus simple à intégrer au début.

use serde::{Serialize, Deserialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum NetMsg {
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
    },
}
