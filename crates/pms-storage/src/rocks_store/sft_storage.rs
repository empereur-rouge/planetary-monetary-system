//! Persistance des classes semi-fongibles (SFT, `pms-spec-semi-fungibles.md`).
//!
//! CF `sft_classes` : `asset_id` (= `"collection:class"`) → [`SftClass`] JSON.
//! C'est le **registre de définitions** (catalogue public) ; les SOLDES vivent
//! dans le moteur UTXO (CF `utxo`, agnostique de l'asset), donc rien de
//! spécifique n'est stocké ici pour la comptabilité — uniquement les métadonnées
//! de classe + les contraintes de mint (`max_supply`, `mint_authority`).

use crate::rocks_store::store::RocksStore;
use crate::token_store::SftClassStorage;
use anyhow::Result;
use pms_types_payload::SftClass;

impl SftClassStorage for RocksStore {
    fn put_sft_class(&self, class: &SftClass) -> Result<()> {
        let cf = self.cf("sft_classes");
        let json = serde_json::to_vec(class)?;
        self.db.put_cf(&cf, class.asset_id.as_bytes(), &json)?;
        Ok(())
    }

    fn get_sft_class(&self, asset_id: &str) -> Result<Option<SftClass>> {
        let cf = self.cf("sft_classes");
        match self.db.get_cf(&cf, asset_id.as_bytes())? {
            Some(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            None => Ok(None),
        }
    }

    fn list_sft_classes(&self) -> Result<Vec<SftClass>> {
        let cf = self.cf("sft_classes");
        let mut out = Vec::new();
        for kv in self.db.iterator_cf(&cf, rocksdb::IteratorMode::Start) {
            let (_k, v) = kv?;
            // Tolère un éventuel marqueur __init__ de migration (valeur vide).
            if let Ok(class) = serde_json::from_slice::<SftClass>(&v) {
                out.push(class);
            }
        }
        Ok(out)
    }

    fn list_sft_classes_by_collection(&self, collection_id: &str) -> Result<Vec<SftClass>> {
        // Le préfixe d'`asset_id` EST la collection (`"{collection}:..."`) — on
        // borne le scan par préfixe plutôt que de tout désérialiser puis filtrer.
        let prefix = format!("{collection_id}:");
        let cf = self.cf("sft_classes");
        let mut out = Vec::new();
        let mode = rocksdb::IteratorMode::From(prefix.as_bytes(), rocksdb::Direction::Forward);
        for kv in self.db.iterator_cf(&cf, mode) {
            let (k, v) = kv?;
            if !k.starts_with(prefix.as_bytes()) {
                break; // sorti du préfixe → fin de la collection
            }
            if let Ok(class) = serde_json::from_slice::<SftClass>(&v) {
                out.push(class);
            }
        }
        Ok(out)
    }

    fn put_collection_owner(&self, collection_id: &str, owner: &str) -> Result<()> {
        let cf = self.cf("sft_collections");
        self.db.put_cf(&cf, collection_id.as_bytes(), owner.as_bytes())?;
        Ok(())
    }

    fn get_collection_owner(&self, collection_id: &str) -> Result<Option<String>> {
        let cf = self.cf("sft_collections");
        match self.db.get_cf(&cf, collection_id.as_bytes())? {
            Some(bytes) => Ok(Some(String::from_utf8(bytes.to_vec())?)),
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rocks_store::store::RocksMemoryConfig;
    use tempfile::TempDir;

    async fn fresh_store() -> (RocksStore, TempDir) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().to_string_lossy().into_owned();
        let store = RocksStore::new(&path, 16, "main", None, &RocksMemoryConfig::default())
            .await
            .expect("rocks");
        (store, dir)
    }

    fn class(collection: &str, class: &str) -> SftClass {
        SftClass {
            asset_id: format!("{collection}:{class}"),
            collection_id: collection.into(),
            class_id: class.into(),
            name: format!("{class} item"),
            uri: None,
            attributes: None,
            decimals: 0,
            max_supply: Some("1000".into()),
            demurrage_bps_per_day: None,
            creator: "creator_pk".into(),
            mint_authority: "minter_pk".into(),
            royalty_bps: None,
            royalty_beneficiary: None,
            royalty_version: 0,
        }
    }

    /// S1 — round-trip + listing global + listing par collection (préfixe).
    #[tokio::test]
    async fn sft_class_roundtrip_and_listing() {
        let (store, _d) = fresh_store().await;
        assert!(store.list_sft_classes().unwrap().is_empty(), "fresh store vide");

        let sword = class("edenite-game", "iron-sword");
        let shield = class("edenite-game", "wood-shield");
        let ticket = class("concert", "vip-pass");
        store.put_sft_class(&sword).unwrap();
        store.put_sft_class(&shield).unwrap();
        store.put_sft_class(&ticket).unwrap();

        // get exact round-trip
        let got = store.get_sft_class("edenite-game:iron-sword").unwrap().unwrap();
        println!("stored class: {got:?}");
        assert_eq!(got, sword, "round-trip exact");
        assert!(store.get_sft_class("nope:nope").unwrap().is_none());

        // list global
        assert_eq!(store.list_sft_classes().unwrap().len(), 3);

        // list par collection (préfixe) — edenite-game a 2 classes, concert 1
        let game = store.list_sft_classes_by_collection("edenite-game").unwrap();
        println!("edenite-game classes: {:?}", game.iter().map(|c| &c.asset_id).collect::<Vec<_>>());
        assert_eq!(game.len(), 2, "edenite-game = 2 classes");
        assert!(game.iter().all(|c| c.collection_id == "edenite-game"));
        assert_eq!(store.list_sft_classes_by_collection("concert").unwrap().len(), 1);
        assert_eq!(store.list_sft_classes_by_collection("absent").unwrap().len(), 0);
    }
}
