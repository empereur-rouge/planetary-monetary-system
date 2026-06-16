use anyhow::Result;
use async_trait::async_trait;
use pms_storage::UtxoDelta;
use pms_storage::store::PutResult;
use pms_types::{TxInput, TxOutput};
use pms_wire::WireBlock;
use std::sync::Arc;

/// Committed fields of a source-ledger `BridgeLock`, resolved cross-ledger so a
/// `BridgeMint` on the destination ledger can be reconciled against the lock it
/// claims to back (audit rang 3, B3 — réconciliation cross-ledger).
#[derive(Debug, Clone)]
pub struct BridgeLockInfo {
    /// Amount the source lock committed (decimal string).
    pub amount: String,
    /// Asset the lock committed (`None` = native PMS).
    pub asset_id: Option<String>,
    /// Destination ledger the lock was addressed to.
    pub dest_ledger_id: String,
    /// Destination address the lock was addressed to.
    pub dest_address: String,
}

/// Resolves a source-ledger `BridgeLock` so the destination ledger's persist
/// path can reconcile a `BridgeMint` against it (amount / asset / recipient).
///
/// The destination `CoreAdapter` only sees its own prefix-scoped store; the
/// implementor (the `LedgerManager`, which sees every ledger) bridges that gap.
/// Returns `Ok(None)` when no such `BridgeLock` exists on `source_ledger_id`
/// (unknown ledger, missing block, or the block is not a `BridgeLock`).
#[async_trait]
pub trait BridgeLockResolver: Send + Sync {
    /// Resolve the committed fields of the `BridgeLock` `lock_block_id` on
    /// `source_ledger_id`.
    async fn resolve_bridge_lock(
        &self,
        source_ledger_id: &str,
        lock_block_id: &str,
    ) -> Result<Option<BridgeLockInfo>>;
}

/// Trait que le serveur réseau utilisera pour interagir avec le core.
#[async_trait]
pub trait NetDagAdapter: Send + Sync {
    /// Vérifie si on possède déjà ce bloc.
    async fn have_block(&self, id: &str) -> bool;
    /// Persiste un bloc (idempotent).
    async fn persist_block(&self, b: &WireBlock) -> Result<PutResult>;

    /// Persist a block together with an externally-provided `UtxoDelta`.
    ///
    /// Exists to plug audit finding H1: the previous caller-side pattern
    /// `persist_block(wb).await; apply_utxo_delta(inputs, outputs).await;`
    /// leaves a gap where the block is already visible in the RAM DAG and
    /// the persist pipeline while the UTXO set still lists the inputs as
    /// spendable, so a concurrent handler could re-select them.
    ///
    /// Encrypted payloads are the target use case: `do_persist_block` can't
    /// derive a `UtxoDelta` from the ciphertext, so the caller — which knows
    /// the plaintext — builds one and hands it over here.
    ///
    /// The default implementation preserves the old two-step behaviour
    /// (non-atomic) so mocks that don't need atomicity keep compiling.
    /// Production implementations MUST override this to apply the delta
    /// in the same critical section as the block insert (see
    /// `CoreAdapter::persist_block_with_delta`).
    async fn persist_block_with_delta(
        &self,
        wb: &WireBlock,
        delta: UtxoDelta,
    ) -> Result<PutResult> {
        let res = self.persist_block(wb).await?;
        if matches!(res, PutResult::Inserted) {
            for (txid, idx) in &delta.spend {
                self.remove_utxo(&pms_types::OutputId {
                    txid: txid.clone(),
                    index: *idx,
                })
                .await;
            }
            for (txid, idx, out) in &delta.create {
                self.add_utxo(txid.clone(), *idx, out.clone()).await;
            }
        }
        Ok(res)
    }

    /// Helper: build a `UtxoDelta` from plaintext `inputs` / `outputs` of an
    /// encrypted transaction. Convenience wrapper callers can use instead
    /// of constructing the delta tuples by hand.
    fn build_encrypted_utxo_delta(
        &self,
        block_id: &str,
        inputs: &[TxInput],
        outputs: &[TxOutput],
    ) -> UtxoDelta {
        let spend = inputs
            .iter()
            .map(|inp| (inp.out.txid.clone(), inp.out.index))
            .collect();
        // Demurrage 2.5 : estampille `created_at` système, comme le pipeline
        // plain de `persist_block` (anti-antidatage). Horloge partagée de
        // pms-storage (pms-utils créerait un cycle via pms-network).
        let now_ms = pms_storage::helpers::now_ms_i64().max(0) as u64;
        let create = outputs
            .iter()
            .enumerate()
            .map(|(i, out)| {
                (
                    block_id.to_string(),
                    i as u32,
                    TxOutput {
                        created_at: Some(now_ms),
                        ..out.clone()
                    },
                )
            })
            .collect();
        UtxoDelta { spend, create }
    }
    /// Diffuse un bloc aux pairs.
    async fn broadcast_block(&self, b: &WireBlock) -> Result<()>;
    async fn top_tips(&self, limit: usize) -> Result<Vec<String>>;
    async fn get_block(&self, id: &str) -> Result<Option<WireBlock>>;
    async fn recent_ids(&self, limit: usize) -> Result<Vec<String>>;
    async fn get_blocks_by_ids(&self, ids: &[String]) -> Result<Vec<WireBlock>>;

