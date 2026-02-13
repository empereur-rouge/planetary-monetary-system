use crate::validations::traits::WriteState;
use pms_types::{Block, PayloadEnvelope, PlainPayload};

/// Applique les effets **en mémoire** (pas de Redis ici).
pub fn apply_block_mem<W: WriteState>(dag: &mut W, b: &Block) {
    // ------------------------------------------------------------
    // 1) Parents uniques (évite de sur-compter les enfants)
    // ------------------------------------------------------------
    let mut uniq = b.parents.clone();
    uniq.sort();
    uniq.dedup();

    // ------------------------------------------------------------
    // 2) Met à jour les compteurs d'enfants / index parent -> enfants
    // ------------------------------------------------------------
    for p in &uniq {
        dag.bump_children(p);
    }
    dag.add_child_edge_mem(&b.id, &uniq);

    // ------------------------------------------------------------
    // 3) UTXO en RAM (double-spend rapide)
    //
    // Ici on ne gère que les *inputs* :
    //   - chaque input est marqué comme "spent" dans `spent_outpoints`
    //   - les outputs (y compris l’output "fees → wallet système")
    //     sont gérés côté RocksDB par la logique UTXO atomique.
    //
    // => La RAM sert seulement de garde-fou rapide contre le re-spend.
    // ------------------------------------------------------------
    if let Some(PayloadEnvelope::Plain(PlainPayload::TxUtxo(tx))) = &b.payload {
        for inp in &tx.inputs {
            dag.mark_spent_ram((&inp.out.txid, inp.out.index));
        }
        // NOTE :
        //  - Les outputs normaux sont inscrits dans le payload du bloc
        //    (TxOutput { address, amount }).
        //  - L'output "fees" vers le SYSTEM_FEES_WALLET est un output
        //    comme les autres, ajouté au moment de la construction de
        //    la transaction.
        //  - Leur existence en tant qu'UTXO exploitable est gérée
        //    dans RocksDB via apply_tx_utxo_atomic() / équivalent.
    }

    // BridgeLock: mark inputs as spent (funds leave this ledger)
    if let Some(PayloadEnvelope::Plain(PlainPayload::BridgeLock { inputs, .. })) = &b.payload {
        for inp in inputs {
            dag.mark_spent_ram((&inp.out.txid, inp.out.index));
        }
    }

    // Seize: mark seized UTXOs as spent (transferred to treasury)
    if let Some(PayloadEnvelope::Plain(PlainPayload::Seize { inputs, .. })) = &b.payload {
        for inp in inputs {
            dag.mark_spent_ram((&inp.out.txid, inp.out.index));
        }
    }

    // Reverse: mark reversed outputs as spent (refunded to original senders)
    if let Some(PayloadEnvelope::Plain(PlainPayload::Reverse { inputs, .. })) = &b.payload {
        for inp in inputs {
            dag.mark_spent_ram((&inp.out.txid, inp.out.index));
        }
    }

    // ------------------------------------------------------------
    // 4) Enregistre le bloc dans le DAG + finalité
    // ------------------------------------------------------------
    dag.add_block_mem(b);
    dag.update_finality_after_insert(&b.id);
}
