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
use axum::{Json, extract::State};
use pms_config::{load_config, treasury_wallets::load_treasury_wallets};
use rust_decimal::Decimal;
use serde::Serialize;

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
}

#[derive(Debug, Serialize)]
pub struct TreasuryWalletDetail {
    pub address: String,
    pub balance: String,
}

/// GET /v1/supply - Retourne le supply circulant
pub async fn get_circulating_supply(
    State(state): State<AppState>,
) -> Json<CirculatingSupplyResponse> {
    // Accède au ShardedUtxoSet via l'adapter du serveur
    let adapter = state.srv.adapter_arc();

    // Calcule le supply via la méthode du trait
    let (total, count) = adapter.circulating_supply().await;

    // Charger la config pour les adresses spéciales
    // Note: En prod, on pourrait cacher ces adresses dans AppState pour éviter de recharger la config
    let mut admin_bal = Decimal::ZERO;
    let mut node_bal = Decimal::ZERO;
    let mut treasury_bal = Decimal::ZERO;
    let mut treasury_details = Vec::new();

    if let Ok(config) = load_config() {
        // 1. Admin Wallet Balance (local node admin)
        if let Some(path) = &config.secrets.admin_wallet_file {
            if let Ok(wallet) = pms_wallet::Wallet::load_from_file(path) {
                let admin_addr = wallet.get_address(&config.address.hrp);
                admin_bal = adapter.balance_by_address(&admin_addr).await;
            }
        }

        // 1b. Node Identity Balance (Rewards)
        let node_addr = state.node_wallet.get_address(&config.address.hrp);
        node_bal = adapter.balance_by_address(&node_addr).await;

        // 2. Treasury Balance (as configured on this node)
        if let Some(path) = &config.admin.treasury_wallets_file {
            if let Some(coord_pk) = &config.validation.coordinator_public_key {
                if let Ok(wallets) = load_treasury_wallets(path, coord_pk) {
                    for addr in wallets.list {
                        let bal = adapter.balance_by_address(&addr).await;
                        treasury_bal += bal;
                        treasury_details.push(TreasuryWalletDetail {
                            address: addr,
                            balance: bal.to_string(),
                        });
                    }
                }
            }
        }
    }

    Json(CirculatingSupplyResponse {
        circulating_supply: total.to_string(),
        utxo_count: count as u64,
        admin_balance: admin_bal.to_string(),
        node_balance: node_bal.to_string(),
        treasury_balance: treasury_bal.to_string(),
        treasury_details,
    })
}
