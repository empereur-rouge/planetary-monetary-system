//! Contract Engine — évaluation des contrats déclaratifs.
//!
//! Évalue les contrats enregistrés lors des événements de trigger (NFT burn, etc.).
//! Les résultats sont des refunds à accumuler via le trait [`RefundSink`](crate::listener::RefundSink).

use pms_storage::ContractStorage;
use pms_types_contract::{ContractAction, MintFormula, TransferFeeFormula};
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

/// Résultat de l'évaluation d'un contrat de frais de transfert.
///
/// Produit par [`evaluate_transfer()`] — chaque résultat correspond à un
/// `TxOutput` additionnel à insérer dans la transaction (déductif).
#[derive(Debug, Clone)]
pub struct TransferFeeResult {
    /// ID du contrat qui a été déclenché
    pub contract_id: String,
    /// Nom du contrat
    pub contract_name: String,
    /// Adresse du bénéficiaire (créateur du ledger, treasury, etc.)
    pub beneficiary_address: String,
    /// Montant du frais prélevé (dans le même asset que le transfert)
    pub fee_amount: Decimal,
}

/// Évalue les contrats de frais de transfert pour un transfert donné.
///
/// Cherche les contrats avec trigger `OnTransfer` dont le scope inclut `ledger_id`.
/// Retourne les frais à ajouter comme `TxOutput` additionnels dans la transaction.
///
/// # Arguments
/// * `contract_store` - Store contenant les contrats enregistrés
/// * `ledger_id` - ID du ledger où le transfert a lieu
/// * `asset_id` - Asset transféré (`None` = PMS, `Some("edenite")` = EDN)
/// * `transfer_amount` - Montant du transfert (pour le calcul du pourcentage)
pub fn evaluate_transfer(
    contract_store: &dyn ContractStorage,
    ledger_id: &str,
    asset_id: Option<&str>,
    transfer_amount: Decimal,
) -> Vec<TransferFeeResult> {
    let contracts = match contract_store.find_transfer_contracts(asset_id, ledger_id) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("ContractEngine: failed to find transfer contracts: {e}");
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
                ContractAction::TransferFee {
                    formula,
                    splits,
                } => {
                    match evaluate_transfer_formula(formula, transfer_amount) {
                        Ok(total_fee) => {
                            if total_fee > Decimal::ZERO && !splits.is_empty() {
                                // Répartit le frais total entre les splits.
                                // Le dernier split récupère le reste pour éviter la poussière
                                // due aux arrondis (dust-free rounding).
                                let mut distributed = Decimal::ZERO;
                                let last_idx = splits.len() - 1;

                                for (i, split) in splits.iter().enumerate() {
                                    let split_amount = if i < last_idx {
                                        let a = (total_fee
                                            * Decimal::from(split.share_bps)
                                            / Decimal::from(10_000u32))
                                            .round_dp(8);
                                        distributed += a;
                                        a
                                    } else {
                                        // Dernier split : total_fee - sum(précédents)
                                        (total_fee - distributed).round_dp(8)
                                    };

                                    if split_amount > Decimal::ZERO {
                                        tracing::info!(
                                            "ContractEngine: transfer fee '{}' v{}: {} {} → {} to {} ({}bps)",
                                            contract.name,
                                            contract.version,
                                            transfer_amount,
                                            asset_id.unwrap_or("PMS"),
                                            split_amount,
                                            &split.address[..20.min(split.address.len())],
                                            split.share_bps,
                                        );
                                        results.push(TransferFeeResult {
                                            contract_id: contract.contract_id.clone(),
                                            contract_name: contract.name.clone(),
                                            beneficiary_address: split.address.clone(),
                                            fee_amount: split_amount,
                                        });
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!(
                                "ContractEngine: transfer fee formula failed for contract '{}': {e}",
                                contract.name
                            );
                        }
                    }
                }
                // Les autres actions (AccumulateRefund, EmitEvent) sont ignorées
                // sur un trigger OnTransfer — seul TransferFee est pertinent.
                _ => {}
            }
        }
    }

    results
}

