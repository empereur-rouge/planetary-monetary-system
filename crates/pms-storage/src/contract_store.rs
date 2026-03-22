//! Stockage des contrats déclaratifs (smart contracts rule-based).
//!
//! Ce module définit le trait `ContractStorage` pour le CRUD et la recherche
//! de contrats enregistrés par le coordinator dans le DAG.
//!
//! ## Modèle de données
//! - `contract_id` → `Contract` (JSON sérialisé)
//! - Recherche par trigger type + nft_type + ledger scope

use anyhow::Result;
use pms_types_contract::Contract;

/// Trait pour le stockage des contrats déclaratifs.
///
/// Abstraction permettant différentes implémentations (RocksDB, in-memory, etc.).
pub trait ContractStorage: Send + Sync {
    /// Récupère un contrat par son ID.
    fn get_contract(&self, contract_id: &str) -> Result<Option<Contract>>;

    /// Enregistre ou met à jour un contrat.
    fn put_contract(&self, contract: &Contract) -> Result<()>;

    /// Liste tous les contrats enregistrés.
    fn list_contracts(&self) -> Result<Vec<Contract>>;

    /// Recherche les contrats activés qui matchent un trigger de type NFT burn.
    ///
    /// Retourne les contrats dont :
    /// - `enabled == true`
    /// - `scope` matche le `ledger_id` (Global ou Ledger contenant ce ledger)
    /// - `trigger` est `OnNftBurn` avec `nft_type` matching (None = wildcard)
    fn find_nft_burn_contracts(
        &self,
        nft_type: Option<&str>,
        ledger_id: &str,
    ) -> Result<Vec<Contract>>;

    /// Recherche les contrats activés qui matchent un trigger de type transfert.
    ///
    /// Retourne les contrats dont :
    /// - `enabled == true`
    /// - `scope` matche le `ledger_id`
    /// - `trigger` est `OnTransfer` avec `asset_id` matching (None = wildcard)
    fn find_transfer_contracts(
        &self,
        asset_id: Option<&str>,
        ledger_id: &str,
    ) -> Result<Vec<Contract>>;

    /// Met à jour un contrat existant (actions, scope, enabled, etc.).
    ///
    /// Le `contract_id` doit correspondre à un contrat existant.
    /// Remplace le contrat complet par les données fournies.
    fn update_contract(&self, contract_id: &str, contract: &Contract) -> Result<()>;

    /// Active ou désactive un contrat.
    fn set_enabled(&self, contract_id: &str, enabled: bool) -> Result<()>;
}

// ─────────────────────────────────────────────────────────────────────────────
// In-memory implementation (for tests)
// ─────────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;
use std::sync::RwLock;

/// Implémentation en mémoire pour les tests unitaires.
pub struct InMemoryContractStore {
    contracts: RwLock<HashMap<String, Contract>>,
}

impl InMemoryContractStore {
    pub fn new() -> Self {
        Self {
            contracts: RwLock::new(HashMap::new()),
        }
    }
}

impl ContractStorage for InMemoryContractStore {
    fn get_contract(&self, contract_id: &str) -> Result<Option<Contract>> {
        let map = self.contracts.read().unwrap_or_else(|p| p.into_inner());
        Ok(map.get(contract_id).cloned())
    }

    fn put_contract(&self, contract: &Contract) -> Result<()> {
        let mut map = self.contracts.write().unwrap_or_else(|p| p.into_inner());
        map.insert(contract.contract_id.clone(), contract.clone());
        Ok(())
    }

    fn list_contracts(&self) -> Result<Vec<Contract>> {
        let map = self.contracts.read().unwrap_or_else(|p| p.into_inner());
        Ok(map.values().cloned().collect())
    }

    fn find_nft_burn_contracts(
        &self,
        nft_type: Option<&str>,
        ledger_id: &str,
    ) -> Result<Vec<Contract>> {
        let map = self.contracts.read().unwrap_or_else(|p| p.into_inner());
        let results = map
            .values()
            .filter(|c| {
                if !c.enabled {
                    return false;
                }
                if !c.scope.matches(ledger_id) {
                    return false;
                }
                match &c.trigger {
                    pms_types_contract::ContractTrigger::OnNftBurn {
                        nft_type: filter, ..
                    } => {
                        // Si le contrat filtre sur un nft_type, il doit matcher
                        match filter {
                            None => true, // wildcard: match tous les burns
                            Some(f) => nft_type.is_some_and(|t| t == f),
                        }
                    }
                    _ => false,
                }
            })
            .cloned()
            .collect();
        Ok(results)
    }

    fn find_transfer_contracts(
        &self,
        asset_id: Option<&str>,
        ledger_id: &str,
    ) -> Result<Vec<Contract>> {
        let map = self.contracts.read().unwrap_or_else(|p| p.into_inner());
        let results = map
            .values()
            .filter(|c| {
                if !c.enabled {
                    return false;
                }
                if !c.scope.matches(ledger_id) {
                    return false;
                }
                match &c.trigger {
                    pms_types_contract::ContractTrigger::OnTransfer {
                        asset_id: filter,
                    } => match filter {
                        None => true, // wildcard: match tous les transferts
                        Some(f) => asset_id.is_some_and(|a| a == f),
                    },
                    _ => false,
                }
            })
            .cloned()
            .collect();
        Ok(results)
    }

    fn update_contract(&self, contract_id: &str, contract: &Contract) -> Result<()> {
        let mut map = self.contracts.write().unwrap_or_else(|p| p.into_inner());
        if !map.contains_key(contract_id) {
            anyhow::bail!("Contract '{}' not found", contract_id);
        }
        map.insert(contract_id.to_string(), contract.clone());
        Ok(())
    }

    fn set_enabled(&self, contract_id: &str, enabled: bool) -> Result<()> {
        let mut map = self.contracts.write().unwrap_or_else(|p| p.into_inner());
        if let Some(c) = map.get_mut(contract_id) {
            c.enabled = enabled;
        }
        Ok(())
    }
}
