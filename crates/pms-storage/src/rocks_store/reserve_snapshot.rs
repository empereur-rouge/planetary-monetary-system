//! Index de commodité du dernier `ReserveSnapshot` ancré (protocole 2.6).
//!
//! Le bloc DAG est la source de vérité (preuve immuable signée Coordinator) ;
//! ce pointeur évite seulement de scanner le DAG pour `GET /v1/reserves/latest`.
//! Réutilise le CF `last_ms` (pointeurs simples, comme le dernier milestone)
//! sous une clé dédiée — pas de nouveau column family ni de migration.

use crate::rocks_store::store::RocksStore;
use anyhow::Result;

/// Clé du pointeur dans le CF `last_ms`.
const LAST_RESERVE_SNAPSHOT_KEY: &[u8] = b"last_reserve_snapshot";

impl RocksStore {
    /// Enregistre le dernier snapshot de réserves ancré.
    /// `snapshot_json` = `{ block_id, state_root, total_supply, utxo_count, computed_at_ms }`.
    pub fn record_reserve_snapshot(&self, snapshot_json: &str) -> Result<()> {
        let cf = self.cf("last_ms");
        self.db
            .put_cf(&cf, LAST_RESERVE_SNAPSHOT_KEY, snapshot_json.as_bytes())?;
        Ok(())
    }

    /// Dernier snapshot de réserves ancré (JSON), `None` si aucun.
    pub fn latest_reserve_snapshot(&self) -> Result<Option<String>> {
        let cf = self.cf("last_ms");
        Ok(self
            .db
            .get_cf(&cf, LAST_RESERVE_SNAPSHOT_KEY)?
            .map(|v| String::from_utf8_lossy(&v).into_owned()))
    }
}
