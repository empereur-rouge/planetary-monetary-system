// pms-core/src/block_builder.rs
use pms_types::{Block, BlockId, PayloadEnvelope};

/// Builder générique pour miner un nonce et construire un Block.
pub struct BlockMineBuilder<FId, FExists> {
    parents: Vec<String>,
    payload: Option<PayloadEnvelope>,
    difficulty_leading_zeros: u8, // 0 = PoW désactivé
    compute_id: FId,              // (&[String], &Option<PayloadEnvelope>, u64) -> BlockId
    id_exists: FExists,           // (&str) -> bool
    canonicalize_parents: bool,   // tri lexicographique avant ID
}

impl<FId, FExists> BlockMineBuilder<FId, FExists>
where
    FId: Fn(&[String], &Option<PayloadEnvelope>, u64) -> BlockId,
    FExists: Fn(&str) -> bool,
{
    pub fn new(
        parents: Vec<String>,
        payload: Option<PayloadEnvelope>,
        compute_id: FId,
        id_exists: FExists,
    ) -> Self {
        Self {
            parents,
            payload,
            difficulty_leading_zeros: 0,
            compute_id,
            id_exists,
            canonicalize_parents: true,
        }
    }

    pub fn difficulty(mut self, leading_zeros_bits: u8) -> Self {
        self.difficulty_leading_zeros = leading_zeros_bits;
        self
    }

    pub fn canonicalize_parents(mut self, yes: bool) -> Self {
        self.canonicalize_parents = yes;
        self
    }

    /// Mine le nonce et retourne (nonce, id).
    pub fn mine(&self) -> (u64, BlockId) {
        use rand::{Rng, rng};
        let mut parents = self.parents.clone();
        if self.canonicalize_parents {
            parents.sort();
        }

        let ok_pow = |hex_id: &str, diff: u8| -> bool {
            if diff == 0 {
                return true;
            }
            let zeros_nibbles = hex_id.chars().take_while(|&c| c == '0').count() as u8;
            zeros_nibbles * 4 >= diff
        };

        let mut rng = rng();
        let mut nonce = rng.random_range(0..u64::MAX);

        for _ in 0..1_000_000 {
            let id = (self.compute_id)(&parents, &self.payload, nonce);
            if !(self.id_exists)(&id) && ok_pow(&id, self.difficulty_leading_zeros) {
                return (nonce, id);
            }
            nonce = nonce.wrapping_add(1);
        }

        loop {
            nonce = rng.random_range(0..u64::MAX);
            let id = (self.compute_id)(&parents, &self.payload, nonce);
            if !(self.id_exists)(&id) && ok_pow(&id, self.difficulty_leading_zeros) {
                return (nonce, id);
            }
        }
    }

    pub fn build(self) -> Block {
        // mine() already sorts parents internally when canonicalize_parents is set
        let (nonce, id) = self.mine();

        let BlockMineBuilder {
            mut parents,
            payload,
            canonicalize_parents,
            ..
        } = self;

        if canonicalize_parents {
            parents.sort();
        }

        Block {
            id,
            parents,
            payload,
            nonce,
            metadata: None,
            signer_pk: None,
            signature: None,
        }
    }
}
