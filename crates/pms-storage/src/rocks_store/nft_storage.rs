use crate::{NftStorage, rocks_store::store::RocksStore};
use anyhow::Result;

impl NftStorage for RocksStore {
    fn get_owner(&self, token_id: &str) -> Result<Option<String>> {
        let cf_nft = self.cf("nft_ownership");
        if let Some(v) = self.db.get_cf(cf_nft, token_id.as_bytes())? {
            Ok(Some(String::from_utf8(v.to_vec())?))
        } else {
            Ok(None)
        }
    }

    fn set_owner(&self, token_id: &str, owner: &str) -> Result<()> {
        let cf_nft = self.cf("nft_ownership");
        self.db
            .put_cf(cf_nft, token_id.as_bytes(), owner.as_bytes())?;
        Ok(())
    }

    fn delete(&self, token_id: &str) -> Result<()> {
        let cf_nft = self.cf("nft_ownership");
        self.db.delete_cf(cf_nft, token_id.as_bytes())?;
        Ok(())
    }
}
