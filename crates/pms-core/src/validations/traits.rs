use pms_types::Block;

/// Écriture (appliquée seulement après validation)
pub trait WriteState {
    fn add_block_mem(&mut self, b: &Block); // index RAM
    fn bump_children(&mut self, parent: &str); // children_count RAM
    fn mark_spent_ram(&mut self, outpoint: (&str, u32)); // anti-double-spend RAM
    fn update_finality_after_insert(&mut self, id: &str);
    fn add_child_edge_mem(&mut self, child: &str, parents: &[String]);
}
