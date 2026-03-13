//! Contract Engine — évaluation des contrats déclaratifs.
//!
//! Évalue les contrats enregistrés lors des événements de trigger (NFT burn, etc.).
//! Les résultats sont des refunds à accumuler dans le FeePool existant.

use pms_storage::ContractStorage;
use pms_types_contract::{ContractAction, MintFormula};
use pms_types_nft::NftMetadata;
use rust_decimal::Decimal;
use std::str::FromStr;

/// Résultat de l'exécution d'un contrat.
#[derive(Debug, Clone)]
pub struct ContractResult {
    /// ID du contrat qui a été déclenché
    pub contract_id: String,
    /// Nom du contrat
    pub contract_name: String,
    /// Adresse du wallet qui reçoit le refund
    pub refund_address: String,
    /// Montant du refund
    pub refund_amount: Decimal,
    /// Asset du refund (None = PMS natif)
    pub asset_id: Option<String>,
    /// Détails de l'exécution (pour logging/audit)
    pub details: String,
}

/// Évalue les contrats NFT burn pour un burn donné.
///
/// Cherche les contrats Global + ceux dont le scope inclut ce `ledger_id`.
/// Retourne les refunds à accumuler dans le FeePool.
///
/// # Arguments
/// * `contract_store` - Store contenant les contrats enregistrés
/// * `ledger_id` - ID du ledger où le burn a eu lieu
/// * `burner_address` - Adresse du wallet qui a burn le NFT
/// * `nft_type` - Type du NFT (from metadata.nft_type)
/// * `nft_metadata` - Metadata du NFT burn (pour AttributeFormula)
/// * `token_count` - Nombre de NFTs burn (1 pour single, N pour batch)
pub fn evaluate_nft_burn(
    contract_store: &dyn ContractStorage,
    ledger_id: &str,
    burner_address: &str,
    nft_type: Option<&str>,
    nft_metadata: Option<&NftMetadata>,
    token_count: u64,
) -> Vec<ContractResult> {
    let contracts = match contract_store.find_nft_burn_contracts(nft_type, ledger_id) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("ContractEngine: failed to find contracts: {e}");
            return vec![];
        }
    };

    if contracts.is_empty() {
        return vec![];
    }

    let mut results = Vec::new();

    for contract in &contracts {
        for action in &contract.actions {
            match action {
                ContractAction::AccumulateRefund { asset_id, formula } => {
                    match evaluate_formula(formula, token_count, nft_metadata) {
                        Ok(amount) => {
                            if amount > Decimal::ZERO {
                                let details = format!(
                                    "Contract '{}' v{}: {} NFT(s) burned → {} {} refund",
                                    contract.name,
                                    contract.version,
                                    token_count,
                                    amount,
                                    asset_id.as_deref().unwrap_or("PMS"),
                                );
                                tracing::info!("ContractEngine: {details}");
                                results.push(ContractResult {
                                    contract_id: contract.contract_id.clone(),
                                    contract_name: contract.name.clone(),
                                    refund_address: burner_address.to_string(),
                                    refund_amount: amount,
                                    asset_id: asset_id.clone(),
                                    details,
                                });
                            }
                        }
                        Err(e) => {
                            tracing::error!(
                                "ContractEngine: formula evaluation failed for contract '{}': {e}",
                                contract.name
                            );
                        }
                    }
                }
                ContractAction::EmitEvent { event_type } => {
                    tracing::info!(
                        "ContractEngine: event '{}' from contract '{}'",
                        event_type,
                        contract.name
                    );
                }
            }
        }
    }

    results
}