/// Évalue une formule de frais de transfert sur un montant décimal.
fn evaluate_transfer_formula(
    formula: &TransferFeeFormula,
    amount: Decimal,
) -> anyhow::Result<Decimal> {
    match formula {
        TransferFeeFormula::PercentageBps { rate_bps } => {
            if *rate_bps > 10_000 {
                anyhow::bail!("PercentageBps: rate_bps {} exceeds 100%", rate_bps);
            }
            let fee = amount * Decimal::from(*rate_bps) / Decimal::from(10_000u32);
            Ok(fee.round_dp(8))
        }
        TransferFeeFormula::FixedAmount { amount: fixed } => {
            let fee = Decimal::from_str(fixed)
                .map_err(|e| anyhow::anyhow!("FixedAmount: invalid amount '{fixed}': {e}"))?;
            if fee < Decimal::ZERO {
                anyhow::bail!("FixedAmount: negative fee '{fixed}'");
            }
            Ok(fee.round_dp(8))
        }
    }
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
                // TransferFee n'est pas applicable aux burns NFT
                ContractAction::TransferFee { .. } => {}
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

    // ─── Transfer fee tests ─────────────────────────────────────────────

    fn make_transfer_fee_contract(
        scope: ContractScope,
        formula: TransferFeeFormula,
    ) -> Contract {
        Contract {
            contract_id: "eden-transfer-fee".into(),
            name: "eden-transfer-fee".into(),
            scope,
            trigger: ContractTrigger::OnTransfer { asset_id: None },
            actions: vec![ContractAction::TransferFee {
                formula,
                splits: vec![TransferFeeSplit {
                    address: "pms1creator".into(),
                    share_bps: 10_000,
                }],
            }],
            enabled: true,
            version: 1,
        }
    }

    #[test]
    fn test_transfer_fee_percentage() {
        let store = InMemoryContractStore::new();
        store
            .put_contract(&make_transfer_fee_contract(
                ContractScope::Ledger(vec!["eden".into()]),
                TransferFeeFormula::PercentageBps { rate_bps: 500 }, // 5%
            ))
            .unwrap();

        let results = evaluate_transfer(
            &store,
            "eden",
            Some("edenite"),
            Decimal::from(100),
        );

        println!("Transfer fee results: {results:?}");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fee_amount, Decimal::from(5)); // 5% of 100
        assert_eq!(results[0].beneficiary_address, "pms1creator");
        println!(
            "5% transfer fee on 100 EDN → {} fee: OK",
            results[0].fee_amount
        );
    }

    #[test]
    fn test_transfer_fee_fixed_amount() {
        let store = InMemoryContractStore::new();
        store
            .put_contract(&make_transfer_fee_contract(
                ContractScope::Global,
                TransferFeeFormula::FixedAmount {
                    amount: "2.5".into(),
                },
            ))
            .unwrap();

        let results = evaluate_transfer(
            &store,
            "eden",
            Some("edenite"),
            Decimal::from(100),
        );

        println!("Fixed transfer fee results: {results:?}");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fee_amount, Decimal::from_str("2.5").unwrap());
        println!(
            "Fixed 2.5 transfer fee → {}: OK",
            results[0].fee_amount
        );
    }

    #[test]
    fn test_transfer_fee_no_matching_contract() {
        let store = InMemoryContractStore::new();
        // Contract on eden, but we transfer on main
        store
            .put_contract(&make_transfer_fee_contract(
                ContractScope::Ledger(vec!["eden".into()]),
                TransferFeeFormula::PercentageBps { rate_bps: 500 },
            ))
            .unwrap();

        let results = evaluate_transfer(
            &store,
            "main",
            None,
            Decimal::from(100),
        );

        println!("No matching contract results: {results:?}");
        assert!(results.is_empty());
        println!("No transfer fee on wrong ledger: OK");
    }

    #[test]
    fn test_transfer_fee_scope_filter() {
        let store = InMemoryContractStore::new();
        store
            .put_contract(&make_transfer_fee_contract(
                ContractScope::Ledger(vec!["eden".into()]),
                TransferFeeFormula::PercentageBps { rate_bps: 300 }, // 3%
            ))
            .unwrap();

        // Should fire on eden
        let r1 = evaluate_transfer(&store, "eden", Some("edenite"), Decimal::from(200));
        assert_eq!(r1.len(), 1);
        assert_eq!(r1[0].fee_amount, Decimal::from(6)); // 3% of 200
        println!("Transfer fee fires on eden: {} fee: OK", r1[0].fee_amount);

        // Should NOT fire on main
        let r2 = evaluate_transfer(&store, "main", None, Decimal::from(200));
        assert!(r2.is_empty());
        println!("Transfer fee silent on main: OK");
    }

    #[test]
    fn test_transfer_fee_asset_filter() {
        let store = InMemoryContractStore::new();
        // Contract only for "edenite" transfers
        let contract = Contract {
            contract_id: "edn-only-fee".into(),
            name: "edn-only-fee".into(),
            scope: ContractScope::Ledger(vec!["eden".into()]),
            trigger: ContractTrigger::OnTransfer {
                asset_id: Some("edenite".into()),
            },
            actions: vec![ContractAction::TransferFee {
                formula: TransferFeeFormula::PercentageBps { rate_bps: 500 },
                splits: vec![TransferFeeSplit {
                    address: "pms1creator".into(),
                    share_bps: 10_000,
                }],
            }],
            enabled: true,
            version: 1,
        };
        store.put_contract(&contract).unwrap();

        // EDN transfer → should match
        let r1 = evaluate_transfer(&store, "eden", Some("edenite"), Decimal::from(100));
        assert_eq!(r1.len(), 1);
        println!("Asset-specific contract matches EDN: OK");

        // PMS transfer → should NOT match
        let r2 = evaluate_transfer(&store, "eden", None, Decimal::from(100));
        assert!(r2.is_empty());
        println!("Asset-specific contract ignores PMS: OK");
    }

    #[test]
    fn test_transfer_fee_disabled_contract() {
        let store = InMemoryContractStore::new();
        let mut contract = make_transfer_fee_contract(
            ContractScope::Global,
            TransferFeeFormula::PercentageBps { rate_bps: 500 },
        );
        contract.enabled = false;
        store.put_contract(&contract).unwrap();

        let results = evaluate_transfer(&store, "eden", Some("edenite"), Decimal::from(100));
        assert!(results.is_empty());
        println!("Disabled transfer fee contract ignored: OK");
    }

    #[test]
    fn test_transfer_fee_formula_edge_cases() {
        // Zero amount → zero fee
        let fee = evaluate_transfer_formula(
            &TransferFeeFormula::PercentageBps { rate_bps: 500 },
            Decimal::ZERO,
        )
        .unwrap();
        assert_eq!(fee, Decimal::ZERO);
        println!("Zero amount → zero fee: OK");

        // Very small amount → rounded
        let fee = evaluate_transfer_formula(
            &TransferFeeFormula::PercentageBps { rate_bps: 100 }, // 1%
            Decimal::from_str("0.001").unwrap(),
        )
        .unwrap();
        println!("1% of 0.001 = {fee}");
        assert_eq!(fee, Decimal::from_str("0.00001").unwrap());
        println!("Small amount fee rounded correctly: OK");

        // rate_bps > 10000 → error
        let result = evaluate_transfer_formula(
            &TransferFeeFormula::PercentageBps { rate_bps: 15000 },
            Decimal::from(100),
        );
        assert!(result.is_err());
        println!("rate_bps > 10000 rejected: OK");
    }

    // ─── Multi-split tests ──────────────────────────────────────────────

    #[test]
    fn test_transfer_fee_two_splits() {
        let store = InMemoryContractStore::new();
        let contract = Contract {
            contract_id: "eden-split-fee".into(),
            name: "eden-split-fee".into(),
            scope: ContractScope::Ledger(vec!["eden".into()]),
            trigger: ContractTrigger::OnTransfer { asset_id: None },
            actions: vec![ContractAction::TransferFee {
                formula: TransferFeeFormula::PercentageBps { rate_bps: 500 }, // 5%
                splits: vec![
                    TransferFeeSplit { address: "pms1creator".into(), share_bps: 6000 },  // 60%
                    TransferFeeSplit { address: "pms1treasury".into(), share_bps: 4000 }, // 40%
                ],
            }],
            enabled: true,
            version: 1,
        };
        store.put_contract(&contract).unwrap();

        let results = evaluate_transfer(
            &store,
            "eden",
            Some("edenite"),
            Decimal::from(100),
        );

        println!("Two-split results: {results:?}");
        assert_eq!(results.len(), 2);
        // 5% of 100 = 5 total fee
        // 60% of 5 = 3.0 → creator
        // 40% of 5 = 2.0 → treasury
        assert_eq!(results[0].beneficiary_address, "pms1creator");
        assert_eq!(results[0].fee_amount, Decimal::from(3));
        assert_eq!(results[1].beneficiary_address, "pms1treasury");
        assert_eq!(results[1].fee_amount, Decimal::from(2));
        let total: Decimal = results.iter().map(|r| r.fee_amount).sum();
        assert_eq!(total, Decimal::from(5));
        println!("60/40 split of 5 fee: {} + {} = {}: OK",
            results[0].fee_amount, results[1].fee_amount, total);
    }

    #[test]
    fn test_transfer_fee_three_splits_dust_free() {
        let store = InMemoryContractStore::new();
        let contract = Contract {
            contract_id: "three-way-fee".into(),
            name: "three-way-fee".into(),
            scope: ContractScope::Global,
            trigger: ContractTrigger::OnTransfer { asset_id: None },
            actions: vec![ContractAction::TransferFee {
                formula: TransferFeeFormula::PercentageBps { rate_bps: 1000 }, // 10%
                splits: vec![
                    TransferFeeSplit { address: "pms1a".into(), share_bps: 3333 },
                    TransferFeeSplit { address: "pms1b".into(), share_bps: 3333 },
                    TransferFeeSplit { address: "pms1c".into(), share_bps: 3334 },
                ],
            }],
            enabled: true,
            version: 1,
        };
        store.put_contract(&contract).unwrap();

        // 10% of 7 = 0.7 total fee
        // 3333/10000 * 0.7 = 0.23331 → rounded 0.23331
        // 3333/10000 * 0.7 = 0.23331 → rounded 0.23331
        // last = 0.7 - 0.23331 - 0.23331 = 0.23338
        let results = evaluate_transfer(&store, "eden", None, Decimal::from(7));

        println!("Three-split results: {results:?}");
        assert_eq!(results.len(), 3);
        let total: Decimal = results.iter().map(|r| r.fee_amount).sum();
        assert_eq!(total, Decimal::from_str("0.7").unwrap(),
            "Dust-free: sum of splits must equal total fee exactly");
        println!("Three-way split of 0.7 fee is dust-free: {total}: OK");
    }

    #[test]
    fn test_transfer_fee_single_split_backward_compat() {
        let store = InMemoryContractStore::new();
        store
            .put_contract(&make_transfer_fee_contract(
                ContractScope::Ledger(vec!["eden".into()]),
                TransferFeeFormula::PercentageBps { rate_bps: 500 },
            ))
            .unwrap();

        let results = evaluate_transfer(
            &store,
            "eden",
            Some("edenite"),
            Decimal::from(100),
        );

        println!("Single split (backward compat) results: {results:?}");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fee_amount, Decimal::from(5));
        assert_eq!(results[0].beneficiary_address, "pms1creator");
        println!("Single split 100% → {} fee: OK", results[0].fee_amount);
    }
}