    /// Approximate number of current DAG tips. Read from an atomic counter
    /// (no Vec clone, no BFS) — used by `GET /v1/dag/status` polled by SaaS
    /// watchers. Default impl falls back to `top_tips(usize::MAX).len()`.
    async fn tip_count_estimate(&self) -> usize {
        self.top_tips(usize::MAX).await.map(|v| v.len()).unwrap_or(0)
    }

    /// Number of distinct descendants of `block_id` in the RAM DAG, capped at
    /// `max_count` so a popular block doesn't BFS the whole graph. The SaaS
    /// payment rail uses this as the "confirmations" equivalent for DAG
    /// finality tiers (cf. `GET /v1/transaction/{id}`).
    /// Default impl returns 0 — concrete adapters MUST override.
    async fn count_descendants(&self, _block_id: &str, _max_count: usize) -> usize {
        0
    }

    /// True iff the block has been marked finalized by the consensus layer
    /// (k-depth confirmation reached or coordinator milestone). Default impl
    /// returns `false` for adapters that don't track finality.
    async fn is_finalized(&self, _block_id: &str) -> bool {
        false
    }

    /// Most recent milestone block id (coordinator-signed checkpoint), if any.
    /// Default impl returns `None`.
    async fn last_milestone(&self) -> Option<String> {
        None
    }
    /// [DEPRECATED] PoW is disabled for Private DAG. Returns 0.
    /// Kept for API compatibility, will be removed in a future version.
    fn min_pow_leading_zero_bits(&self) -> u8;

    /// Retourne le supply total en circulation (PMS natif) et le nombre d'UTXOs.
    async fn circulating_supply(&self) -> (rust_decimal::Decimal, u64);

    /// Retourne le supply en circulation d'un asset spécifique (None = PMS natif).
    async fn circulating_supply_by_asset(
        &self,
        asset_id: Option<&str>,
    ) -> (rust_decimal::Decimal, u64);

    /// Retourne la balance d'une adresse (somme des UTXOs non dépensés PMS).
    async fn balance_by_address(&self, address: &str) -> rust_decimal::Decimal;

    /// Retourne la balance d'une adresse pour un asset spécifique.
    /// `asset_id = None` → PMS natif (O(1) via cache).
    /// `asset_id = Some("edenite")` → balance custom token (shard scan).
    async fn balance_by_address_and_asset(
        &self,
        address: &str,
        asset_id: Option<&str>,
    ) -> rust_decimal::Decimal;

    /// Retourne tous les UTXOs d'une adresse depuis le set UTXO en mémoire.
    async fn utxos_by_address(
        &self,
        address: &str,
    ) -> Vec<(pms_types::OutputId, pms_types::TxOutput)>;

    /// Returns up to `limit` UTXOs for coin selection, stopping when enough
    /// value is accumulated. Avoids cloning ALL UTXOs for large addresses
    /// (e.g., coordinator with millions of fee reward UTXOs).
    ///
    /// Default implementation falls back to full `utxos_by_address()` + filter.
    /// `CoreAdapter` overrides with an optimized early-exit implementation.
    async fn utxos_for_selection(
        &self,
        address: &str,
        asset_id: &Option<String>,
        target: rust_decimal::Decimal,
        limit: usize,
    ) -> (
        Vec<(pms_types::OutputId, pms_types::TxOutput, rust_decimal::Decimal)>,
        rust_decimal::Decimal,
    ) {
        let all = self.utxos_by_address(address).await;
        let mut result = Vec::new();
        let mut total = rust_decimal::Decimal::ZERO;
        for (oid, txo) in all {
            if txo.asset_id != *asset_id {
                continue;
            }
            if let Ok(amt) = rust_decimal::Decimal::from_str_exact(&txo.amount) {
                result.push((oid, txo, amt));
                total += amt;
                if result.len() >= limit && total >= target {
                    break;
                }
            }
        }
        (result, total)
    }