/// Évalue une formule de calcul de montant.
fn evaluate_formula(
    formula: &MintFormula,
    count: u64,
    metadata: Option<&NftMetadata>,
) -> anyhow::Result<Decimal> {
    match formula {
        MintFormula::FixedRate {
            rate_numerator,
            rate_denominator,
        } => {
            if *rate_denominator == 0 {
                anyhow::bail!("FixedRate: rate_denominator is zero");
            }
            let result = Decimal::from(count) * Decimal::from(*rate_numerator)
                / Decimal::from(*rate_denominator);
            Ok(result.round_dp(8))
        }

        MintFormula::AttributeFormula {
            attribute_names,
            divisor,
        } => {
            if *divisor == 0 {
                anyhow::bail!("AttributeFormula: divisor is zero");
            }
            let extra_json = metadata
                .and_then(|m| m.extra.as_deref())
                .unwrap_or("{}");

            let extra: serde_json::Value = serde_json::from_str(extra_json)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

            // Recherche les attributs. Supporte un niveau de nesting ("attributes.weight")
            let mut product = Decimal::ONE;
            for name in attribute_names {
                let val = find_attribute(&extra, name).ok_or_else(|| {
                    anyhow::anyhow!("Attribute '{name}' not found in NFT metadata extra")
                })?;
                product *= val;
            }

            let result = product * Decimal::from(count) / Decimal::from(*divisor);
            Ok(result.round_dp(8))
        }

        MintFormula::FixedAmount { amount } => {
            let per_item = Decimal::from_str(amount)
                .map_err(|e| anyhow::anyhow!("FixedAmount: invalid amount '{amount}': {e}"))?;
            Ok((per_item * Decimal::from(count)).round_dp(8))
        }
    }
}

