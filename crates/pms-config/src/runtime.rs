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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeConfig {
    /// Taux de commission sur les transactions (en basis points, 100 = 1%)
    pub fee_rate_bps: u32,

    /// Part des fees allant à la plateforme (en basis points, 2000 = 20%)
    pub platform_fee_bps: u32,

    /// Part des fees allant au pool des nœuds (en basis points, 3000 = 30%)
    pub node_fee_bps: u32,

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
            fee_rate_bps: 100,      // 1%
            platform_fee_bps: 2000, // 20% des fees
            node_fee_bps: 3000,     // 30% des fees pour les nœuds
            min_pow_bits: 8,        // Difficulté minimale
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
            ConfigUpdate::SetPlatformFee { bps } => {
                new_config.platform_fee_bps = *bps;
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
            ConfigUpdate::SetNodeFee { bps } => {
                new_config.node_fee_bps = *bps;
            }
            ConfigUpdate::BatchUpdate(updates) => {
                for u in updates {
                    new_config = new_config.apply_update(u, block_id, timestamp);
                }
            }
        }

        new_config
    }
}

/// Type de mise à jour de configuration.
///
/// Envoyé dans un bloc signé par le Coordinator via `PlainPayload::ConfigUpdate`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConfigUpdate {
    /// Modifier le taux de commission (basis points, max 10000 = 100%)
    SetFeeRate { bps: u32 },

    /// Modifier la part plateforme (basis points, max 10000 = 100%)
    SetPlatformFee { bps: u32 },

    /// Modifier la difficulté PoW minimale
    SetMinPow { bits: u8 },

    /// Modifier le maximum de tokens par mint
    SetMaxMint { amount: u64 },

    /// Activer/désactiver le minting
    SetMintEnabled { enabled: bool },

    /// Modifier la part des fees pour les nœuds (basis points)
    SetNodeFee { bps: u32 },

    /// Appliquer plusieurs updates en une transaction
    BatchUpdate(Vec<ConfigUpdate>),
}

impl ConfigUpdate {
    /// Retourne une description humaine de la mise à jour.
    pub fn description(&self) -> String {
        match self {
            Self::SetFeeRate { bps } => format!("SetFeeRate({}bps)", bps),
            Self::SetPlatformFee { bps } => format!("SetPlatformFee({}bps)", bps),
            Self::SetMinPow { bits } => format!("SetMinPow({}bits)", bits),
            Self::SetMaxMint { amount } => format!("SetMaxMint({})", amount),
            Self::SetMintEnabled { enabled } => format!("SetMintEnabled({})", enabled),
            Self::SetNodeFee { bps } => format!("SetNodeFee({}bps)", bps),
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
}
