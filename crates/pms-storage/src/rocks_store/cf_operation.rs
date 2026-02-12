use crate::rocks_store::store::RocksStore;
use std::sync::Arc;
use rocksdb::BoundColumnFamily;

impl RocksStore {
    /// Récupère un handle de colonne "prefix:<name>"
    pub fn cf(&self, short: &str) -> Arc<BoundColumnFamily<'_>> {
        let full = format!("{}:{}", self.prefix, short);
        self.db
            .cf_handle(&full)
            .unwrap_or_else(|| panic!("missing column family {}", full))
    }

    /// Colonnes pratiques (doivent exister dans `new()`):
    ///   <prefix>:utxo        (HASH outpoint -> json {addr, amt})
    ///   <prefix>:tx:applied  (SET txid -> "")
    #[inline]
    pub(crate) fn cf_utxo(&self) -> Arc<BoundColumnFamily<'_>> {
        self.cf("utxo")
    }
    #[inline]
    pub(crate) fn cf_tx_applied(&self) -> Arc<BoundColumnFamily<'_>> {
        self.cf("tx_applied")
    }
}
