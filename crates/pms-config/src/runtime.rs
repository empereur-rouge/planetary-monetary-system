//! Types pour la configuration runtime modifiable via transactions signées.
//!
//! Le système Hot-Swap permet au Coordinator de modifier les paramètres réseau
//! via des blocs signés contenant `PlainPayload::ConfigUpdate`.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Un palier dans un barème de fees progressif/dégressif.
/// Les paliers sont ordonnés par `up_to` croissant. Le dernier palier a `up_to: None` (infini).
/// Le calcul est marginal : chaque palier s'applique uniquement à la portion du montant dans sa tranche.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeeTier {
    /// Borne supérieure de ce palier (exclusive). None = pas de borne (dernier palier catch-all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up_to: Option<String>,
    /// Ratio de fee pour les montants dans ce palier (ex: "0.03" = 3%)
    pub ratio: String,
}

/// Valide un barème de fee tiers.
/// - Au moins un tier
/// - `up_to` strictement croissants (parsés en Decimal)
/// - Dernier tier doit avoir `up_to: None` (catch-all)
/// - Tous les non-derniers doivent avoir `up_to: Some`
/// - Tous les `ratio` parsent en Decimal >= 0
pub fn validate_fee_tiers(tiers: &[FeeTier]) -> Result<(), String> {
    if tiers.is_empty() {
        return Err("Fee tiers must have at least one tier".into());
    }

    if tiers.last().unwrap().up_to.is_some() {
        return Err("Last fee tier must have up_to: None (catch-all)".into());
    }

    let mut prev_boundary = Decimal::ZERO;
    for (i, tier) in tiers.iter().enumerate() {
        let _ratio = Decimal::from_str_exact(&tier.ratio)
            .map_err(|e| format!("Tier {} ratio '{}' is not a valid decimal: {}", i, tier.ratio, e))?;
        if _ratio < Decimal::ZERO {
            return Err(format!("Tier {} ratio must be >= 0, got {}", i, _ratio));
        }

        if i < tiers.len() - 1 {
            match &tier.up_to {
                None => return Err(format!("Tier {} must have up_to (only last tier can be None)", i)),
                Some(up_to_str) => {
                    let up_to = Decimal::from_str_exact(up_to_str)
                        .map_err(|e| format!("Tier {} up_to '{}' is not a valid decimal: {}", i, up_to_str, e))?;
                    if up_to <= prev_boundary {
                        return Err(format!(
                            "Tier {} up_to ({}) must be strictly greater than previous boundary ({})",
                            i, up_to, prev_boundary
                        ));
                    }
                    prev_boundary = up_to;
                }
            }
        }
    }

    Ok(())
}

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

    /// Barème de fees par paliers. Si non vide, remplace fee_rate_bps pour le calcul.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fee_tiers: Vec<FeeTier>,

    /// Distribution N-way des fees. Si None, utilise coordinator_fee_bps/treasury_fee_bps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee_distribution: Option<crate::FeeDistributionConfig>,

    /// Frais fixe sur le minting de tokens custom (ex: "1.0" PMS). None = pas de mint fee.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mint_fee_base: Option<String>,
    /// Ratio sur le montant minté (ex: "0.01" = 1%). None = pas de ratio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mint_fee_ratio: Option<String>,

    /// Fee one-time pour la création de token (ex: "100" PMS). None = pas de fee.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_creation_fee: Option<String>,

    /// Fee sur le mint de NFT (ex: "0.5" PMS). None = pas de fee.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nft_mint_fee: Option<String>,
    /// Types de NFT exemptés de fee (ex: ["cube", "reward"]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nft_fee_exempt_types: Vec<String>,

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
            fee_tiers: Vec::new(),
            fee_distribution: None,
            mint_fee_base: None,
            mint_fee_ratio: None,
            token_creation_fee: None,
            nft_mint_fee: None,
            nft_fee_exempt_types: Vec::new(),
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

    /// Applique une mise à jour, valide le résultat, et retourne la nouvelle config.
    /// La validation globale (`validate_fee_config`) s'exécute une seule fois
    /// après toutes les mutations (y compris les sous-updates d'un BatchUpdate).
    pub fn apply_update(&self, update: &ConfigUpdate, block_id: &str, timestamp: i64) -> Result<Self, String> {
        let result = self.apply_update_inner(update, block_id, timestamp)?;
        result.validate_fee_config()?;
        Ok(result)
    }

    /// Applique les mutations sans validation globale (utilisé en interne par BatchUpdate).
    /// Valide eagerly les données malformées (tiers invalides, distribution invalide).
    fn apply_update_inner(&self, update: &ConfigUpdate, block_id: &str, timestamp: i64) -> Result<Self, String> {
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
            ConfigUpdate::SetFeeTiers { tiers } => {
                validate_fee_tiers(tiers)?;
                new_config.fee_tiers = tiers.clone();
            }
            ConfigUpdate::ClearFeeTiers => {
                new_config.fee_tiers.clear();
            }
            ConfigUpdate::SetFeeDistribution { beneficiaries } => {
                let dist = crate::FeeDistributionConfig {
                    beneficiaries: beneficiaries.clone(),
                };
                dist.validate()?;
                new_config.fee_distribution = Some(dist);
            }
            ConfigUpdate::SetMintFee { base, ratio } => {
                new_config.mint_fee_base = base.clone();
                new_config.mint_fee_ratio = ratio.clone();
            }
            ConfigUpdate::SetTokenCreationFee { fee } => {
                new_config.token_creation_fee = fee.clone();
            }
            ConfigUpdate::SetNftMintFee { fee } => {
                new_config.nft_mint_fee = fee.clone();
            }
            ConfigUpdate::SetNftFeeExemptTypes { types } => {
                new_config.nft_fee_exempt_types = types.clone();
            }
            ConfigUpdate::BatchUpdate(updates) => {
                for u in updates {
                    new_config = new_config.apply_update_inner(u, block_id, timestamp)?;
                }
            }
        }

        Ok(new_config)
    }

    /// Valide que coordinator_fee_bps + treasury_fee_bps = 10000 (100%).
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

    /// Validation globale de la config fee.
    /// - Si `fee_distribution` est active : valide la somme des bps
    /// - Sinon : valide coordinator_fee_bps + treasury_fee_bps
    /// - Si `fee_tiers` non vide : valide l'ordre et le catch-all
    pub fn validate_fee_config(&self) -> Result<(), String> {
        match &self.fee_distribution {
            Some(dist) => dist.validate()?,
            None => self.validate_fee_split()?,
        }
        if !self.fee_tiers.is_empty() {
            validate_fee_tiers(&self.fee_tiers)?;
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

    /// Définir un barème de fees par paliers (remplace fee_rate_bps si non vide)
    SetFeeTiers { tiers: Vec<FeeTier> },

    /// Supprimer les paliers (revenir au fee_rate_bps linéaire)
    ClearFeeTiers,

    /// Définir la distribution N-way des fees
    SetFeeDistribution { beneficiaries: Vec<crate::FeeBeneficiary> },

    /// Modifier les fees de minting (base fixe + ratio sur montant)
    SetMintFee { base: Option<String>, ratio: Option<String> },

    /// Modifier le fee de création de token
    SetTokenCreationFee { fee: Option<String> },

    /// Modifier le fee de mint NFT
    SetNftMintFee { fee: Option<String> },

    /// Modifier les types de NFT exemptés de fee
    SetNftFeeExemptTypes { types: Vec<String> },

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
            Self::SetFeeTiers { tiers } => format!("SetFeeTiers({} tiers)", tiers.len()),
            Self::ClearFeeTiers => "ClearFeeTiers".to_string(),
            Self::SetFeeDistribution { beneficiaries } => {
                format!("SetFeeDistribution({} beneficiaries)", beneficiaries.len())
            }
            Self::SetMintFee { base, ratio } => {
                format!("SetMintFee(base={:?}, ratio={:?})", base, ratio)
            }
            Self::SetTokenCreationFee { fee } => format!("SetTokenCreationFee({:?})", fee),
            Self::SetNftMintFee { fee } => format!("SetNftMintFee({:?})", fee),
            Self::SetNftFeeExemptTypes { types } => {
                format!("SetNftFeeExemptTypes({:?})", types)
            }
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

        let new_config = config.apply_update(&update, "block-001", 1234567890).unwrap();

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

        let new_config = config.apply_update(&update, "block-002", 1234567890).unwrap();

        assert_eq!(new_config.fee_rate_bps, 150);
        assert!(!new_config.mint_enabled);
    }

    #[test]
    fn test_default_fee_split_is_valid() {
        let config = RuntimeConfig::default();
        assert_eq!(config.coordinator_fee_bps, 6700);
        assert_eq!(config.treasury_fee_bps, 3300);
        assert!(config.validate_fee_split().is_ok());
    }

    #[test]
    fn test_invalid_fee_split() {
        let mut config = RuntimeConfig::default();
        config.coordinator_fee_bps = 5000;
        config.treasury_fee_bps = 3000;
        assert!(config.validate_fee_split().is_err());
    }

    #[test]
    fn test_apply_coordinator_fee_alone_breaks_split() {
        let config = RuntimeConfig::default();
        // Setting coordinator alone makes split invalid (7000 + 3300 != 10000)
        let result = config.apply_update(&ConfigUpdate::SetCoordinatorFee { bps: 7000 }, "block-003", 100);
        assert!(result.is_err());
    }

    #[test]
    fn test_apply_treasury_fee_alone_breaks_split() {
        let config = RuntimeConfig::default();
        // Setting treasury alone makes split invalid (6700 + 4000 != 10000)
        let result = config.apply_update(&ConfigUpdate::SetTreasuryFee { bps: 4000 }, "block-004", 100);
        assert!(result.is_err());
    }

    #[test]
    fn test_batch_update_coordinator_and_treasury() {
        let config = RuntimeConfig::default();
        let update = ConfigUpdate::BatchUpdate(vec![
            ConfigUpdate::SetCoordinatorFee { bps: 7000 },
            ConfigUpdate::SetTreasuryFee { bps: 3000 },
        ]);
        let new_config = config.apply_update(&update, "block-005", 100).unwrap();
        assert_eq!(new_config.coordinator_fee_bps, 7000);
        assert_eq!(new_config.treasury_fee_bps, 3000);
    }

    #[test]
    fn test_apply_set_fee_tiers() {
        let config = RuntimeConfig::default();
        let tiers = vec![
            FeeTier { up_to: Some("100".into()), ratio: "0.03".into() },
            FeeTier { up_to: None, ratio: "0.01".into() },
        ];
        let update = ConfigUpdate::SetFeeTiers { tiers: tiers.clone() };
        let new_config = config.apply_update(&update, "block-t1", 100).unwrap();

        assert_eq!(new_config.fee_tiers.len(), 2);
        assert_eq!(new_config.fee_tiers[0].ratio, "0.03");
        assert_eq!(new_config.fee_tiers[1].up_to, None);

        // ClearFeeTiers
        let cleared = new_config.apply_update(&ConfigUpdate::ClearFeeTiers, "block-t2", 200).unwrap();
        assert!(cleared.fee_tiers.is_empty());
    }

    #[test]
    fn test_apply_set_fee_distribution() {
        let config = RuntimeConfig::default();
        let update = ConfigUpdate::SetFeeDistribution {
            beneficiaries: vec![
                crate::FeeBeneficiary { role: "coordinator".into(), percent_bps: 5000, address: None },
                crate::FeeBeneficiary { role: "client".into(), percent_bps: 3000, address: Some("cli_addr".into()) },
                crate::FeeBeneficiary { role: "treasury".into(), percent_bps: 2000, address: None },
            ],
        };
        let new_config = config.apply_update(&update, "block-fd", 300).unwrap();

        let dist = new_config.fee_distribution.expect("should be Some");
        assert_eq!(dist.beneficiaries.len(), 3);
        assert_eq!(dist.beneficiaries[1].percent_bps, 3000);
    }

    #[test]
    fn test_apply_set_mint_fee() {
        let config = RuntimeConfig::default();
        let update = ConfigUpdate::SetMintFee {
            base: Some("0.5".into()),
            ratio: Some("0.01".into()),
        };
        let new_config = config.apply_update(&update, "block-mf", 400).unwrap();

        assert_eq!(new_config.mint_fee_base, Some("0.5".into()));
        assert_eq!(new_config.mint_fee_ratio, Some("0.01".into()));
    }

    #[test]
    fn test_apply_set_token_creation_fee() {
        let config = RuntimeConfig::default();
        let update = ConfigUpdate::SetTokenCreationFee { fee: Some("100".into()) };
        let new_config = config.apply_update(&update, "block-tcf", 500).unwrap();

        assert_eq!(new_config.token_creation_fee, Some("100".into()));
    }

    #[test]
    fn test_apply_set_nft_mint_fee() {
        let config = RuntimeConfig::default();
        let update = ConfigUpdate::SetNftMintFee { fee: Some("0.5".into()) };
        let new_config = config.apply_update(&update, "block-nmf", 600).unwrap();

        assert_eq!(new_config.nft_mint_fee, Some("0.5".into()));
    }

    #[test]
    fn test_apply_set_nft_fee_exempt_types() {
        let config = RuntimeConfig::default();
        let update = ConfigUpdate::SetNftFeeExemptTypes {
            types: vec!["cube".into(), "reward".into()],
        };
        let new_config = config.apply_update(&update, "block-nfe", 700).unwrap();

        assert_eq!(new_config.nft_fee_exempt_types.len(), 2);
        assert!(new_config.nft_fee_exempt_types.contains(&"cube".to_string()));
        assert!(new_config.nft_fee_exempt_types.contains(&"reward".to_string()));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Fee tiers validation
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_validate_fee_tiers_valid() {
        let tiers = vec![
            FeeTier { up_to: Some("100".into()), ratio: "0.03".into() },
            FeeTier { up_to: Some("10000".into()), ratio: "0.015".into() },
            FeeTier { up_to: None, ratio: "0.005".into() },
        ];
        assert!(validate_fee_tiers(&tiers).is_ok());
    }

    #[test]
    fn test_validate_fee_tiers_empty() {
        assert!(validate_fee_tiers(&[]).is_err());
    }

    #[test]
    fn test_validate_fee_tiers_no_catchall() {
        let tiers = vec![
            FeeTier { up_to: Some("100".into()), ratio: "0.03".into() },
        ];
        assert!(validate_fee_tiers(&tiers).is_err());
    }

    #[test]
    fn test_validate_fee_tiers_unordered() {
        let tiers = vec![
            FeeTier { up_to: Some("10000".into()), ratio: "0.03".into() },
            FeeTier { up_to: Some("100".into()), ratio: "0.015".into() },
            FeeTier { up_to: None, ratio: "0.005".into() },
        ];
        assert!(validate_fee_tiers(&tiers).is_err());
    }

    #[test]
    fn test_validate_fee_tiers_negative_ratio() {
        let tiers = vec![
            FeeTier { up_to: None, ratio: "-0.01".into() },
        ];
        assert!(validate_fee_tiers(&tiers).is_err());
    }

    #[test]
    fn test_validate_fee_tiers_invalid_decimal() {
        let tiers = vec![
            FeeTier { up_to: Some("abc".into()), ratio: "0.01".into() },
            FeeTier { up_to: None, ratio: "0.005".into() },
        ];
        assert!(validate_fee_tiers(&tiers).is_err());
    }

    #[test]
    fn test_set_fee_tiers_rejected_if_invalid() {
        let config = RuntimeConfig::default();
        let bad_tiers = vec![
            FeeTier { up_to: Some("100".into()), ratio: "0.03".into() },
            // Missing catch-all
        ];
        let result = config.apply_update(
            &ConfigUpdate::SetFeeTiers { tiers: bad_tiers },
            "block-bad",
            100,
        );
        assert!(result.is_err());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // validate_fee_config coherence
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_validate_fee_config_default_ok() {
        let config = RuntimeConfig::default();
        assert!(config.validate_fee_config().is_ok());
    }

    #[test]
    fn test_validate_fee_config_with_valid_distribution() {
        let mut config = RuntimeConfig::default();
        config.fee_distribution = Some(crate::FeeDistributionConfig::new(7000, 3000));
        assert!(config.validate_fee_config().is_ok());
    }

    #[test]
    fn test_validate_fee_config_with_invalid_distribution() {
        let mut config = RuntimeConfig::default();
        // Bypass eager validation by setting directly
        config.fee_distribution = Some(crate::FeeDistributionConfig::new(6000, 5000));
        assert!(config.validate_fee_config().is_err());
    }

    #[test]
    fn test_set_fee_distribution_invalid_rejected() {
        let config = RuntimeConfig::default();
        let result = config.apply_update(
            &ConfigUpdate::SetFeeDistribution {
                beneficiaries: vec![
                    crate::FeeBeneficiary { role: "coordinator".into(), percent_bps: 6000, address: None },
                    crate::FeeBeneficiary { role: "treasury".into(), percent_bps: 5000, address: None },
                ],
            },
            "block-bad",
            100,
        );
        assert!(result.is_err());
    }
}
