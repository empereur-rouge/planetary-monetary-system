//! Implémentation RocksDB de ContractStorage.
//!
//! ## Schéma du Column Family:
//! - `contracts`: `contract_id` -> JSON sérialisé de `Contract`
//!
//! Tous les contrats sont stockés dans une seule CF.
//! La recherche par trigger est faite par scan complet (nombre de contrats faible).

use crate::{ContractStorage, rocks_store::store::RocksStore};
use anyhow::Result;
use pms_types_contract::Contract;

impl ContractStorage for RocksStore {
    fn get_contract(&self, contract_id: &str) -> Result<Option<Contract>> {
        let cf = self.cf("contracts");
        if let Some(v) = self.db.get_cf(&cf, contract_id.as_bytes())? {
            let contract: Contract = serde_json::from_slice(&v)?;
            Ok(Some(contract))
        } else {
            Ok(None)
        }
    }

    fn put_contract(&self, contract: &Contract) -> Result<()> {
        let cf = self.cf("contracts");
        let json = serde_json::to_vec(contract)?;
        self.db
            .put_cf(&cf, contract.contract_id.as_bytes(), &json)?;
        Ok(())
    }

    fn list_contracts(&self) -> Result<Vec<Contract>> {
        let cf = self.cf("contracts");
        let mut contracts = Vec::new();
        for kv in self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start) {
            let (_k, v) = kv?;
            let contract: Contract = serde_json::from_slice(&v)?;
            contracts.push(contract);
        }
        Ok(contracts)
    }

    fn find_nft_burn_contracts(
        &self,
        nft_type: Option<&str>,
        ledger_id: &str,
    ) -> Result<Vec<Contract>> {
        let cf = self.cf("contracts");
        let mut results = Vec::new();
        for kv in self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start) {
            let (_k, v) = kv?;
            let contract: Contract = serde_json::from_slice(&v)?;

            if !contract.enabled {
                continue;
            }
            if !contract.scope.matches(ledger_id) {
                continue;
            }
            match &contract.trigger {
                pms_types_contract::ContractTrigger::OnNftBurn {
                    nft_type: filter, ..
                } => {
                    match filter {
                        None => results.push(contract), // wildcard
                        Some(f) => {
                            if nft_type.is_some_and(|t| t == f) {
                                results.push(contract);
                            }
                        }
                    }
                }
                _ => continue,
            }
        }
        Ok(results)
    }

    fn set_enabled(&self, contract_id: &str, enabled: bool) -> Result<()> {
        let cf = self.cf("contracts");
        if let Some(v) = self.db.get_cf(&cf, contract_id.as_bytes())? {
            let mut contract: Contract = serde_json::from_slice(&v)?;
            contract.enabled = enabled;
            let json = serde_json::to_vec(&contract)?;
            self.db
                .put_cf(&cf, contract_id.as_bytes(), &json)?;
        }
        Ok(())
    }
}
