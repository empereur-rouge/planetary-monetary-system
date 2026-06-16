use anyhow::{Context, Result, bail};
use pms_storage::rocks_store::store::RocksStore;
use std::sync::Arc;

use crate::types::BridgeLink;

/// Accès au storage des ponts (CFs `bridge_links` et `bridge_consumed`).
///
/// Utilise le store du default ledger pour accéder aux CFs globales.
pub struct BridgeStore {
    store: Arc<RocksStore>,
}

impl BridgeStore {
    pub fn new(store: Arc<RocksStore>) -> Self {
        Self { store }
    }

    // --- Bridge Links ---

    /// Crée ou met à jour un lien de pont.
    pub fn set_bridge_link(&self, link: &BridgeLink) -> Result<()> {
        let cf = self.store.cf("bridge_links");
        let key = BridgeLink::storage_key(&link.ledger_a, &link.ledger_b);
        let val = serde_json::to_vec(link).context("serialize BridgeLink")?;
        self.store
            .db
            .put_cf(&cf, key.as_bytes(), &val)
            .context("put bridge_links")?;
        Ok(())
    }

    /// Récupère un lien de pont entre deux ledgers.
    pub fn get_bridge_link(&self, ledger_a: &str, ledger_b: &str) -> Result<Option<BridgeLink>> {
        let cf = self.store.cf("bridge_links");
        let key = BridgeLink::storage_key(ledger_a, ledger_b);
        match self.store.db.get_cf(&cf, key.as_bytes())? {
            Some(val) => {
                let link: BridgeLink =
                    serde_json::from_slice(&val).context("deserialize BridgeLink")?;
                Ok(Some(link))
            }
            None => Ok(None),
        }
    }

    /// Vérifie si un pont actif existe entre les deux ledgers dans la direction donnée.
    pub fn is_bridge_enabled(&self, from: &str, to: &str) -> Result<bool> {
        match self.get_bridge_link(from, to)? {
            Some(link) => Ok(link.allows_transfer(from, to)),
            None => Ok(false),
        }
    }

    /// Désactive un pont (ne le supprime pas).
    pub fn disable_bridge_link(&self, ledger_a: &str, ledger_b: &str) -> Result<()> {
        let mut link = self
            .get_bridge_link(ledger_a, ledger_b)?
            .ok_or_else(|| anyhow::anyhow!("bridge link not found: {}:{}", ledger_a, ledger_b))?;
        link.enabled = false;
        link.disabled_at = Some(now_ms());
        self.set_bridge_link(&link)
    }

    /// Liste tous les liens de pont.
    pub fn list_bridge_links(&self) -> Result<Vec<BridgeLink>> {
        let cf = self.store.cf("bridge_links");
        let iter = self.store.db.iterator_cf(&cf, rocksdb::IteratorMode::Start);
        let mut links = Vec::new();
        for item in iter {
            let (_, val) = item.context("iterate bridge_links")?;
            let link: BridgeLink =
                serde_json::from_slice(&val).context("deserialize BridgeLink")?;
            links.push(link);
        }
        Ok(links)
    }

    // --- Bridge transfer-status index (lock → mint) ---
    //
    // NOTE (audit rang 3, B3) : ces entrées (CF `bridge_consumed` du store `main`)
    // ne sont PLUS le mécanisme anti-replay autoritaire. L'anti-replay est enforced
    // au niveau persist du `BridgeMint` (claim atomique RAM + CF `bridge_consumed`
    // du store DESTINATION), et la réconciliation montant/asset/destinataire/ledger
    // l'est aussi. Ici il ne s'agit que de l'index de statut lock→mint qui sert à
    // `BridgeEngine::transfer_status` (et au unit-test du store).

    /// Vérifie si un lock a une entrée de statut (consommé) — index, pas l'autorité.
    pub fn is_bridge_lock_consumed(&self, lock_block_id: &str) -> Result<bool> {
        let cf = self.store.cf("bridge_consumed");
        Ok(self
            .store
            .db
            .get_cf(&cf, lock_block_id.as_bytes())?
            .is_some())
    }

    /// Marque un BridgeLock comme consommé, enregistrant quel BridgeMint l'a consommé.
    pub fn mark_bridge_lock_consumed(
        &self,
        lock_block_id: &str,
        mint_block_id: &str,
    ) -> Result<()> {
        let cf = self.store.cf("bridge_consumed");
        if self
            .store
            .db
            .get_cf(&cf, lock_block_id.as_bytes())?
            .is_some()
        {
            bail!("bridge lock already consumed: {}", lock_block_id);
        }
        self.store
            .db
            .put_cf(&cf, lock_block_id.as_bytes(), mint_block_id.as_bytes())
            .context("put bridge_consumed")?;
        Ok(())
    }

    /// Récupère l'ID du bloc BridgeMint qui a consommé un lock donné.
    pub fn get_bridge_mint_for_lock(&self, lock_block_id: &str) -> Result<Option<String>> {
        let cf = self.store.cf("bridge_consumed");
        match self.store.db.get_cf(&cf, lock_block_id.as_bytes())? {
            Some(val) => Ok(Some(
                String::from_utf8(val.to_vec()).context("bridge_consumed value as utf8")?,
            )),
            None => Ok(None),
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
