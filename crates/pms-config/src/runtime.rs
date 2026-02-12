//! Types pour la configuration runtime modifiable via transactions signées.
//!
//! Le système Hot-Swap permet au Coordinator de modifier les paramètres réseau
//! via des blocs signés contenant `PlainPayload::ConfigUpdate`.

use serde::{Deserialize, Serialize};

/// Configuration runtime modifiable dynamiquement.
///
/// Ces paramètres peuvent être changés par le Coordinator via une transaction
/// `PlainPayload::ConfigUpdate`. Ils sont persistés dans RocksDB et chargés
/// au démarrage du nœud.
///
/// # Single Writer Mode (Private DAG)
/// En mode Single Writer, seuls le Coordinator et le Treasury reçoivent les fees.
/// - `coordinator_fee_bps` : Part du Coordinator (défaut: 67%)
/// - `treasury_fee_bps` : Part du Treasury (défaut: 33%)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeConfig {
    /// Taux de commission sur les transactions (en basis points, 100 = 1%)
    pub fee_rate_bps: u32,

    /// Frais fixes par transaction (ex: "0.001")
    pub base_fee: String,

    /// Part des fees allant au Coordinator (en basis points, 6700 = 67%)
    /// [SINGLE WRITER] Remplace l'ancien `platform_fee_bps`
    pub coordinator_fee_bps: u32,

    /// Part des fees allant au Treasury (en basis points, 3300 = 33%)
    /// [SINGLE WRITER] Remplace l'ancien `node_fee_bps`
    pub treasury_fee_bps: u32,

    /// Nombre minimum de bits de zéro pour le PoW
    pub min_pow_bits: u8,

    /// Maximum de tokens mintables par bloc
    pub max_mint_per_block: u64,

    /// Minting autorisé ou non (kill switch)
    pub mint_enabled: bool,

    /// Block ID où cette config a été appliquée (vide = config initiale)
    pub updated_at_block: String,

    /// Timestamp Unix (ms) de la dernière mise à jour
    pub updated_at_timestamp: i64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            fee_rate_bps: 300,                 // 3% de commission totale
            base_fee: "0.0000001".to_string(), // Frais fixes par défaut
            coordinator_fee_bps: 6700,         // 67% des fees au Coordinator
            treasury_fee_bps: 3300,            // 33% des fees au Treasury
            min_pow_bits: 8,                   // Difficulté minimale
            max_mint_per_block: 1_000_000,
            mint_enabled: true,
            updated_at_block: String::new(),
            updated_at_timestamp: 0,
        }
    }
}

impl RuntimeConfig {
    /// Crée une nouvelle config avec les valeurs par défaut.
    pub fn new() -> Self {
        Self::default()
    }

    /// Applique une mise à jour et retourne la nouvelle config.
    pub fn apply_update(&self, update: &ConfigUpdate, block_id: &str, timestamp: i64) -> Self {
        let mut new_config = self.clone();
        new_config.updated_at_block = block_id.to_string();
        new_config.updated_at_timestamp = timestamp;

        match update {
            ConfigUpdate::SetFeeRate { bps } => {
                new_config.fee_rate_bps = *bps;
            }
            ConfigUpdate::SetBaseFee { fee } => {
                new_config.base_fee = fee.clone();
            }
            ConfigUpdate::SetCoordinatorFee { bps } => {
                new_config.coordinator_fee_bps = *bps;
            }
            ConfigUpdate::SetTreasuryFee { bps } => {
                new_config.treasury_fee_bps = *bps;
            }
            ConfigUpdate::SetMinPow { bits } => {
                new_config.min_pow_bits = *bits;
            }
            ConfigUpdate::SetMaxMint { amount } => {
                new_config.max_mint_per_block = *amount;
            }
            ConfigUpdate::SetMintEnabled { enabled } => {
                new_config.mint_enabled = *enabled;
            }
            ConfigUpdate::BatchUpdate(updates) => {
                for u in updates {
                    new_config = new_config.apply_update(u, block_id, timestamp);
                }
            }
        }

        new_config
    }

    /// Valide que les pourcentages totalisent 100% (10000 bps)
    pub fn validate_fee_split(&self) -> Result<(), String> {
        let total = self.coordinator_fee_bps + self.treasury_fee_bps;
        if total != 10000 {
            return Err(format!(
                "Coordinator + Treasury fees must sum to 10000 bps (100%), got {} bps",
                total
            ));
        }
        Ok(())
    }
}

