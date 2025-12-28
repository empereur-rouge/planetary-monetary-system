use pms_types::Block;

/// Lecture seule (utilisée par la validation)
pub trait ReadState {
    fn have_block(&self, id: &str) -> bool;
    fn parent_exists(&self, id: &str) -> bool;
    fn spent_in_ram(&self, outpoint: (&str, u32)) -> bool;
    // … (balance/UTXO lecture plus tard)
}

/// Écriture (appliquée seulement après validation)
pub trait WriteState {
    fn add_block_mem(&mut self, b: &Block);            // index RAM
    fn bump_children(&mut self, parent: &str);         // children_count RAM
    fn mark_spent_ram(&mut self, outpoint: (&str,u32));// anti-double-spend RAM
    fn update_finality_after_insert(&mut self, id: &str);
    fn add_child_edge_mem(&mut self, child: &str, parents: &[String]);
}