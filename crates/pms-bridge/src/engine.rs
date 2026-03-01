use anyhow::{Result, bail};
use pms_ledger::LedgerManager;
use pms_storage::PutResult;
use pms_types_payload::{PayloadEnvelope, PlainPayload};
use pms_types_transaction::{TxInput, TxOutput};
use pms_utils::{check_pow_leading_zero_bits, compute_block_id};
use pms_wallet::utils::signing_wire::canonical_wireblock_message;
use pms_wallet::{SignerBackend, Wallet};
use pms_wire::WireBlock;
use rust_decimal::Decimal;
use std::str::FromStr;
use std::sync::Arc;

use crate::auth::BridgeAuth;
use crate::store::BridgeStore;
use crate::types::{
    BridgeDisableRequest, BridgeEnableRequest, BridgeLink, BridgeTransferRequest,
    BridgeTransferResponse,
};

/// Orchestre les opérations de pont cross-ledger.
pub struct BridgeEngine {
    pub ledger_mgr: Arc<LedgerManager>,
    pub bridge_store: BridgeStore,
    pub node_wallet: Arc<Wallet>,
}

impl BridgeEngine {
    pub fn new(
        ledger_mgr: Arc<LedgerManager>,
        bridge_store: BridgeStore,
        node_wallet: Arc<Wallet>,
    ) -> Self {
        Self {
            ledger_mgr,
            bridge_store,
            node_wallet,
        }
    }

    /// Active un pont entre deux ledgers.
    pub fn enable_bridge(
        &self,
        req: &BridgeEnableRequest,
        is_admin: bool,
        signer_pubkey: Option<&str>,
    ) -> Result<BridgeLink> {
        let inst_a = self
            .ledger_mgr
            .get(&req.ledger_a)
            .ok_or_else(|| anyhow::anyhow!("ledger '{}' not found", req.ledger_a))?;
        let inst_b = self
            .ledger_mgr
            .get(&req.ledger_b)
            .ok_or_else(|| anyhow::anyhow!("ledger '{}' not found", req.ledger_b))?;

        if !BridgeAuth::can_enable(&inst_a.def, &inst_b.def, is_admin, signer_pubkey) {
            bail!("unauthorized: insufficient permissions to enable bridge");
        }

        if let Some(existing) = self
            .bridge_store
            .get_bridge_link(&req.ledger_a, &req.ledger_b)?
            && existing.enabled
        {
            bail!(
                "bridge already enabled between '{}' and '{}'",
                req.ledger_a,
                req.ledger_b
            );
        }

        let now = now_ms();
        let (a, b) = if req.ledger_a <= req.ledger_b {
            (req.ledger_a.clone(), req.ledger_b.clone())
        } else {
            (req.ledger_b.clone(), req.ledger_a.clone())
        };

        let link = BridgeLink {
            ledger_a: a,
            ledger_b: b,
            direction: req.direction.clone(),
            enabled: true,
            created_at: now,
            disabled_at: None,
            authorized_by: signer_pubkey
                .map(|s| vec![s.to_string()])
                .unwrap_or_default(),
        };

        self.bridge_store.set_bridge_link(&link)?;

        tracing::info!(
            ledger_a = %link.ledger_a,
            ledger_b = %link.ledger_b,
            direction = ?link.direction,
            "Bridge enabled"
        );

        Ok(link)
    }

    /// Désactive un pont entre deux ledgers.
    pub fn disable_bridge(
        &self,
        req: &BridgeDisableRequest,
        is_admin: bool,
        signer_pubkey: Option<&str>,
    ) -> Result<BridgeLink> {
        let inst_a = self
            .ledger_mgr
            .get(&req.ledger_a)
            .ok_or_else(|| anyhow::anyhow!("ledger '{}' not found", req.ledger_a))?;
        let inst_b = self
            .ledger_mgr
            .get(&req.ledger_b)
            .ok_or_else(|| anyhow::anyhow!("ledger '{}' not found", req.ledger_b))?;

        if !BridgeAuth::can_disable(&inst_a.def, &inst_b.def, is_admin, signer_pubkey) {
            bail!("unauthorized: insufficient permissions to disable bridge");
        }

        self.bridge_store
            .disable_bridge_link(&req.ledger_a, &req.ledger_b)?;

        let link = self
            .bridge_store
            .get_bridge_link(&req.ledger_a, &req.ledger_b)?
            .expect("link just disabled");

        tracing::info!(
            ledger_a = %link.ledger_a,
            ledger_b = %link.ledger_b,
            "Bridge disabled"
        );

        Ok(link)
    }

