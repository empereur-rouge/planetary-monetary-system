use pms_types::{Transaction, TxOutput};

pub enum LedgerMutation<'a> {
    None,
    Mint {
        block_id: &'a str,
        outputs: &'a [TxOutput],
    },
    TxUtxo {
        tx: &'a Transaction,
    },
    // plus tard:
    // Milestone { ... },
    // NftMint { ... },
}
