use serde::{Deserialize, Serialize};

/// Direction d'un pont entre deux ledgers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BridgeDirection {
    /// Transferts dans les deux sens (A↔B)
    Bidirectional,
    /// Transferts de A vers B uniquement
    AtoB,
    /// Transferts de B vers A uniquement
    BtoA,
}

/// Lien de pont entre deux ledgers.
/// Stocké dans la CF `bridge_links` du RocksDB partagé.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeLink {
    /// Premier ledger (trié alphabétiquement)
    pub ledger_a: String,
    /// Second ledger (trié alphabétiquement)
    pub ledger_b: String,
    /// Direction autorisée
    pub direction: BridgeDirection,
    /// Pont actif ou coupé
    pub enabled: bool,
    /// Timestamp de création (ms)
    pub created_at: i64,
    /// Timestamp de désactivation (None = jamais désactivé)
    pub disabled_at: Option<i64>,
    /// Clés publiques qui ont autorisé ce pont
    pub authorized_by: Vec<String>,
}

impl BridgeLink {
    /// Normalise les IDs de ledger (tri alphabétique) pour la clé de stockage.
    pub fn storage_key(a: &str, b: &str) -> String {
        if a <= b {
            format!("{}:{}", a, b)
        } else {
            format!("{}:{}", b, a)
        }
    }

    /// Vérifie si un transfert de `from` vers `to` est autorisé par ce lien.
    pub fn allows_transfer(&self, from: &str, to: &str) -> bool {
        if !self.enabled {
            return false;
        }
        match &self.direction {
            BridgeDirection::Bidirectional => {
                (from == self.ledger_a && to == self.ledger_b)
                    || (from == self.ledger_b && to == self.ledger_a)
            }
            BridgeDirection::AtoB => from == self.ledger_a && to == self.ledger_b,
            BridgeDirection::BtoA => from == self.ledger_b && to == self.ledger_a,
        }
    }
}

/// Requête de transfert cross-ledger.
#[derive(Debug, Clone, Deserialize)]
pub struct BridgeTransferRequest {
    /// Ledger source
    pub from_ledger: String,
    /// Ledger destination
    pub to_ledger: String,
    /// Adresse source (possesseur des fonds)
    pub from_address: String,
    /// Adresse destination
    pub to_address: String,
    /// Montant à transférer
    pub amount: String,
    /// Asset ID (None = PMS natif)
    #[serde(default)]
    pub asset_id: Option<String>,
}

/// Réponse d'un transfert cross-ledger.
#[derive(Debug, Clone, Serialize)]
pub struct BridgeTransferResponse {
    /// ID du bloc BridgeLock sur le ledger source
    pub lock_block_id: String,
    /// ID du bloc BridgeMint sur le ledger destination
    pub mint_block_id: String,
    /// Ledger source
    pub from_ledger: String,
    /// Ledger destination
    pub to_ledger: String,
    /// Montant transféré
    pub amount: String,
    /// Asset ID
    pub asset_id: Option<String>,
}

/// Requête d'activation d'un pont.
#[derive(Debug, Clone, Deserialize)]
pub struct BridgeEnableRequest {
    pub ledger_a: String,
    pub ledger_b: String,
    #[serde(default = "default_direction")]
    pub direction: BridgeDirection,
}

fn default_direction() -> BridgeDirection {
    BridgeDirection::Bidirectional
}

/// Requête de désactivation d'un pont.
#[derive(Debug, Clone, Deserialize)]
pub struct BridgeDisableRequest {
    pub ledger_a: String,
    pub ledger_b: String,
}