    /// Liste tous les ponts.
    pub fn list_bridges(&self) -> Result<Vec<BridgeLink>> {
        self.bridge_store.list_bridge_links()
    }

    /// Statut d'un transfert bridge (par lock_block_id).
    pub fn transfer_status(&self, lock_block_id: &str) -> Result<Option<String>> {
        self.bridge_store.get_bridge_mint_for_lock(lock_block_id)
    }

    /// Exécute un transfert cross-ledger complet :
    /// 1. Vérifie le bridge link actif
    /// 2. Sélection de coins sur le ledger source
    /// 3. Crée BridgeLock sur source (consomme UTXOs)
    /// 4. Crée BridgeMint sur dest (crée UTXOs)
    /// 5. Marque le lock comme consommé (anti-replay)
    pub async fn execute_transfer(
        &self,
        req: &BridgeTransferRequest,
    ) -> Result<BridgeTransferResponse> {
        // 1) Vérifier que le bridge link est actif
        if !self
            .bridge_store
            .is_bridge_enabled(&req.from_ledger, &req.to_ledger)?
        {
            bail!(
                "no active bridge from '{}' to '{}'",
                req.from_ledger,
                req.to_ledger
            );
        }

        // 2) Récupérer les instances des deux ledgers
        let source = self
            .ledger_mgr
            .get(&req.from_ledger)
            .ok_or_else(|| anyhow::anyhow!("source ledger '{}' not found", req.from_ledger))?;
        let dest = self
            .ledger_mgr
            .get(&req.to_ledger)
            .ok_or_else(|| anyhow::anyhow!("dest ledger '{}' not found", req.to_ledger))?;

        let amount_dec = Decimal::from_str(&req.amount)
            .map_err(|e| anyhow::anyhow!("invalid amount '{}': {}", req.amount, e))?;
        if amount_dec <= Decimal::ZERO {
            bail!("amount must be positive");
        }

        // 3) Coin selection sur le ledger source
        let all_utxos = source.adapter.utxos_by_address(&req.from_address).await;
        let filtered: Vec<_> = all_utxos
            .into_iter()
            .filter(|(_, tx_out)| tx_out.asset_id == req.asset_id)
            .collect();

        if filtered.is_empty() {
            bail!(
                "no UTXOs found for address '{}' on ledger '{}'",
                req.from_address,
                req.from_ledger
            );
        }

        // Parse amounts and sort largest-first
        let mut utxo_list: Vec<_> = filtered
            .into_iter()
            .filter_map(|(out_id, tx_out)| {
                Decimal::from_str(&tx_out.amount)
                    .ok()
                    .map(|amt| (out_id, tx_out, amt))
            })
            .collect();
        utxo_list.sort_by(|a, b| b.2.cmp(&a.2));

        let mut selected = Vec::new();
        let mut selected_sum = Decimal::ZERO;
        for (out_id, tx_out, amt) in utxo_list {
            if selected_sum >= amount_dec {
                break;
            }
            selected_sum += amt;
            selected.push((out_id, tx_out));
        }

        if selected_sum < amount_dec {
            bail!(
                "insufficient balance: need {}, have {} on ledger '{}'",
                req.amount,
                selected_sum,
                req.from_ledger
            );
        }

        // Build TxInputs
        let inputs: Vec<TxInput> = selected
            .iter()
            .map(|(out_id, _)| TxInput {
                out: out_id.clone(),
            })
            .collect();

        // 4) Construire et persister le BridgeLock sur le ledger source
        let lock_payload = PayloadEnvelope::Plain(PlainPayload::BridgeLock {
            inputs: inputs.clone(),
            amount: req.amount.clone(),
            asset_id: req.asset_id.clone(),
            dest_ledger_id: req.to_ledger.clone(),
            dest_address: req.to_address.clone(),
        });

        let lock_wb = self
            .build_and_sign_block(
                &source.adapter,
                &source.def.network_id,
                source.def.protocol_version,
                lock_payload,
            )
            .await?;

        let lock_block_id = lock_wb.id.clone();

        match source.adapter.persist_block(&lock_wb).await? {
            PutResult::Inserted => {}
            PutResult::AlreadyExists => bail!("BridgeLock block already exists (race condition)"),
            PutResult::Rejected(r) => bail!("BridgeLock rejected: {}", r),
        }

        tracing::info!(
            lock_block_id = %lock_block_id,
            from = %req.from_ledger,
            to = %req.to_ledger,
            amount = %req.amount,
            "BridgeLock persisted"
        );

        // 5) Anti-replay check
        if self.bridge_store.is_bridge_lock_consumed(&lock_block_id)? {
            bail!("BridgeLock {} already consumed", lock_block_id);
        }

        // 6) Construire et persister le BridgeMint sur le ledger destination
        let mint_outputs = vec![TxOutput {
            address: req.to_address.clone(),
            amount: req.amount.clone(),
            asset_id: req.asset_id.clone(),
        }];

        let mint_payload = PayloadEnvelope::Plain(PlainPayload::BridgeMint {
            outputs: mint_outputs,
            lock_block_id: lock_block_id.clone(),
            source_ledger_id: req.from_ledger.clone(),
        });

        let mint_wb = self
            .build_and_sign_block(
                &dest.adapter,
                &dest.def.network_id,
                dest.def.protocol_version,
                mint_payload,
            )
            .await?;

        let mint_block_id = mint_wb.id.clone();

        match dest.adapter.persist_block(&mint_wb).await? {
            PutResult::Inserted => {}
            PutResult::AlreadyExists => {
                bail!("BridgeMint block already exists (race condition)")
            }
            PutResult::Rejected(r) => bail!("BridgeMint rejected: {}", r),
        }

        tracing::info!(
            mint_block_id = %mint_block_id,
            lock_block_id = %lock_block_id,
            "BridgeMint persisted"
        );

        // 7) Marquer le lock comme consommé
        self.bridge_store
            .mark_bridge_lock_consumed(&lock_block_id, &mint_block_id)?;

        Ok(BridgeTransferResponse {
            lock_block_id,
            mint_block_id,
            from_ledger: req.from_ledger.clone(),
            to_ledger: req.to_ledger.clone(),
            amount: req.amount.clone(),
            asset_id: req.asset_id.clone(),
        })
    }

