use crate::rocks_store::store::RocksStore;
use rocksdb::BoundColumnFamily;
use std::sync::Arc;

impl RocksStore {
    /// Récupère un handle de colonne "prefix:<name>" via pre-computed name cache.
    /// No `format!()` allocation — just a HashMap lookup.
    pub fn cf(&self, short: &str) -> Arc<BoundColumnFamily<'_>> {
        let full = self
            .cf_names
            .get(short)
            .unwrap_or_else(|| panic!("unknown column family short name: {short}"));
        self.db
            .cf_handle(full)
            .unwrap_or_else(|| panic!("missing column family {full}"))
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
