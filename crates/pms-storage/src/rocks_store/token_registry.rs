use crate::rocks_store::store::RocksStore;
use anyhow::Result;
use pms_types_payload::TokenMetadata;
use rocksdb::BoundColumnFamily;
use std::sync::Arc;

impl RocksStore {
    fn cf_token_registry(&self) -> Arc<BoundColumnFamily<'_>> {
        self.cf("token_registry")
    }

    /// Validates token metadata format before registration.
    fn validate_token_metadata(metadata: &TokenMetadata) -> Result<()> {
        // asset_id: alphanumeric + underscore/hyphen, 1-64 chars
        if metadata.asset_id.is_empty() || metadata.asset_id.len() > 64 {
            anyhow::bail!(
                "asset_id must be 1-64 characters, got {}",
                metadata.asset_id.len()
            );
        }
        if !metadata
            .asset_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            anyhow::bail!(
                "asset_id must be alphanumeric/underscore/hyphen: {}",
                metadata.asset_id
            );
        }

        // symbol: 1-10 chars
        if metadata.symbol.is_empty() || metadata.symbol.len() > 10 {
            anyhow::bail!(
                "symbol must be 1-10 characters, got {}",
                metadata.symbol.len()
            );
        }

        // name: 1-128 chars
        if metadata.name.is_empty() || metadata.name.len() > 128 {
            anyhow::bail!("name must be 1-128 characters, got {}", metadata.name.len());
        }

        // decimals: 0-18
        if metadata.decimals > 18 {
            anyhow::bail!("decimals must be 0-18, got {}", metadata.decimals);
        }

        // max_supply: must be a valid positive decimal if present
        if let Some(ref max_supply) = metadata.max_supply {
            use rust_decimal::Decimal;
            use std::str::FromStr;
            match Decimal::from_str(max_supply) {
                Ok(d) if d > Decimal::ZERO => {}
                Ok(_) => anyhow::bail!("max_supply must be positive"),
                Err(e) => anyhow::bail!("max_supply is not a valid decimal: {}", e),
            }
        }

        // creator and mint_authority: non-empty
        if metadata.creator.trim().is_empty() {
            anyhow::bail!("creator address cannot be empty");
        }
        if metadata.mint_authority.trim().is_empty() {
            anyhow::bail!("mint_authority cannot be empty");
        }

        // demurrage (2.5): au plus 10_000 bps/jour (100 %/jour)
        if let Some(bps) = metadata.demurrage_bps_per_day {
            if bps > 10_000 {
                anyhow::bail!("demurrage_bps_per_day must be <= 10000, got {bps}");
            }
        }

        // mint collatéralisé (2.3 v2) : cohérence des trois champs
        match (&metadata.collateral_address, metadata.collateral_ratio_bps) {
            (Some(addr), ratio) => {
                if addr.trim().is_empty() {
                    anyhow::bail!("collateral_address cannot be empty");
                }
                match ratio {
                    Some(0) | None => anyhow::bail!(
                        "collateral_ratio_bps is required (and must be > 0) when collateral_address is set"
                    ),
                    Some(_) => {}
                }
            }
            (None, Some(_)) => {
                anyhow::bail!("collateral_ratio_bps requires collateral_address");
            }
            (None, None) => {
                if metadata.collateral_asset_id.is_some() {
                    anyhow::bail!("collateral_asset_id requires collateral_address");
                }
            }
        }

        Ok(())
    }

    /// Enregistre un nouveau token dans le registre.
    /// Retourne une erreur si le token existe déjà ou metadata is invalid.
    pub fn register_token(&self, metadata: &TokenMetadata) -> Result<()> {
        // Validate metadata format
        Self::validate_token_metadata(metadata)?;

        let cf = self.cf_token_registry();
        let key = metadata.asset_id.as_bytes();

        // Vérifier l'unicité
        if self.db.get_cf(&cf, key)?.is_some() {
            anyhow::bail!("token already exists: {}", metadata.asset_id);
        }

        let json = serde_json::to_vec(metadata)?;
        self.db.put_cf(&cf, key, json)?;
        Ok(())
    }

    /// Récupère les métadonnées d'un token par son asset_id.
    pub fn get_token(&self, asset_id: &str) -> Result<Option<TokenMetadata>> {
        let cf = self.cf_token_registry();
        if let Some(val) = self.db.get_cf(&cf, asset_id.as_bytes())? {
            let meta: TokenMetadata = serde_json::from_slice(&val)?;
            Ok(Some(meta))
        } else {
            Ok(None)
        }
    }

    /// Liste tous les tokens enregistrés.
    pub fn list_tokens(&self) -> Result<Vec<TokenMetadata>> {
        let cf = self.cf_token_registry();
        let mut tokens = Vec::new();
        let iter = self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start);
        for kv in iter {
            let (_, val) = kv?;
            let meta: TokenMetadata = serde_json::from_slice(&val)?;
            tokens.push(meta);
        }
        Ok(tokens)
    }
}

/// Délégation vers la méthode inhérente — permet au hot path générique
/// (`CoreAdapter<S: TokenRegistryStorage + …>`) de lire le registre.
impl crate::token_store::TokenRegistryStorage for RocksStore {
    fn get_token(&self, asset_id: &str) -> Result<Option<TokenMetadata>> {
        RocksStore::get_token(self, asset_id)
    }
}
