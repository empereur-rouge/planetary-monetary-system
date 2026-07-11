use crate::api::AppState;
use axum::Json;
use axum::extract::Path;
use axum::extract::State;
use axum::http::StatusCode;
use pms_wallet::decode_address;

#[derive(serde::Serialize)]
pub struct Outpoint {
    pub txid: String,
    pub index: u32,
}

#[derive(serde::Serialize)]
pub struct UtxoItem {
    pub address: String,
    pub amount: String,
    pub outpoint: Outpoint,
}

#[derive(serde::Serialize)]
pub struct UtxoResp {
    pub utxos: Vec<UtxoItem>,
}

#[derive(serde::Deserialize)]
pub struct BalanceReq {
    bech32_addr: String,
    // Kept for backward compatibility (no longer used server-side)
    #[allow(dead_code)]
    x25519_sk_hex: String,
    #[allow(dead_code)]
    ecdsa_pk_hex: String,
    #[allow(dead_code)]
    scan_limit: Option<usize>,
}
#[derive(serde::Serialize)]
pub struct UtxoView {
    txid: String,
    index: u32,
    amount: String,
}
#[derive(serde::Serialize)]
pub struct BalanceResp {
    balance: String,
    utxos: Vec<UtxoView>,
}

pub async fn wallet_balance(
    State(app): State<AppState>,
    Json(req): Json<BalanceReq>,
) -> Result<Json<BalanceResp>, (StatusCode, String)> {
    // Validate address
    let (_h20, _xpk_hex) = decode_address(&req.bech32_addr)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("invalid address: {e}")))?;

    // Balance from O(1) cache + UTXO list from shard-batched lookup
    let adapter = app.srv.adapter_arc();
    let balance = adapter.balance_by_address(&req.bech32_addr).await;
    let utxos = adapter.utxos_by_address(&req.bech32_addr).await;

    let list = utxos
        .into_iter()
        .map(|(oid, txo)| UtxoView {
            txid: oid.txid,
            index: oid.index,
            amount: txo.amount,
        })
        .collect::<Vec<_>>();

    Ok(Json(BalanceResp {
        balance: balance.to_string(),
        utxos: list,
    }))
}

/// Simple balance query by address.
///
/// Supports optional `ledger_id` to query a custom ledger's balance from the
/// main endpoint, and optional `asset_id` to query custom token balances.
///
/// # Examples
/// ```json
/// { "address": "8e1abc..." }                                       // PMS on current ledger
/// { "address": "8e1abc...", "asset_id": "edenite" }                // EDN on current ledger
/// { "address": "8e1abc...", "ledger_id": "eden" }                  // PMS on eden
/// { "address": "8e1abc...", "ledger_id": "eden", "asset_id": "edenite" } // EDN on eden
/// ```
#[derive(serde::Deserialize)]
pub struct SimpleBalanceReq {
    address: String,
    /// Optional: query a specific ledger (e.g. "eden"). If omitted, uses the
    /// current ledger (main, or the one from the `/l/{id}/` URL prefix).
    ledger_id: Option<String>,
    /// Optional: query balance for a specific asset (e.g. "edenite").
    /// If omitted, returns native PMS balance.
    asset_id: Option<String>,
    /// Optional: pubkey secp256k1 hex du wallet. Fourni → le solde renvoyé
    /// **unit les formes-propriétaire** : l'adresse interrogée + la pubkey hex
    /// (± `0x`). Ainsi un solde interrogé sur le **bech32m canonique** inclut
    /// aussi les fonds mintés vers la forme hex (« forme SDK »), levant
    /// l'asymétrie « dépensable mais invisible ». La pubkey DOIT correspondre à
    /// l'adresse (même hash20), sinon `400`.
    #[serde(default)]
    public_key_hex: Option<String>,
}

/// Formes-propriétaire à sommer pour un solde « unifié » d'un wallet.
///
/// Toujours l'adresse interrogée ; **plus** la pubkey hex (± `0x`) si
/// `public_key_hex` est fourni ET correspond à l'adresse (même identité hash20).
/// Comme l'index d'adresse est keyé par string exacte, des strings distincts =
/// buckets disjoints → sommer sur l'ensemble dédupliqué unit sans double-compter.
///
/// Ne peut pas reconstruire la forme bech32m depuis la pubkey seule (il faut la
/// clé x25519 nœud) : le client interroge donc par son **bech32m canonique** et
/// fournit `public_key_hex` → l'union {bech32m interrogé} ∪ {hex} couvre les deux
/// buckets réels sans secret ni x25519.
fn balance_union_forms(
    address: &str,
    public_key_hex: Option<&str>,
) -> Result<Vec<String>, (StatusCode, String)> {
    let mut forms = vec![address.to_string()];
    if let Some(pk) = public_key_hex {
        let pk_norm = pk.trim().trim_start_matches("0x").to_lowercase();
        if !pms_wallet::is_valid_secp_pubkey_hex(&pk_norm) {
            return Err((StatusCode::BAD_REQUEST, "invalid public_key_hex".to_string()));
        }
        // La pubkey doit correspondre à l'adresse interrogée (anti-somme de
        // wallets sans rapport). Même relation forme↔hash20 que l'autorisation.
        let pk_h20 = hex::encode(pms_wallet::pubkey_hash20(
            &hex::decode(&pk_norm).expect("valid hex checked above"),
        ));
        if pms_core::validations::ownership::address_identity(address) != pk_h20 {
            return Err((
                StatusCode::BAD_REQUEST,
                "public_key_hex does not match address".to_string(),
            ));
        }
        for form in [pk_norm.clone(), format!("0x{pk_norm}")] {
            if !forms.contains(&form) {
                forms.push(form);
            }
        }
    }
    Ok(forms)
}
#[derive(serde::Serialize)]
pub struct SimpleBalanceResp {
    balance: String,
    /// Echoes back the ledger that was queried.
    ledger_id: String,
    /// Echoes back the asset that was queried (null = PMS native).
    asset_id: Option<String>,
}

