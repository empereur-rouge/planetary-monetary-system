//! Endpoint pour les informations de version du noeud.

use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};

use crate::api::AppState;

/// Version de l'API REST — à incrémenter à chaque modification des routes/formats.
/// v25 (v0.20.0) : `POST /admin/sft/classes` accepte `demurrage_bps_per_day` (≤10000) —
/// une classe SFT peut décoter ses UTXO comme un token (protocole 2.5).
/// v24 (v0.19.0) : semi-fongibles (SFT) — `POST /admin/sft/{classes,mint}` (admin)
/// + `GET /v1/sft/{classes,classes/{asset_id},collections/{collection}}` (public).
/// Une classe = asset fongible `"collection:class"` ; mint contraint par `max_supply`.
/// v23 (v0.18.1) : `POST /admin/governance/propose` accepte désormais le `tier` en
/// lowercase (`"operator"`) EN PLUS du PascalCase (`"Operator"`) — symétrie avec les
/// réponses publiques qui renvoient le palier en lowercase. Rétro-compatible.
/// v22 (v0.18.0) : nouvel endpoint **public** `GET /v1/governance/blocks` (journal
/// d'audit de TOUS les blocs DAG de gouvernance : proposal/enact/cancel avec leurs
/// block_ids). `/v1/governance/{pending,history}` exposent désormais aussi les
/// `proposal_block_id` / `enact_block_id` / `cancel_block_id` du cycle.
/// v21 (v0.17.0) : gouvernance P3 — nouvelle variante `ConfigUpdate::SetEmissionCorridor`
/// (couloir d'émission gouverné, palier Constitution) acceptée par `POST /admin/config`
/// et `POST /admin/governance/propose`. Le couloir (ceiling/floor/target/epoch) est
/// désormais modifiable par gouvernance timelock au lieu d'être boot-only.
/// v20 (v0.16.0) : gouvernance P2 — `POST /admin/governance/propose` applique
/// désormais le palier MINIMUM par paramètre (rejet `3071` si trop bas) et
/// l'asymétrie tighten/loosen (un resserrage a `enact_after == announced_at`,
/// timelock instantané ; un desserrage garde le délai plein du palier). Nouveau
/// code `5031` (MintDisabled) : le kill-switch `mint_enabled=false` fait échouer
/// les chemins de mint natif (on-ramp, conversion token→PMS, faucet) en 503.
/// `POST /admin/config` ne s'applique PLUS instantanément (P2c) : il forge un
/// `GovernanceProposal` (palier auto-assigné), appliqué immédiatement si
/// resserrage (200 `applied`), sinon proposition timelockée (202 `proposed`).
/// v19 (v0.15.0) : routes gouvernance timelock (plan §4) — `POST /admin/governance/{propose,enact,cancel}`
/// + `GET /v1/governance/{pending,history}` (public).
/// v18 (v0.14.0) : nouvelle route `POST /v1/wallet/token/burn` (burn de token
/// owner-signé, plan §3.1 voie B) + nouveau `PlainPayload::TokenBurn`
/// (DAG_VERSION 3.3.0).
/// v17 (v0.13.0) : nouvelle route `POST /admin/onramp` (voie A fiat→PMS, mint
/// natif sous budget d'émission partagé, plan §3.1) ; nouveau code d'erreur
/// `5030` (EmissionBudgetExhausted) sur les chemins de mint gatés.
/// v14 (audit H-5, v0.9.2) : les endpoints de restauration de wallet passent de
/// `/v1/wallet/restore/{mnemonic,private-key}` (API-key) à
/// `/admin/wallet/restore/{mnemonic,private-key}` (admin-gated). L'ancien
/// chemin renvoie 404.
/// v13 (audit sécurité v0.9.0) : `POST /v1/wallet/tx/send` exige des unlocks
/// valides (401 sinon) et la conservation par asset ; `POST /submit/block`
/// rejette les tx sans autorisation de dépense et les block ids non canoniques.
/// v26 (audit sécurité 2026-06, cause A) : `POST /wallet/tx/send` et
/// `POST /v1/wallet/send-simple` valident désormais le plaintext via la MÊME
/// fonction que le hot-path (gel compliance, time-locks, MultiSig/HashLock,
/// dédup d'inputs anti-inflation, conservation) — une tx invalide est rejetée
/// (400) au lieu d'être emballée chiffrée et appliquée.
/// v27 (audit sécurité 2026-06, cause A-bis) : `POST /submit/block`,
/// `POST /wallet/tx/send` et `POST /v1/wallet/send-simple` rejettent de façon
/// fiable les **double-dépenses concurrentes** (claim atomique des inputs avant
/// application du delta) — ferme un TOCTOU validate→apply.
/// v28 (audit rang 2 — frais brûlés à la source) : `/v1/tx/prepare` ne renvoie
/// plus d'output de frais ; `/wallet/tx/send` & `/v1/wallet/send-simple` brûlent
/// le frais de gas (`in − out`) au lieu de l'envoyer au coordinateur. Le frais
/// implicite doit couvrir le minimum (sinon `insufficient fees`).
/// v29 (audit rang 3 — B2) : l'enregistrement de contrat (`/admin/contracts`)
/// rejette une action `AccumulateRefund` en PMS natif (`asset_id=None`) —
/// l'émission native ne passe jamais par un refund de contrat (anti mint illimité).
/// v30 (audit rang 3 — B3) : `POST /submit/block` rejette un `BridgeMint` qui
/// rejoue un `lock_block_id` déjà minté (anti-replay durable, CF `bridge_consumed`)
/// — ferme une inflation par re-soumission de bridge mint.
/// v31 (audit rang 3 — B3 réconciliation cross-ledger) : `POST /submit/block`
/// réconcilie un `BridgeMint` avec son `BridgeLock` source (montant == verrouillé,
/// asset, destinataire, ledger destination) — rejette sur-émission, vol,
/// lock inexistant, ou mauvais ledger destination.
/// v32 (protocole 2.7 — marketplace) : nouvel endpoint `POST /v1/market/settle`
/// (règlement atomique vente/revente + royalty enforced consensus) ; champs
/// `royalty_bps`/`royalty_beneficiary` ajoutés à `POST /admin/tokens/create` et
/// `POST /admin/sft/classes`.
/// v33 (protocole 2.7 — royalty mutable) : nouvel endpoint `POST /admin/royalty`
/// (redirige/modifie le bénéficiaire ou le taux de royalty d'un asset existant).
/// v34 (protocole 2.7 — royalty co-signée) : `/admin/royalty` remplacé par
/// `POST /v1/royalty/prepare` + `POST /v1/royalty/update` (API-key, PAS admin) —
/// autorisés par la SIGNATURE du bénéficiaire courant (custodial ou pré-signée).
/// v35 (revue marketplace — D1) : `POST /v1/market/settle` et
/// `POST /v1/royalty/{prepare,update}` renvoient désormais des erreurs au format
/// `ApiError` à **code numérique stable** (`{"code":NNNN,"message":...}`) au lieu
/// de `{"error":"..."}` ad hoc. Nouveau code `1050` (Forbidden — clé fournie ≠
/// bénéficiaire courant, 403). Un rejet consensus (royalty/settlement) mappe sur
/// `3070` (Conflict, 409).
/// v36 (v0.30.0 — fee distribution) : `POST /admin/distribute_fees` et la tâche
/// périodique créditent désormais la **part producteur du coordinateur**
/// (`coordinator_fee_percent`, 65 % après la coupe treasury) au wallet du
/// coordinateur au lieu de la rediriger vers la treasury. Format de requête/réponse
/// inchangé — seul le comportement de routage des fonds change.
/// v37 (v0.30.1 — durcissement sécurité) : `POST /v1/nft/mint` est create-only
/// (409 si le token existe) ET exige une **autorité d'émission** sur le chemin
/// API-key (`creator_pubkey_hex` + `creator_signature_b64` d'un coordinateur /
/// admin-signer / owner de ledger) — anti vol de NFT par re-mint + anti farming
/// de contrat burn-refund. `/v1/register`, `/v1/heartbeat`, `/v1/peers/connect`
/// passent derrière `require_local_or_admin`. `/internal/*` retiré du routeur
/// public. `require_api_key` fail-closed en mainnet si aucune clé n'est provisionnée.
/// v38 (v0.30.1 — auth crypto node registry) : `POST /v1/register` et
/// `/v1/heartbeat` acceptent une **preuve de possession de `node_pk`** (champs
/// `ts_ms` + `signature_b64` sur `node_register_signing_message`) en plus du token
/// admin — un pair peut s'auto-inscrire sans le token opérateur. Anti-rejeu
/// (fraîcheur ±5 min + monotonie sur `ts_ms`) + cap anti-DoS. `peers/connect`
/// reste opérateur-only.
pub const API_VERSION: u32 = 38;