/// Type de mise à jour de configuration.
///
/// Envoyé dans un bloc signé par le Coordinator via `PlainPayload::ConfigUpdate`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConfigUpdate {
    /// Modifier le taux de commission (basis points, max 10000 = 100%)
    SetFeeRate { bps: u32 },

    /// Modifier les frais fixes
    SetBaseFee { fee: String },

    /// Modifier la part du Coordinator (basis points, max 10000 = 100%)
    /// [SINGLE WRITER] Remplace l'ancien `SetPlatformFee`
    SetCoordinatorFee { bps: u32 },

    /// Modifier la part du Treasury (basis points, max 10000 = 100%)
    /// [SINGLE WRITER] Remplace l'ancien `SetNodeFee`
    SetTreasuryFee { bps: u32 },

    /// Modifier la difficulté PoW minimale
    SetMinPow { bits: u8 },

    /// Modifier le maximum de tokens par mint
    SetMaxMint { amount: u64 },

    /// Activer/désactiver le minting
    SetMintEnabled { enabled: bool },

    /// Appliquer plusieurs updates en une transaction
    BatchUpdate(Vec<ConfigUpdate>),
}

impl ConfigUpdate {
    /// Retourne une description humaine de la mise à jour.
    pub fn description(&self) -> String {
        match self {
            Self::SetFeeRate { bps } => format!("SetFeeRate({}bps)", bps),
            Self::SetBaseFee { fee } => format!("SetBaseFee({})", fee),
            Self::SetCoordinatorFee { bps } => format!("SetCoordinatorFee({}bps)", bps),
            Self::SetTreasuryFee { bps } => format!("SetTreasuryFee({}bps)", bps),
            Self::SetMinPow { bits } => format!("SetMinPow({}bits)", bits),
            Self::SetMaxMint { amount } => format!("SetMaxMint({})", amount),
            Self::SetMintEnabled { enabled } => format!("SetMintEnabled({})", enabled),
            Self::BatchUpdate(updates) => {
                format!("BatchUpdate({} items)", updates.len())
            }
        }
    }
}

/// Entrée d'historique pour un changement de config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigHistoryEntry {
    /// Block ID où le changement a été appliqué
    pub block_id: String,

    /// Timestamp du changement
    pub timestamp: i64,

    /// Type de mise à jour appliquée
    pub update: ConfigUpdate,

    /// Config résultante après application
    pub resulting_config: RuntimeConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_single_update() {
        let config = RuntimeConfig::default();
        let update = ConfigUpdate::SetFeeRate { bps: 200 };

        let new_config = config.apply_update(&update, "block-001", 1234567890);

        assert_eq!(new_config.fee_rate_bps, 200);
        assert_eq!(new_config.updated_at_block, "block-001");
    }

    #[test]
    fn test_apply_batch_update() {
        let config = RuntimeConfig::default();
        let update = ConfigUpdate::BatchUpdate(vec![
            ConfigUpdate::SetFeeRate { bps: 150 },
            ConfigUpdate::SetMintEnabled { enabled: false },
        ]);

        let new_config = config.apply_update(&update, "block-002", 1234567890);

        assert_eq!(new_config.fee_rate_bps, 150);
        assert!(!new_config.mint_enabled);
    }

    #[test]
    fn test_default_fee_split_is_valid() {
        let config = RuntimeConfig::default();
        // 67% + 33% = 100%
        assert_eq!(config.coordinator_fee_bps, 6700);
        assert_eq!(config.treasury_fee_bps, 3300);
        assert!(config.validate_fee_split().is_ok());
    }

    #[test]
    fn test_invalid_fee_split() {
        let mut config = RuntimeConfig::default();
        config.coordinator_fee_bps = 5000;
        config.treasury_fee_bps = 3000; // Total = 8000, pas 10000

        assert!(config.validate_fee_split().is_err());
    }

    #[test]
    fn test_apply_coordinator_fee_update() {
        let config = RuntimeConfig::default();
        let update = ConfigUpdate::SetCoordinatorFee { bps: 7000 };

        let new_config = config.apply_update(&update, "block-003", 1234567890);

        assert_eq!(new_config.coordinator_fee_bps, 7000);
    }

    #[test]
    fn test_apply_treasury_fee_update() {
        let config = RuntimeConfig::default();
        let update = ConfigUpdate::SetTreasuryFee { bps: 4000 };

        let new_config = config.apply_update(&update, "block-004", 1234567890);

        assert_eq!(new_config.treasury_fee_bps, 4000);
    }
}