/// `POST /v1/balance` — query wallet balance by address, with optional
/// `ledger_id` and `asset_id` parameters.
///
/// **Sémantique par forme d'adresse (v0.32.0).** Par défaut le solde est celui
/// du **string d'adresse exact** interrogé. Depuis v0.32.0 les chemins de dépense
/// (`select_utxos_multi`) unissent les formes-propriétaire d'un wallet (bech32m
/// canonique + pubkey hex « forme SDK ») → des fonds mintés vers la pubkey hex
/// sont dépensables même si `get_address` dérive le bech32m. Pour que le solde
/// reflète cette réalité, fournir le champ optionnel **`public_key_hex`** : le
/// solde somme alors l'adresse interrogée + la forme hex (cf.
/// [`balance_union_forms`]), levant l'asymétrie « dépensable mais invisible ».
/// Sans `public_key_hex`, comportement historique (une seule forme). La bech32m
/// n'étant pas reconstructible depuis la pubkey seule, interroger par le
/// **bech32m canonique** (`POST /v1/wallet/canonical-address`) + `public_key_hex`.
pub async fn balance_by_address(
    State(app): State<AppState>,
    Json(req): Json<SimpleBalanceReq>,
) -> Result<Json<SimpleBalanceResp>, (StatusCode, String)> {
    let effective_ledger = req.ledger_id.as_deref().unwrap_or(&app.ledger_id);

    // Resolve the adapter for the target ledger.
    let adapter = if req.ledger_id.is_some() && effective_ledger != app.ledger_id {
        // Cross-ledger query — look up from LedgerManager
        let mgr = app.ledger_mgr.as_ref().ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "multi-ledger not enabled".to_string(),
            )
        })?;
        let instance = mgr.get(effective_ledger).ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                format!("ledger '{}' not found", effective_ledger),
            )
        })?;
        instance.adapter.clone()
    } else {
        app.srv.adapter_arc()
    };

    // Solde unifié sur les formes-propriétaire (cf. `balance_union_forms`) :
    // sans `public_key_hex`, c'est exactement l'adresse interrogée (comportement
    // historique) ; avec, on somme aussi la forme hex (lève « dépensable mais
    // invisible »).
    let forms = balance_union_forms(&req.address, req.public_key_hex.as_deref())?;
    let mut balance = rust_decimal::Decimal::ZERO;
    for form in &forms {
        balance += adapter
            .balance_by_address_and_asset(form, req.asset_id.as_deref())
            .await;
    }

    Ok(Json(SimpleBalanceResp {
        balance: balance.to_string(),
        ledger_id: effective_ledger.to_string(),
        asset_id: req.asset_id,
    }))
}

/// Response format for UTXOs — flat structure returned by `GET /v1/wallet/{address}/utxos`.
///
/// Includes `asset_id` for multi-asset support (PMS native = `null`, custom token = `"edenite"`).
#[derive(serde::Serialize)]
pub struct UtxoFlatItem {
    #[serde(rename = "txId")]
    pub tx_id: String,
    #[serde(rename = "outIdx")]
    pub out_idx: u32,
    /// L'output complet, aplati : `address`, `amount`, `asset_id` (null = PMS
    /// natif) + champs protocole optionnels (`locked_until`, `spend_condition`,
    /// `created_at`). `flatten` garantit que tout futur champ de `TxOutput`
    /// est exposé automatiquement — les SDK voient toujours les contraintes
    /// de dépense réelles.
    #[serde(flatten)]
    pub output: pms_types::TxOutput,
}

#[derive(serde::Serialize)]
pub struct UtxoFlatResp {
    pub utxos: Vec<UtxoFlatItem>,
}

pub async fn get_utxos_by_address(
    State(app): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<UtxoFlatResp>, (StatusCode, String)> {
    // Read from in-memory UTXO set (RAM) instead of RocksDB for consistency with /v1/balance
    let utxos = app.srv.adapter_arc().utxos_by_address(&address).await;

    let list = utxos
        .into_iter()
        .map(|(output_id, tx_output)| UtxoFlatItem {
            tx_id: output_id.txid,
            out_idx: output_id.index,
            output: tx_output,
        })
        .collect();

    Ok(Json(UtxoFlatResp { utxos: list }))
}