/// Réponse pour GET /v1/version
#[derive(Debug, Serialize, Deserialize)]
pub struct VersionResponse {
    /// Version du logiciel (Cargo.toml)
    pub software_version: String,
    /// Version du protocole DAG (SemVer, stockée dans RocksDB)
    pub dag_version: String,
    /// Version du schéma RocksDB (entier)
    pub schema_version: i64,
    /// Version du protocole P2P
    pub protocol_version: u32,
    /// Version de l'API REST (entier)
    pub api_version: u32,
}

/// GET /v1/version
///
/// Retourne toutes les informations de version du noeud.
pub async fn get_version(
    State(st): State<AppState>,
) -> (StatusCode, Json<VersionResponse>) {
    let software_version = env!("CARGO_PKG_VERSION").to_string();
    let dag_version = st
        .store
        .get_dag_version()
        .await
        .unwrap_or_else(|_| "1.0.0".to_string());
    let schema_version = st.store.get_version().await.unwrap_or(0);
    let protocol_version = st.settings.network.protocol_version;

    (
        StatusCode::OK,
        Json(VersionResponse {
            software_version,
            dag_version,
            schema_version,
            protocol_version,
            api_version: API_VERSION,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_response_serialization() {
        let response = VersionResponse {
            software_version: "0.1.3".to_string(),
            dag_version: "1.0.0".to_string(),
            schema_version: 5,
            protocol_version: 1,
            api_version: API_VERSION,
        };

        let json = serde_json::to_string_pretty(&response).unwrap();
        println!("Version API response JSON:\n{json}");

        let parsed: VersionResponse = serde_json::from_str(&json).unwrap();
        println!("Parsed back: software={}, dag={}, schema={}, protocol={}, api={}",
            parsed.software_version, parsed.dag_version,
            parsed.schema_version, parsed.protocol_version, parsed.api_version);

        assert_eq!(parsed.software_version, "0.1.3");
        assert_eq!(parsed.dag_version, "1.0.0");
        assert_eq!(parsed.schema_version, 5);
        assert_eq!(parsed.protocol_version, 1);
        // Pin the LITERAL (not `API_VERSION` vs itself): any bump of API_VERSION
        // must consciously update this assertion + the CHANGELOG. The real
        // GET /v1/version handler is exercised in tests/version_endpoint.rs.
        // v0.11.0: 15 → 16 (faucet locked_until + champs collateral_* sur
        // /admin/tokens/create — mint collatéralisé 2.3 v2).
        // v0.13.0: 16 → 17 (POST /admin/onramp voie A + code 5030).
        // v0.14.0: 17 → 18 (POST /v1/wallet/token/burn + PlainPayload::TokenBurn).
        // v0.15.0: 18 → 19 (gouvernance timelock routes).
        // v0.16.0: 19 → 20 (gouvernance P2 : palier-min + asymétrie sur propose).
        // v0.17.0: 20 → 21 (gouvernance P3 : ConfigUpdate::SetEmissionCorridor).
        // v0.18.0: 21 → 22 (GET /v1/governance/blocks + block_ids dans pending/history).
        // v0.18.1: 22 → 23 (propose accepte tier lowercase).
        // v0.19.0: 23 → 24 (semi-fongibles SFT : routes /admin/sft + /v1/sft).
        // v0.20.0: 24 → 25 (SFT demurrage : champ demurrage_bps_per_day sur create).
        // v0.22.0: 25 → 26 (audit cause A : /wallet/tx/send & /v1/wallet/send-simple
        //          valident le plaintext — gel/time-lock/MultiSig/dédup/conservation).
        // v0.23.0: 26 → 27 (audit cause A-bis : rejet fiable des double-dépenses
        //          concurrentes — claim atomique avant apply, ferme un TOCTOU).
        // v0.24.0: 27 → 28 (audit rang 2 : frais brûlés à la source — prepare sans
        //          output de frais, /wallet/tx/send & send-simple brûlent in−out).
        // v0.24.2: 28 → 29 (audit rang 3/B2 : /admin/contracts rejette les refunds
        //          AccumulateRefund en PMS natif — anti mint natif illimité).
        // v0.25.0: 29 → 30 (audit rang 3/B3 : /submit/block rejette un BridgeMint
        //          rejouant un lock déjà consommé — anti-replay durable).
        // v0.26.0: 30 → 31 (audit rang 3/B3 : réconciliation cross-ledger du
        //          BridgeMint avec son BridgeLock — montant/asset/destinataire/ledger).
        // v0.27.0: 31 → 32 (protocole 2.7 : POST /v1/market/settle + champs royalty
        //          sur tokens/create & sft/classes).
        // v0.28.0: 32 → 33 (protocole 2.7 : POST /admin/royalty — royalty mutable
        //          post-mint).
        // v0.29.0: 33 → 34 (protocole 2.7 : /v1/royalty/{prepare,update} co-signés
        //          par le bénéficiaire courant ; /admin/royalty retiré).
        // v0.29.0 (revue D1): 34 → 35 (market/royalty renvoient ApiError à code
        //          stable ; nouveau code 1050 Forbidden). Toujours dans le cycle
        //          non-publié 0.29.0 — l'API 34 n'a jamais été released.
        // v0.30.0: 35 → 36 (fee distribution crédite la part producteur du
        //          coordinateur au lieu de la rediriger vers la treasury).
        // v0.30.1: 36 → 37 (durcissement sécurité : nft/mint create-only +
        //          signature d'émetteur ; node routes gated ; /internal retiré
        //          du public ; api-key fail-closed en prod).
        // v0.30.1: 37 → 38 (auth crypto node registry : /v1/register &
        //          /v1/heartbeat acceptent une signature de node_pk).
        assert_eq!(parsed.api_version, 38);
    }
}
