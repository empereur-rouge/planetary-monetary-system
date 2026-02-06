use crate::Dag;
use crate::validations::traits::WriteState;
use pms_types::Block;

impl WriteState for Dag {
    fn add_block_mem(&mut self, b: &Block) {
        self.blocks.insert(b.id.clone(), b.clone());
    }
    fn bump_children(&mut self, p: &str) {
        *self.children.entry(p.into()).or_default() += 1;
    }
    fn mark_spent_ram(&mut self, out: (&str, u32)) {
        self.spent_outpoints.insert((out.0.to_string(), out.1));
    }
    fn update_finality_after_insert(&mut self, id: &str) {
        self.update_finality_after_insert(id);
    }
    fn add_child_edge_mem(&mut self, child: &str, parents: &[String]) {
        for p in parents {
            self.children_idx
                .entry(p.clone())
                .or_default()
                .push(child.to_string());
        }
    }
}
