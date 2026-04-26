//! API endpoint pour les informations du Coordinateur.
//!
//! Permet aux clients de récupérer les clés publiques du nœud
//! pour vérifier les signatures et chiffrer des données.

use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;

use crate::api::AppState;
use pms_wallet::SignerBackend;

/// One coordinator-side shard, exposed for audit. Each entry is fully
/// self-contained: an external observer with the master pubkey CAN'T
/// derive the private key (HKDF needs the secret), but they CAN sum the
/// per-shard balances via `GET /v1/balance/{address}` to verify the
/// cumulative coordinator holdings.
#[derive(Debug, Serialize)]
pub struct CoordinatorShard {
    /// Round-robin index assigned at derivation time (0..N).
    pub index: u32,
    /// secp256k1 pubkey of this shard (hex). Derived from the master
    /// pk via HKDF-SHA256("pms/coord-shard-salt/v1", "pms/coord-shard/v1/" + idx).
    pub secp256k1_pubkey: String,
    /// X25519 pubkey of this shard (hex). Derived inside the shard's
    /// own keypair (not from the master's X25519). For receive-only
    /// fee output addresses, only the bech32 address matters.
    pub x25519_pubkey: String,
    /// Bech32 address — what users see in fee outputs when sharding
    /// is enabled.
    pub address: String,
}

/// Réponse pour GET /v1/coordinator/info
#[derive(Debug, Serialize)]
pub struct CoordinatorInfoResponse {
    /// Ce nœud est-il le Coordinateur ?
    pub is_coordinator: bool,
    /// Clé publique secp256k1 (hex) - pour vérifier les signatures
    pub secp256k1_pubkey: String,
    /// Clé publique X25519 (hex) - pour le chiffrement
    pub x25519_pubkey: String,
    /// Préfixe d'adresse du ledger (ex: "pms")
    pub address_prefix: String,
    /// Number of coordinator sub-address shards in use. `0` = sharding
    /// disabled (legacy single-address fee routing). When > 0, fee
    /// outputs are round-robin'd across `shards`.
    pub coord_shard_count: u32,
    /// Public list of every coordinator shard. Empty when sharding is
    /// disabled. Auditors can sum `GET /v1/balance/{shards[i].address}`
    /// across this list to verify the total coordinator balance —
    /// trust model doc has the procedure. Audit follow-up to v0.7.4.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shards: Vec<CoordinatorShard>,
}

/// GET /v1/coordinator/info
///
/// Retourne les clés publiques du nœud actuel.
/// Utile pour:
/// - Vérifier si ce nœud est le Coordinateur
/// - Récupérer la clé secp256k1 pour vérifier les signatures
/// - Récupérer la clé X25519 pour chiffrer des données destinées au Coordinator
pub async fn get_coordinator_info(
    State(st): State<AppState>,
) -> (StatusCode, Json<CoordinatorInfoResponse>) {
    let settings = st.settings.as_ref();
    let node_wallet = &st.node_wallet;

    // Vérifie si ce nœud est le Coordinateur
    let is_coordinator = if let Some(coord_pk) = &settings.validation.coordinator_public_key {
        node_wallet.encoded_public_key() == *coord_pk
    } else {
        true // Dev mode: pas de coordinateur défini
    };

    // Retourne les clés du coordinateur définies dans settings,
    // ou celles du nœud local en fallback (Dev mode sans config explicite)
    let (secp256k1, x25519) = match (
        &settings.validation.coordinator_public_key,
        &settings.validation.coordinator_x25519_public_key,
    ) {
        (Some(secp), Some(x25519)) => (secp.clone(), x25519.clone()),
        _ => (
            node_wallet.encoded_public_key(),
            node_wallet.x25519_pub_hex().to_string(),
        ),
    };

    // Build the shard list for audit. Empty when sharding is disabled,
    // populated from the boot-time derivation (Phase 2).
    let hrp = &settings.address.hrp;
    let shards: Vec<CoordinatorShard> = st
        .coord_shard_wallets
        .iter()
        .enumerate()
        .map(|(idx, w)| CoordinatorShard {
            index: idx as u32,
            secp256k1_pubkey: w.encoded_public_key(),
            x25519_pubkey: w.x25519_pub_hex().to_string(),
            address: w.get_address(hrp),
        })
        .collect();
    let coord_shard_count = shards.len() as u32;

    let response = CoordinatorInfoResponse {
        is_coordinator,
        secp256k1_pubkey: secp256k1,
        x25519_pubkey: x25519,
        address_prefix: settings.rocks.prefix.clone(),
        coord_shard_count,
        shards,
    };

    (StatusCode::OK, Json(response))
}
