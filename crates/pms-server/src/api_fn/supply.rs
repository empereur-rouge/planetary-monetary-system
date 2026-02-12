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
use axum::{Json, extract::{Query, State}};
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

    /// Asset ID queried (None = PMS natif)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<String>,
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

    // Calcule le supply via la méthode du trait
    let (total, count) = if let Some(ref asset_id) = query.asset_id {
        adapter.circulating_supply_by_asset(Some(asset_id)).await
    } else {
        adapter.circulating_supply().await
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

    Json(CirculatingSupplyResponse {
        circulating_supply: total.to_string(),
        utxo_count: count as u64,
        admin_balance: admin_bal.to_string(),
        node_balance: node_bal.to_string(),
        treasury_balance: treasury_bal.to_string(),
        treasury_details,
        asset_id: query.asset_id,
    })
}
