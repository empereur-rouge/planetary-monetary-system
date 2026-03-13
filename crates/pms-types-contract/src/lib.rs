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