/// Cherche un attribut dans un JSON Value.
/// Supporte le dot-notation : "attributes.weight" cherche extra["attributes"]["weight"].
fn find_attribute(value: &serde_json::Value, path: &str) -> Option<Decimal> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = value;

    for part in &parts {
        current = current.get(part)?;
    }

    // Convertir en Decimal (supporte int, float, string)
    match current {
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(Decimal::from(i))
            } else if let Some(f) = n.as_f64() {
                Decimal::try_from(f).ok()
            } else {
                None
            }
        }
        serde_json::Value::String(s) => Decimal::from_str(s).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pms_storage::InMemoryContractStore;
    use pms_types_contract::*;

    fn make_cube_contract(scope: ContractScope) -> Contract {
        Contract {
            contract_id: "cube-burn-pms".into(),
            name: "cube-burn-to-pms".into(),
            scope,
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
        }
    }

    #[test]
    fn test_fixed_rate_single_burn() {
        let store = InMemoryContractStore::new();
        store.put_contract(&make_cube_contract(ContractScope::Global)).unwrap();

        let results =
            evaluate_nft_burn(&store, "main", "pms1alice", Some("cube"), None, 1);

        println!("Results: {results:?}");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].refund_amount, Decimal::from_str("0.1").unwrap());
        assert_eq!(results[0].refund_address, "pms1alice");
        assert!(results[0].asset_id.is_none()); // PMS natif
        println!(
            "Single cube burn → {} PMS refund: OK",
            results[0].refund_amount
        );
    }

    #[test]
    fn test_fixed_rate_batch_burn() {
        let store = InMemoryContractStore::new();
        store.put_contract(&make_cube_contract(ContractScope::Global)).unwrap();

        let results =
            evaluate_nft_burn(&store, "main", "pms1alice", Some("cube"), None, 10);

        println!("Results: {results:?}");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].refund_amount, Decimal::from(1));
        println!(
            "Batch 10 cubes → {} PMS refund: OK",
            results[0].refund_amount
        );
    }

    #[test]
    fn test_no_matching_contract() {
        let store = InMemoryContractStore::new();
        store.put_contract(&make_cube_contract(ContractScope::Global)).unwrap();

        // NFT type "ticket" → pas de match avec le contrat "cube"
        let results =
            evaluate_nft_burn(&store, "main", "pms1alice", Some("ticket"), None, 1);

        println!("Results for 'ticket' type: {results:?}");
        assert!(results.is_empty());
        println!("No match for wrong nft_type: OK");
    }

    #[test]
    fn test_disabled_contract_ignored() {
        let store = InMemoryContractStore::new();
        let mut c = make_cube_contract(ContractScope::Global);
        c.enabled = false;
        store.put_contract(&c).unwrap();

        let results =
            evaluate_nft_burn(&store, "main", "pms1alice", Some("cube"), None, 1);

        println!("Results with disabled contract: {results:?}");
        assert!(results.is_empty());
        println!("Disabled contract ignored: OK");
    }

    #[test]
    fn test_scope_ledger_filter() {
        let store = InMemoryContractStore::new();
        store.put_contract(&make_cube_contract(ContractScope::Ledger(vec![
            "main".into(),
        ]))).unwrap();

        // Match sur le bon ledger
        let results =
            evaluate_nft_burn(&store, "main", "pms1alice", Some("cube"), None, 1);
        assert_eq!(results.len(), 1);
        println!("Contract fires on matching ledger: OK");

        // Pas de match sur un autre ledger
        let results =
            evaluate_nft_burn(&store, "nft", "pms1alice", Some("cube"), None, 1);
        assert!(results.is_empty());
        println!("Contract silent on non-matching ledger: OK");
    }

    #[test]
    fn test_attribute_formula() {
        let store = InMemoryContractStore::new();
        let contract = Contract {
            contract_id: "edenite-formula".into(),
            name: "cube-burn-edenite".into(),
            scope: ContractScope::Global,
            trigger: ContractTrigger::OnNftBurn {
                nft_type: Some("cube".into()),
            },
            actions: vec![ContractAction::AccumulateRefund {
                asset_id: Some("edenite".into()),
                formula: MintFormula::AttributeFormula {
                    attribute_names: vec![
                        "attributes.weight".into(),
                        "attributes.size".into(),
                        "attributes.density".into(),
                    ],
                    divisor: 1_000_000, // Simplified divisor for test
                },
            }],
            enabled: true,
            version: 1,
        };
        store.put_contract(&contract).unwrap();

        let metadata = NftMetadata {
            name: Some("Cube #1".into()),
            description: None,
            uri: None,
            nft_type: Some("cube".into()),
            extra: Some(
                r#"{"attributes":{"weight":1000,"size":50,"density":80},"rarity":"common"}"#.into(),
            ),
        };

        let results = evaluate_nft_burn(
            &store,
            "main",
            "pms1alice",
            Some("cube"),
            Some(&metadata),
            1,
        );

        println!("Attribute formula results: {results:?}");
        assert_eq!(results.len(), 1);
        // 1000 * 50 * 80 / 1_000_000 = 4.0
        assert_eq!(results[0].refund_amount, Decimal::from(4));
        assert_eq!(results[0].asset_id.as_deref(), Some("edenite"));
        println!(
            "Attribute formula: {} edenite: OK",
            results[0].refund_amount
        );
    }

    #[test]
    fn test_fixed_amount_formula() {
        let result = evaluate_formula(
            &MintFormula::FixedAmount {
                amount: "5.5".into(),
            },
            3,
            None,
        )
        .unwrap();

        println!("FixedAmount 5.5 * 3 = {result}");
        assert_eq!(result, Decimal::from_str("16.5").unwrap());
        println!("FixedAmount formula: OK");
    }

    #[test]
    fn test_zero_denominator_rejected() {
        let result = evaluate_formula(
            &MintFormula::FixedRate {
                rate_numerator: 1,
                rate_denominator: 0,
            },
            1,
            None,
        );

        println!("Zero denominator result: {result:?}");
        assert!(result.is_err());
        println!("Zero denominator correctly rejected: OK");
    }

    #[test]
    fn test_wildcard_nft_type_matches_all() {
        let store = InMemoryContractStore::new();
        let contract = Contract {
            contract_id: "wildcard".into(),
            name: "all-burns-rebate".into(),
            scope: ContractScope::Global,
            trigger: ContractTrigger::OnNftBurn { nft_type: None }, // Wildcard
            actions: vec![ContractAction::AccumulateRefund {
                asset_id: None,
                formula: MintFormula::FixedAmount {
                    amount: "0.01".into(),
                },
            }],
            enabled: true,
            version: 1,
        };
        store.put_contract(&contract).unwrap();

        // Should match any nft_type
        let r1 = evaluate_nft_burn(&store, "main", "pms1a", Some("cube"), None, 1);
        let r2 = evaluate_nft_burn(&store, "main", "pms1a", Some("ticket"), None, 1);
        let r3 = evaluate_nft_burn(&store, "main", "pms1a", None, None, 1);

        assert_eq!(r1.len(), 1);
        assert_eq!(r2.len(), 1);
        assert_eq!(r3.len(), 1);
        println!("Wildcard matches all nft_types: OK");
    }
}