    /// Ajoute un UTXO manuellement (utilisé par le coordinateur pour les EncryptedReward).
    ///
    /// Prend le `TxOutput` COMPLET — tout champ protocole de l'output
    /// (asset_id, locked_until, spend_condition, …) doit survivre jusqu'au
    /// cache UTXO, sinon il devient invisible au validateur.
    async fn add_utxo(&self, txid: String, index: u32, output: TxOutput);

    /// Supprime un UTXO du cache (utilisé quand un input est consommé par une TX encrypted)
    /// Retourne true si l'UTXO existait et a été supprimé, false sinon.
    async fn remove_utxo(&self, output_id: &pms_types::OutputId) -> bool;

    /// Récupère un UTXO spécifique par son OutputId depuis le set UTXO en mémoire.
    /// Retourne None si l'UTXO n'existe pas (déjà dépensé ou inexistant).
    async fn get_utxo(&self, output_id: &pms_types::OutputId) -> Option<pms_types::TxOutput>;

    /// **Validation complète du plaintext d'un `TxUtxo`** avant soumission via un
    /// payload CHIFFRÉ : signatures, appariement input/unlock, binding ownership
    /// (C-1), autorisation MultiSig/HashLock, time-locks des inputs, **dédup des
    /// inputs dupliqués** (anti-inflation), conservation par-asset, ET gel
    /// compliance (inputs + outputs). Retourne les outputs des inputs résolus.
    ///
    /// Un payload chiffré est opaque pour `persist_block` (qui saute
    /// `validate_transaction_full`) : tout handler qui chiffre un `TxUtxo` DOIT
    /// appeler ceci sur le plaintext AVANT chiffrement. C'est la MÊME logique
    /// que le hot-path (`CoreAdapter::validate_plain_txutxo`), donc les deux
    /// chemins ne peuvent pas diverger (audit 2026-06, cause A).
    ///
    /// # Default
    /// **FAIL-CLOSED** : rejette par défaut. Tout adaptateur réel DOIT l'override
    /// (CoreAdapter le fait). Le défaut n'existe que pour que les mocks de test
    /// (qui ne soumettent jamais de tx) compilent sans bypasser silencieusement.
    async fn validate_txutxo_full(
        &self,
        _tx: &pms_types::Transaction,
        _now_ms: u64,
    ) -> std::result::Result<Vec<pms_types::TxOutput>, String> {
        Err("validate_txutxo_full not implemented for this adapter (fail-closed)".to_string())
    }

    /// Retourne l'EventBus pour s'abonner aux événements (SSE streaming).
    /// Default: None (mocks de test n'ont pas besoin d'event bus).
    fn event_bus(&self) -> Option<pms_event::EventBus> {
        None
    }

    /// Healthz introspection — current depth of the background persist
    /// queue and its maximum capacity. Returns `(used, capacity)` where
    /// `used` is the number of jobs currently pending in the channel and
    /// `capacity` is the buffer size set at spawn time.
    ///
    /// Default `None` so mocks that don't run a real persist task don't
    /// need to fake numbers. Production `CoreAdapter` returns a real
    /// reading derived from `tokio::sync::mpsc::Sender::{capacity,
    /// max_capacity}`. Used by `/healthz` to flag a saturated pipeline
    /// (degraded), and by ops dashboards.
    fn persist_queue_depth(&self) -> Option<(usize, usize)> {
        None
    }

    /// Current size of the in-memory UTXO set (number of unspent outputs).
    ///
    /// Default `None` so mocks that don't keep a UTXO set can opt out — the
    /// metrics sampler ignores `None`. Production `CoreAdapter` reports
    /// `ShardedUtxoSet::total_len()`. Used by the metrics sampler to
    /// publish `pms_utxo_set_size`, an early-warning gauge for the cap
    /// configured by `[rocks].max_utxos`. When this gauge approaches the
    /// cap, the LRU starts evicting and balance lookups fall through to
    /// the storage layer.
    async fn utxo_set_size(&self) -> Option<usize> {
        None
    }

    /// Inject the cross-ledger [`BridgeLockResolver`] **and this adapter's own
    /// ledger id** used to reconcile a `BridgeMint` against its source
    /// `BridgeLock` (amount / asset / recipient / destination ledger). Wired
    /// together by `LedgerManager` after bootstrap so every ledger adapter can
    /// see all ledgers AND knows which ledger it is (so it can assert a mint is
    /// applied on the lock's intended destination ledger). Default no-op for
    /// mocks. A production adapter that receives `BridgeMint` blocks but has NO
    /// resolver wired rejects them (fail-closed; see `CoreAdapter`).
    /// (audit rang 3, B3)
    fn set_bridge_resolver(
        &self,
        _resolver: Arc<dyn BridgeLockResolver>,
        _ledger_id: String,
    ) {
    }
}
