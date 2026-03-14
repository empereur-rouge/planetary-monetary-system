// ═══════════════════════════════════════════════════════════════════════════════
// supply.rs - API endpoint pour le supply en circulation
// ═══════════════════════════════════════════════════════════════════════════════
//
// Cet endpoint retourne le nombre total de tokens en circulation.
// Il est important pour la transparence de la supply totale de la monnaie.
//
// GET /v1/supply -> { "circulating_supply": "...", "utxo_count": N }
// ═══════════════════════════════════════════════════════════════════════════════

use crate::api::AppState;
use axum::{
    Json,
    extract::{Query, State},
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Réponse de l'endpoint /v1/supply
#[derive(Debug, Serialize)]
pub struct CirculatingSupplyResponse {
    /// Nombre total de tokens en circulation (somme des UTXOs non dépensés)
    pub circulating_supply: String,

    /// Nombre de UTXOs actifs (outputs non dépensés)
    pub utxo_count: u64,

    /// Solde du Wallet Admin (si configuré localement sur ce nœud)
    pub admin_balance: String,

    /// Solde du Node Identity Wallet (là où vont les rewards)
    #[serde(default)]
    pub node_balance: String,

    /// Solde total de la Trésorerie (somme des wallets treasury configurés)
    pub treasury_balance: String,

    /// Détail par wallet de trésorerie
    #[serde(default)]
    pub treasury_details: Vec<TreasuryWalletDetail>,

    /// Cumulative total of fees permanently burned (deflationary mechanism)
    pub total_burned: String,

    /// Asset ID queried (None = PMS natif)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,

    /// Native token symbol for this ledger
    pub symbol: String,
}

#[derive(Debug, Serialize)]
pub struct TreasuryWalletDetail {
    pub address: String,
    pub balance: String,
}

#[derive(Debug, Deserialize)]
pub struct SupplyQuery {
    /// Optional asset_id filter. If absent, returns PMS native supply.
    pub asset_id: Option<String>,
}

/// GET /v1/supply - Retourne le supply circulant
/// Query params: ?asset_id=edenite (optional, default = PMS natif)
pub async fn get_circulating_supply(
    State(state): State<AppState>,
    Query(query): Query<SupplyQuery>,
) -> Json<CirculatingSupplyResponse> {
    // Accède au ShardedUtxoSet via l'adapter du serveur
    let adapter = state.srv.adapter_arc();

    // Calcule le supply via la méthode du trait.
    // Si pas d'asset_id explicite : essaie le natif PMS, et si 0 + tokens enregistrés,
    // fallback automatique sur le premier token (ex: edenite sur le ledger eden).
    let explicit_asset = query.asset_id.clone();
    let (total, count, resolved_asset) = if let Some(ref asset_id) = explicit_asset {
        let (t, c) = adapter.circulating_supply_by_asset(Some(asset_id)).await;
        (t, c, Some(asset_id.clone()))
    } else {
        let (t, c) = adapter.circulating_supply().await;
        if t == Decimal::ZERO {
            // Native supply is 0 — check if there's a registered custom token
            if let Ok(tokens) = state.store.list_tokens() {
                if let Some(first) = tokens.first() {
                    let (t2, c2) = adapter
                        .circulating_supply_by_asset(Some(&first.asset_id))
                        .await;
                    (t2, c2, Some(first.asset_id.clone()))
                } else {
                    (t, c, None)
                }
            } else {
                (t, c, None)
            }
        } else {
            (t, c, None)
        }
    };

    let settings = &state.settings;
    let mut admin_bal = Decimal::ZERO;
    let mut treasury_bal = Decimal::ZERO;
    let mut treasury_details = Vec::new();

    // 1. Admin Wallet Balance (coordinator)
    if let Some(path) = &settings.secrets.admin_wallet_file {
        if let Ok(wallet) = pms_wallet::Wallet::load_from_file(path) {
            let addr = wallet.get_address(&settings.address.hrp);
            admin_bal = adapter.balance_by_address(&addr).await;
        }
    }

    // 2. Node Identity Balance (rewards)
    let node_addr = state.node_wallet.get_address(&settings.address.hrp);
    let node_bal = adapter.balance_by_address(&node_addr).await;

    // 3. Treasury Balance - pre-loaded wallets, fallback to fees config
    if !state.treasury_wallets.is_empty() {
        for addr in &state.treasury_wallets.list {
            let bal = adapter.balance_by_address(addr).await;
            treasury_bal += bal;
            treasury_details.push(TreasuryWalletDetail {
                address: addr.clone(),
                balance: bal.to_string(),
            });
        }
    } else {
        for addr in &settings.fees.treasury_addresses {
            let bal = adapter.balance_by_address(addr).await;
            treasury_bal += bal;
            treasury_details.push(TreasuryWalletDetail {
                address: addr.clone(),
                balance: bal.to_string(),
            });
        }
    }

    // Resolve native symbol: per-ledger def > network config > default "PMS"
    let symbol = state
        .ledger_mgr
        .as_ref()
        .and_then(|mgr| mgr.get(&state.ledger_id))
        .and_then(|inst| inst.def.symbol.clone())
        .or_else(|| settings.network.symbol.clone())
        .unwrap_or_else(|| "PMS".to_string());

    let total_burned = state
        .store
        .get_total_burned()
        .unwrap_or(Decimal::ZERO)
        .to_string();

    Json(CirculatingSupplyResponse {
        circulating_supply: total.to_string(),
        utxo_count: count as u64,
        admin_balance: admin_bal.to_string(),
        node_balance: node_bal.to_string(),
        treasury_balance: treasury_bal.to_string(),
        treasury_details,
        total_burned,
        asset_id: resolved_asset,
        symbol,
    })
}
