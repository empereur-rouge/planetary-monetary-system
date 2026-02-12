use anyhow::Result;
use rust_decimal::Decimal;
use std::str::FromStr;

use pms_storage::{DagStorage, PutResult};

use pms_types::{OutputId, PayloadEnvelope, PlainPayload, TxOutput};
use pms_wallet::{SelectedInput, Wallet};
use pms_wire::{WireBlock, WireMeta};

// adapte à ton type
use crate::{TestCtx, forge_signed_wire_block_for_test};

pub async fn mint_to_wallet_and_get_inputs(
    ctx: &TestCtx,
    to_wallet: &Wallet,
    amount: &str,
) -> Result<(Vec<SelectedInput>, Decimal)> {
    // 0) meta réseau + hrp
    let meta = WireMeta::from(&ctx.settings);
    let hrp = ctx.settings.address.hrp.as_str();

    // 1) parents = tips (fallback genesis)
    let mut parents = ctx.store.top_tips(2).await?;
    if parents.is_empty() {
        let ids = ctx.store.all_block_ids().await?;
        let g = ids
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("empty DAG"))?;
        parents = vec![g];
    }
    parents.sort();
    parents.dedup();

    // 2) Mint output vers l’adresse du wallet cible
    let to_addr = to_wallet.get_address(hrp);
    let outputs = vec![TxOutput {
        address: to_addr,
        amount: amount.to_string(),
        asset_id: None,
    }];

    let payload = PayloadEnvelope::Plain(PlainPayload::Mint { outputs });

    // 3) Forge un WireBlock signé (utilise ton helper existant)
    // nonce = 1 (ou n’importe, tant que stable pour le test)
    let wb: WireBlock = forge_signed_wire_block_for_test(
        parents,
        &meta,
        &ctx.node_wallet, // 👈 le minter = node_wallet du ctx
        1,
        Some(payload),
    );

    // 4) Persist via le chemin officiel (validation + rocks + dag)
    let res = ctx.srv.adapter_arc().persist_block(&wb).await?;
    match res {
        PutResult::Inserted | PutResult::AlreadyExists => {}
        PutResult::Rejected(r) => anyhow::bail!("mint rejected: {r}"),
    }

    // 5) Pour un Mint: l’UTXO créé est (txid=block_id, index=0)
    let input = SelectedInput {
        id: OutputId {
            txid: wb.id.clone(),
            index: 0,
        },
        amount: amount.to_string(),
    };

    let minted = Decimal::from_str(amount)?;
    Ok((vec![input], minted))
}
