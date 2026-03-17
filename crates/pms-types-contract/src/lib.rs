use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// Contract — règle déclarative stockée dans RocksDB
// ─────────────────────────────────────────────────────────────────────────────

/// Un contrat déclaratif enregistré par le coordinator.
/// Évalué nativement par le ContractEngine lors des événements de trigger.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Contract {
    /// Identifiant unique (hash SHA-256 du contenu au moment de l'enregistrement)
    pub contract_id: String,
    /// Nom lisible (ex: "cube-burn-to-pms")
    pub name: String,
    /// Scope d'application : tous les ledgers ou un sous-ensemble
    pub scope: ContractScope,
    /// Événement déclencheur
    pub trigger: ContractTrigger,
    /// Actions à exécuter quand le trigger fire
    pub actions: Vec<ContractAction>,
    /// Actif ou non
    pub enabled: bool,
    /// Version du contrat (incrémentée lors de mises à jour)
    pub version: u32,
}

// ─────────────────────────────────────────────────────────────────────────────
// Scope
// ─────────────────────────────────────────────────────────────────────────────

/// Portée d'un contrat dans un engine multi-ledger.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContractScope {
    /// S'applique à tous les ledgers de l'engine.
    Global,
    /// S'applique uniquement aux ledgers listés.
    Ledger(Vec<String>),
}