    /// Construit un WireBlock signé par le coordinator wallet, prêt à être persisté.
    async fn build_and_sign_block(
        &self,
        adapter: &Arc<dyn pms_interface::NetDagAdapter>,
        network_id: &str,
        protocol_version: u32,
        payload: PayloadEnvelope,
    ) -> Result<WireBlock> {
        // Single-writer mode: 1 parent
        let mut parents = adapter.top_tips(1).await?;
        if parents.is_empty() {
            bail!("no tips available on ledger");
        }
        parents.truncate(1);

        let payload_opt = Some(payload);
        let mut block_id = compute_block_id(&parents, &payload_opt, 0);
        let mut nonce: u64 = 0;

        // PoW si requis
        let min_bits = adapter.min_pow_leading_zero_bits();
        if min_bits > 0 {
            while !check_pow_leading_zero_bits(&block_id, min_bits) {
                nonce += 1;
                block_id = compute_block_id(&parents, &payload_opt, nonce);
            }
        }

        let payload_json = payload_opt
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;

        let mut wb = WireBlock {
            id: block_id,
            parents,
            payload_json,
            nonce,
            network_id: network_id.to_string(),
            protocol_version: protocol_version as u16,
            signer_pk_hex: self.node_wallet.encoded_public_key(),
            signature_hex: String::new(),
            metadata: None,
        };

        let msg = canonical_wireblock_message(&wb);
        wb.signature_hex = self.node_wallet.sign(&msg)?;

        Ok(wb)
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