impl ContractScope {
    /// Vérifie si le contrat s'applique au ledger donné.
    pub fn matches(&self, ledger_id: &str) -> bool {
        match self {
            ContractScope::Global => true,
            ContractScope::Ledger(ids) => ids.iter().any(|id| id == ledger_id),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Triggers
// ─────────────────────────────────────────────────────────────────────────────

/// Événement qui déclenche l'évaluation d'un contrat.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContractTrigger {
    /// Déclenché quand un NFT est burn.
    /// `nft_type`: filtre optionnel sur le champ `nft_type` des metadata.
    /// Si `None`, match tous les burns NFT.
    OnNftBurn { nft_type: Option<String> },

    /// Déclenché quand un token fungible est burn (UTXO burn).
    OnTokenBurn { asset_id: String },

    /// Déclenché lors d'un transfert de tokens (UTXO send).
    /// `asset_id`: filtre optionnel — `None` = tous les assets, `Some("edenite")` = EDN seulement.
    OnTransfer { asset_id: Option<String> },
}

// ─────────────────────────────────────────────────────────────────────────────
// Actions
// ─────────────────────────────────────────────────────────────────────────────

/// Action exécutée quand un contrat est déclenché.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContractAction {
    /// Accumule un refund dans le FeePool.
    /// Distribué au prochain cycle de fee distribution (Milestone/Reward).
    AccumulateRefund {
        /// Asset à créditer. `None` = PMS natif.
        asset_id: Option<String>,
        /// Formule de calcul du montant.
        formula: MintFormula,
    },

    /// Émet un événement (pour traitement off-chain, webhooks, etc.)
    EmitEvent { event_type: String },

    /// Prélève un frais de transfert au sender, réparti entre plusieurs bénéficiaires.
    /// Le frais est un ensemble de `TxOutput` additionnels dans la transaction (déductif, pas de mint).
    /// Évalué au moment de la préparation de la transaction.
    ///
    /// # Invariant
    /// La somme des `share_bps` de tous les splits DOIT être exactement 10 000 (100%).
    TransferFee {
        /// Formule de calcul du frais total.
        formula: TransferFeeFormula,
        /// Répartition du frais entre les bénéficiaires.
        /// Chaque split définit une adresse et sa part en basis points.
        splits: Vec<TransferFeeSplit>,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// TransferFeeSplit — part d'un frais de transfert
// ─────────────────────────────────────────────────────────────────────────────

/// Part d'un frais de transfert routée vers un bénéficiaire.
///
/// Utilisé dans [`ContractAction::TransferFee`] pour répartir le frais total
/// entre plusieurs wallets (créateur, treasury, partenaire, etc.).
///
/// # Invariant
/// La somme de `share_bps` sur tous les splits d'une action DOIT être exactement 10 000.
///
/// # Exemple
/// ```
/// use pms_types_contract::TransferFeeSplit;
///
/// let splits = vec![
///     TransferFeeSplit { address: "pms1creator".into(), share_bps: 6000 }, // 60%
///     TransferFeeSplit { address: "pms1treasury".into(), share_bps: 4000 }, // 40%
/// ];
/// assert_eq!(splits.iter().map(|s| s.share_bps).sum::<u32>(), 10_000);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransferFeeSplit {
    /// Adresse wallet du bénéficiaire (Bech32).
    pub address: String,
    /// Part du frais en basis points (sur 10 000). Ex: 6000 = 60%.
    pub share_bps: u32,
}

impl ContractAction {
    /// Valide une action de contrat.
    ///
    /// Pour `TransferFee`, vérifie que :
    /// - `splits` n'est pas vide
    /// - Chaque split a un `share_bps > 0` et une adresse non-vide
    /// - La somme des `share_bps` est exactement 10 000
    pub fn validate(&self) -> Result<(), String> {
        match self {
            ContractAction::TransferFee { splits, .. } => {
                if splits.is_empty() {
                    return Err("TransferFee: splits cannot be empty".into());
                }
                let total: u32 = splits.iter().map(|s| s.share_bps).sum();
                if total != 10_000 {
                    return Err(format!(
                        "TransferFee: splits share_bps must sum to 10000, got {total}"
                    ));
                }
                for (i, split) in splits.iter().enumerate() {
                    if split.share_bps == 0 {
                        return Err(format!(
                            "TransferFee: split[{i}] has zero share_bps"
                        ));
                    }
                    if split.address.trim().is_empty() {
                        return Err(format!(
                            "TransferFee: split[{i}] has empty address"
                        ));
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Formules de calcul
// ─────────────────────────────────────────────────────────────────────────────

/// Formule pour calculer le montant d'un mint/refund.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MintFormula {
    /// Taux de conversion fixe : `output = count * numerator / denominator`
    ///
    /// Exemple : 10 CUBE → 1 PMS = `{ rate_numerator: 1, rate_denominator: 10 }`
    FixedRate {
        rate_numerator: u64,
        rate_denominator: u64,
    },

    /// Formule basée sur les attributs du NFT (depuis metadata.extra JSON).
    ///
    /// `output = (attr[0] * attr[1] * ... * attr[n]) / divisor`
    ///
    /// Exemple (Edenite) : `weight * size * density / 19_300_000_000`
    AttributeFormula {
        attribute_names: Vec<String>,
        divisor: u64,
    },

    /// Montant fixe par item.
    FixedAmount { amount: String },
}

// ─────────────────────────────────────────────────────────────────────────────
// Formules de frais de transfert
// ─────────────────────────────────────────────────────────────────────────────

/// Formule pour calculer un frais de transfert prélevé au sender.
///
/// Contrairement à `MintFormula` (basée sur un count d'items), cette formule
/// opère sur le montant décimal du transfert.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransferFeeFormula {
    /// Pourcentage en basis points : `fee = amount * rate_bps / 10_000`.
    ///
    /// Exemple : 500 bps = 5% du montant transféré.
    PercentageBps { rate_bps: u32 },

    /// Montant fixe prélevé par transfert, indépendant du montant.
    FixedAmount { amount: String },
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scope_global_matches_any_ledger() {
        let scope = ContractScope::Global;
        assert!(scope.matches("main"));
        assert!(scope.matches("nft"));
        assert!(scope.matches("test-ledger"));
        println!("Global scope matches all ledgers: OK");
    }

    #[test]
    fn test_scope_ledger_matches_only_listed() {
        let scope = ContractScope::Ledger(vec!["main".into(), "trading".into()]);
        assert!(scope.matches("main"));
        assert!(scope.matches("trading"));
        assert!(!scope.matches("nft"));
        assert!(!scope.matches("other"));
        println!("Ledger scope matches only listed ledgers: OK");
    }

    #[test]
    fn test_scope_ledger_empty_matches_none() {
        let scope = ContractScope::Ledger(vec![]);
        assert!(!scope.matches("main"));
        assert!(!scope.matches(""));
        println!("Empty Ledger scope matches nothing: OK");
    }

    #[test]
    fn test_contract_serde_roundtrip() {
        let contract = Contract {
            contract_id: "abc123".into(),
            name: "cube-burn-to-pms".into(),
            scope: ContractScope::Ledger(vec!["main".into()]),
            trigger: ContractTrigger::OnNftBurn {
                nft_type: Some("cube".into()),
            },
            actions: vec![ContractAction::AccumulateRefund {
                asset_id: None,
                formula: MintFormula::FixedRate {
                    rate_numerator: 1,
                    rate_denominator: 10,
                },
            }],
            enabled: true,
            version: 1,
        };

        let json = serde_json::to_string_pretty(&contract).unwrap();
        println!("Serialized contract:\n{json}");

        let deserialized: Contract = serde_json::from_str(&json).unwrap();
        assert_eq!(contract, deserialized);
        println!("Roundtrip OK");
    }

    #[test]
    fn test_transfer_fee_contract_serde_roundtrip() {
        let contract = Contract {
            contract_id: "eden-transfer-fee".into(),
            name: "eden-transfer-fee".into(),
            scope: ContractScope::Ledger(vec!["eden".into()]),
            trigger: ContractTrigger::OnTransfer { asset_id: None },
            actions: vec![ContractAction::TransferFee {
                formula: TransferFeeFormula::PercentageBps { rate_bps: 500 },
                splits: vec![
                    TransferFeeSplit { address: "pms1creator".into(), share_bps: 6000 },
                    TransferFeeSplit { address: "pms1treasury".into(), share_bps: 4000 },
                ],
            }],
            enabled: true,
            version: 1,
        };

        let json = serde_json::to_string_pretty(&contract).unwrap();
        println!("TransferFee contract JSON:\n{json}");

        let deserialized: Contract = serde_json::from_str(&json).unwrap();
        assert_eq!(contract, deserialized);
        println!("TransferFee contract roundtrip OK");
    }

    #[test]
    fn test_transfer_fee_splits_validation() {
        // Valid: 2 splits summing to 10,000
        let action = ContractAction::TransferFee {
            formula: TransferFeeFormula::PercentageBps { rate_bps: 500 },
            splits: vec![
                TransferFeeSplit { address: "pms1a".into(), share_bps: 6000 },
                TransferFeeSplit { address: "pms1b".into(), share_bps: 4000 },
            ],
        };
        assert!(action.validate().is_ok());
        println!("Valid 60/40 splits: OK");

        // Invalid: sum != 10,000
        let bad_sum = ContractAction::TransferFee {
            formula: TransferFeeFormula::PercentageBps { rate_bps: 500 },
            splits: vec![
                TransferFeeSplit { address: "pms1a".into(), share_bps: 5000 },
                TransferFeeSplit { address: "pms1b".into(), share_bps: 4000 },
            ],
        };
        let err = bad_sum.validate().unwrap_err();
        println!("Sum=9000 rejected: {err}");
        assert!(err.contains("10000"));

        // Invalid: empty splits
        let empty = ContractAction::TransferFee {
            formula: TransferFeeFormula::PercentageBps { rate_bps: 500 },
            splits: vec![],
        };
        let err = empty.validate().unwrap_err();
        println!("Empty splits rejected: {err}");
        assert!(err.contains("empty"));

        // Invalid: zero share_bps
        let zero_share = ContractAction::TransferFee {
            formula: TransferFeeFormula::PercentageBps { rate_bps: 500 },
            splits: vec![
                TransferFeeSplit { address: "pms1a".into(), share_bps: 10_000 },
                TransferFeeSplit { address: "pms1b".into(), share_bps: 0 },
            ],
        };
        let err = zero_share.validate().unwrap_err();
        println!("Zero share_bps rejected: {err}");
        assert!(err.contains("zero"));

        // Invalid: empty address
        let empty_addr = ContractAction::TransferFee {
            formula: TransferFeeFormula::PercentageBps { rate_bps: 500 },
            splits: vec![
                TransferFeeSplit { address: "".into(), share_bps: 10_000 },
            ],
        };
        let err = empty_addr.validate().unwrap_err();
        println!("Empty address rejected: {err}");
        assert!(err.contains("empty address"));

        // AccumulateRefund always valid
        let refund = ContractAction::AccumulateRefund {
            asset_id: None,
            formula: MintFormula::FixedAmount { amount: "1".into() },
        };
        assert!(refund.validate().is_ok());
        println!("Non-TransferFee actions always valid: OK");
    }

    #[test]
    fn test_transfer_fee_formula_serde() {
        let pct = TransferFeeFormula::PercentageBps { rate_bps: 500 };
        let json = serde_json::to_string(&pct).unwrap();
        println!("PercentageBps JSON: {json}");
        let back: TransferFeeFormula = serde_json::from_str(&json).unwrap();
        assert_eq!(pct, back);

        let fixed = TransferFeeFormula::FixedAmount {
            amount: "2.5".into(),
        };
        let json = serde_json::to_string(&fixed).unwrap();
        println!("FixedAmount JSON: {json}");
        let back: TransferFeeFormula = serde_json::from_str(&json).unwrap();
        assert_eq!(fixed, back);
        println!("TransferFeeFormula serde roundtrip OK");
    }

    #[test]
    fn test_on_transfer_trigger_serde() {
        // Wildcard (any asset)
        let trigger = ContractTrigger::OnTransfer { asset_id: None };
        let json = serde_json::to_string(&trigger).unwrap();
        println!("OnTransfer wildcard JSON: {json}");
        let back: ContractTrigger = serde_json::from_str(&json).unwrap();
        assert_eq!(trigger, back);

        // Specific asset
        let trigger = ContractTrigger::OnTransfer {
            asset_id: Some("edenite".into()),
        };
        let json = serde_json::to_string(&trigger).unwrap();
        println!("OnTransfer edenite JSON: {json}");
        let back: ContractTrigger = serde_json::from_str(&json).unwrap();
        assert_eq!(trigger, back);
        println!("OnTransfer trigger serde roundtrip OK");
    }

    #[test]
    fn test_attribute_formula_serde() {
        let formula = MintFormula::AttributeFormula {
            attribute_names: vec!["weight".into(), "size".into(), "density".into()],
            divisor: 19_300_000_000,
        };

        let json = serde_json::to_string(&formula).unwrap();
        println!("AttributeFormula JSON: {json}");

        let back: MintFormula = serde_json::from_str(&json).unwrap();
        assert_eq!(formula, back);
        println!("AttributeFormula roundtrip OK");
    }
}
