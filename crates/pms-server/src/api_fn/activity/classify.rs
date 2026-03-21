use super::ActivityItem;
use pms_types_payload::PlainPayload;

// ═══════════════════════════════════════════════════════════════════
// Classification logic
// ═══════════════════════════════════════════════════════════════════

/// Classify a PlainPayload into ActivityItems for a given address.
/// Async version with UTXO lookup for sender detection.
pub(crate) async fn classify_activity(
    plain: &PlainPayload,
    addr: &str,
    adapter: &dyn pms_interface::NetDagAdapter,
) -> Vec<ActivityItem> {
    match plain {
        PlainPayload::Mint { outputs } => {
            let my_outputs: Vec<_> = outputs.iter().filter(|o| o.address == addr).collect();
            my_outputs
                .iter()
                .map(|o| ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "mint".to_string(),
                    direction: "in".to_string(),
                    amount: Some(o.amount.clone()),
                    asset_id: o.asset_id.clone(),
                    counterparty: None,
                    ledger_id: None,
                    payload: serde_json::to_value(plain).unwrap_or_default(),
                })
                .collect()
        }

        PlainPayload::TxUtxo(tx) => {
            // Resolve sender from inputs via UTXO cache
            let sender_addr = resolve_sender(tx, adapter).await;
            let is_sender = sender_addr.as_deref() == Some(addr);
            let is_receiver = tx.outputs.iter().any(|o| o.address == addr);
            let has_other_recipients = tx.outputs.iter().any(|o| o.address != addr);

            let payload_val = serde_json::to_value(tx).unwrap_or_default();
            let mut items = Vec::new();

            if is_sender && is_receiver && !has_other_recipients {
                // True self-transfer (consolidation): ALL outputs go back to sender
                let net: rust_decimal::Decimal = tx
                    .outputs
                    .iter()
                    .filter(|o| o.address == addr)
                    .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
                    .sum();
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "transfer_self".to_string(),
                    direction: "info".to_string(),
                    amount: Some(net.to_string()),
                    asset_id: tx.outputs.first().and_then(|o| o.asset_id.clone()),
                    counterparty: None,
                    ledger_id: None,
                    payload: payload_val,
                });
            } else if is_sender {
                // Transfer out (with or without change back to sender)
                let recipient = tx
                    .outputs
                    .iter()
                    .find(|o| o.address != addr)
                    .map(|o| o.address.clone());
                let sent_amount: rust_decimal::Decimal = tx
                    .outputs
                    .iter()
                    .filter(|o| o.address != addr)
                    .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
                    .sum();
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "transfer_out".to_string(),
                    direction: "out".to_string(),
                    amount: Some(sent_amount.to_string()),
                    asset_id: tx.outputs.first().and_then(|o| o.asset_id.clone()),
                    counterparty: recipient,
                    ledger_id: None,
                    payload: payload_val,
                });
            } else if is_receiver {
                // Received from someone
                let received: rust_decimal::Decimal = tx
                    .outputs
                    .iter()
                    .filter(|o| o.address == addr)
                    .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
                    .sum();
                // Detect fee outputs: if the address only receives exactly the
                // fee amount, it is the fee collector — classify as fee_received.
                let activity_type = if is_fee_output_only(tx, addr) {
                    "fee_received"
                } else {
                    "transfer_in"
                };
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: activity_type.to_string(),
                    direction: "in".to_string(),
                    amount: Some(received.to_string()),
                    asset_id: tx
                        .outputs
                        .iter()
                        .find(|o| o.address == addr)
                        .and_then(|o| o.asset_id.clone()),
                    counterparty: sender_addr,
                    ledger_id: None,
                    payload: payload_val,
                });
            }
            items
        }

        PlainPayload::Reward {
            fee_outputs,
            reward_outputs,
            ..
        } => {
            let payload_val = serde_json::to_value(plain).unwrap_or_default();
            let mut items = Vec::new();
            for o in fee_outputs.iter().filter(|o| o.address == addr) {
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "fee_received".to_string(),
                    direction: "in".to_string(),
                    amount: Some(o.amount.clone()),
                    asset_id: None,
                    counterparty: None,
                    ledger_id: None,
                    payload: payload_val.clone(),
                });
            }
            for o in reward_outputs.iter().filter(|o| o.address == addr) {
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "reward".to_string(),
                    direction: "in".to_string(),
                    amount: Some(o.amount.clone()),
                    asset_id: None,
                    counterparty: None,
                    ledger_id: None,
                    payload: payload_val.clone(),
                });
            }
            items
        }

        PlainPayload::Nft(action) => {
            let payload_val = serde_json::to_value(action).unwrap_or_default();
            match action {
                pms_types_nft::NftAction::Mint { creator, .. } if creator == addr => {
                    vec![ActivityItem {
                        block_id: String::new(),
                        ts_ms: 0,
                        activity_type: "nft_mint".to_string(),
                        direction: "in".to_string(),
                        amount: None,
                        asset_id: None,
                        counterparty: None,
                        ledger_id: None,
                        payload: payload_val,
                    }]
                }
                pms_types_nft::NftAction::Transfer { from, to, .. } => {
                    if to == addr {
                        vec![ActivityItem {
                            block_id: String::new(),
                            ts_ms: 0,
                            activity_type: "nft_transfer_in".to_string(),
                            direction: "in".to_string(),
                            amount: None,
                            asset_id: None,
                            counterparty: Some(from.clone()),
                            ledger_id: None,
                            payload: payload_val,
                        }]
                    } else if from == addr {
                        vec![ActivityItem {
                            block_id: String::new(),
                            ts_ms: 0,
                            activity_type: "nft_transfer_out".to_string(),
                            direction: "out".to_string(),
                            amount: None,
                            asset_id: None,
                            counterparty: Some(to.clone()),
                            ledger_id: None,
                            payload: payload_val,
                        }]
                    } else {
                        vec![]
                    }
                }
                pms_types_nft::NftAction::Burn { burner, .. }
                | pms_types_nft::NftAction::BatchBurn { burner, .. }
                    if burner == addr =>
                {
                    vec![ActivityItem {
                        block_id: String::new(),
                        ts_ms: 0,
                        activity_type: "nft_burn".to_string(),
                        direction: "out".to_string(),
                        amount: None,
                        asset_id: None,
                        counterparty: None,
                        ledger_id: None,
                        payload: payload_val,
                    }]
                }
                pms_types_nft::NftAction::Use { user, .. } if user == addr => {
                    vec![ActivityItem {
                        block_id: String::new(),
                        ts_ms: 0,
                        activity_type: "nft_use".to_string(),
                        direction: "info".to_string(),
                        amount: None,
                        asset_id: None,
                        counterparty: None,
                        ledger_id: None,
                        payload: payload_val,
                    }]
                }
                _ => vec![],
            }
        }

        PlainPayload::TokenCreate(meta) if meta.creator == addr => {
            vec![ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "token_create".to_string(),
                direction: "info".to_string(),
                amount: None,
                asset_id: Some(meta.asset_id.clone()),
                counterparty: None,
                ledger_id: None,
                payload: serde_json::to_value(meta).unwrap_or_default(),
            }]
        }

        PlainPayload::BridgeLock {
            dest_address,
            amount,
            asset_id,
            ..
        } if dest_address == addr => {
            vec![ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "bridge_lock_in".to_string(),
                direction: "in".to_string(),
                amount: Some(amount.clone()),
                asset_id: asset_id.clone(),
                counterparty: None,
                ledger_id: None,
                payload: serde_json::to_value(plain).unwrap_or_default(),
            }]
        }

        PlainPayload::BridgeMint { outputs, .. } => outputs
            .iter()
            .filter(|o| o.address == addr)
            .map(|o| ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "bridge_mint".to_string(),
                direction: "in".to_string(),
                amount: Some(o.amount.clone()),
                asset_id: o.asset_id.clone(),
                counterparty: None,
                ledger_id: None,
                payload: serde_json::to_value(plain).unwrap_or_default(),
            })
            .collect(),

        PlainPayload::Freeze {
            address, reason, ..
        } if address == addr => {
            vec![ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "freeze".to_string(),
                direction: "info".to_string(),
                amount: None,
                asset_id: None,
                counterparty: None,
                ledger_id: None,
                payload: serde_json::json!({ "reason": reason }),
            }]
        }

        PlainPayload::Unfreeze {
            address, reason, ..
        } if address == addr => {
            vec![ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "unfreeze".to_string(),
                direction: "info".to_string(),
                amount: None,
                asset_id: None,
                counterparty: None,
                ledger_id: None,
                payload: serde_json::json!({ "reason": reason }),
            }]
        }

        PlainPayload::Seize {
            from_address,
            outputs,
            reason,
            ..
        } => {
            let payload_val = serde_json::json!({ "reason": reason });
            let mut items = Vec::new();
            if from_address == addr {
                let total: rust_decimal::Decimal = outputs
                    .iter()
                    .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
                    .sum();
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "seized".to_string(),
                    direction: "out".to_string(),
                    amount: Some(total.to_string()),
                    asset_id: None,
                    counterparty: None,
                    ledger_id: None,
                    payload: payload_val.clone(),
                });
            }
            for o in outputs.iter().filter(|o| o.address == addr) {
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "seize_received".to_string(),
                    direction: "in".to_string(),
                    amount: Some(o.amount.clone()),
                    asset_id: None,
                    counterparty: Some(from_address.clone()),
                    ledger_id: None,
                    payload: payload_val.clone(),
                });
            }
            items
        }

        PlainPayload::Reverse {
            outputs, reason, ..
        } => {
            let payload_val = serde_json::json!({ "reason": reason });
            outputs
                .iter()
                .filter(|o| o.address == addr)
                .map(|o| ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "reverse_received".to_string(),
                    direction: "in".to_string(),
                    amount: Some(o.amount.clone()),
                    asset_id: o.asset_id.clone(),
                    counterparty: None,
                    ledger_id: None,
                    payload: payload_val.clone(),
                })
                .collect()
        }

        _ => vec![],
    }
}

/// Sync version for SSE (no UTXO lookup, outputs-only for TxUtxo sender detection).
pub(crate) fn classify_activity_sync(plain: &PlainPayload, addr: &str) -> Vec<ActivityItem> {
    match plain {
        PlainPayload::Mint { outputs } => outputs
            .iter()
            .filter(|o| o.address == addr)
            .map(|o| ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "mint".to_string(),
                direction: "in".to_string(),
                amount: Some(o.amount.clone()),
                asset_id: o.asset_id.clone(),
                counterparty: None,
                ledger_id: None,
                payload: serde_json::to_value(plain).unwrap_or_default(),
            })
            .collect(),

        PlainPayload::TxUtxo(tx) => {
            // In SSE mode, we can't do async UTXO lookups.
            // We classify based on outputs only.
            let is_receiver = tx.outputs.iter().any(|o| o.address == addr);
            if is_receiver {
                let received: rust_decimal::Decimal = tx
                    .outputs
                    .iter()
                    .filter(|o| o.address == addr)
                    .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
                    .sum();
                let activity_type = if is_fee_output_only(tx, addr) {
                    "fee_received"
                } else {
                    "transfer_in"
                };
                vec![ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: activity_type.to_string(),
                    direction: "in".to_string(),
                    amount: Some(received.to_string()),
                    asset_id: tx
                        .outputs
                        .iter()
                        .find(|o| o.address == addr)
                        .and_then(|o| o.asset_id.clone()),
                    counterparty: None,
                    ledger_id: None,
                    payload: serde_json::to_value(tx).unwrap_or_default(),
                }]
            } else {
                vec![]
            }
        }

        PlainPayload::Reward {
            fee_outputs,
            reward_outputs,
            ..
        } => {
            let payload_val = serde_json::to_value(plain).unwrap_or_default();
            let mut items = Vec::new();
            for o in fee_outputs.iter().filter(|o| o.address == addr) {
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "fee_received".to_string(),
                    direction: "in".to_string(),
                    amount: Some(o.amount.clone()),
                    asset_id: None,
                    counterparty: None,
                    ledger_id: None,
                    payload: payload_val.clone(),
                });
            }
            for o in reward_outputs.iter().filter(|o| o.address == addr) {
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "reward".to_string(),
                    direction: "in".to_string(),
                    amount: Some(o.amount.clone()),
                    asset_id: None,
                    counterparty: None,
                    ledger_id: None,
                    payload: payload_val.clone(),
                });
            }
            items
        }

        PlainPayload::Nft(action) => {
            let payload_val = serde_json::to_value(action).unwrap_or_default();
            match action {
                pms_types_nft::NftAction::Mint { creator, .. } if creator == addr => {
                    vec![ActivityItem {
                        block_id: String::new(),
                        ts_ms: 0,
                        activity_type: "nft_mint".to_string(),
                        direction: "in".to_string(),
                        amount: None,
                        asset_id: None,
                        counterparty: None,
                        ledger_id: None,
                        payload: payload_val,
                    }]
                }
                pms_types_nft::NftAction::Transfer { from, to, .. } => {
                    if to == addr {
                        vec![ActivityItem {
                            block_id: String::new(),
                            ts_ms: 0,
                            activity_type: "nft_transfer_in".to_string(),
                            direction: "in".to_string(),
                            amount: None,
                            asset_id: None,
                            counterparty: Some(from.clone()),
                            ledger_id: None,
                            payload: payload_val,
                        }]
                    } else if from == addr {
                        vec![ActivityItem {
                            block_id: String::new(),
                            ts_ms: 0,
                            activity_type: "nft_transfer_out".to_string(),
                            direction: "out".to_string(),
                            amount: None,
                            asset_id: None,
                            counterparty: Some(to.clone()),
                            ledger_id: None,
                            payload: payload_val,
                        }]
                    } else {
                        vec![]
                    }
                }
                pms_types_nft::NftAction::Burn { burner, .. }
                | pms_types_nft::NftAction::BatchBurn { burner, .. }
                    if burner == addr =>
                {
                    vec![ActivityItem {
                        block_id: String::new(),
                        ts_ms: 0,
                        activity_type: "nft_burn".to_string(),
                        direction: "out".to_string(),
                        amount: None,
                        asset_id: None,
                        counterparty: None,
                        ledger_id: None,
                        payload: payload_val,
                    }]
                }
                pms_types_nft::NftAction::Use { user, .. } if user == addr => {
                    vec![ActivityItem {
                        block_id: String::new(),
                        ts_ms: 0,
                        activity_type: "nft_use".to_string(),
                        direction: "info".to_string(),
                        amount: None,
                        asset_id: None,
                        counterparty: None,
                        ledger_id: None,
                        payload: payload_val,
                    }]
                }
                _ => vec![],
            }
        }

        PlainPayload::Freeze {
            address, reason, ..
        } if address == addr => {
            vec![ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "freeze".to_string(),
                direction: "info".to_string(),
                amount: None,
                asset_id: None,
                counterparty: None,
                ledger_id: None,
                payload: serde_json::json!({ "reason": reason }),
            }]
        }

        PlainPayload::Unfreeze {
            address, reason, ..
        } if address == addr => {
            vec![ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "unfreeze".to_string(),
                direction: "info".to_string(),
                amount: None,
                asset_id: None,
                counterparty: None,
                ledger_id: None,
                payload: serde_json::json!({ "reason": reason }),
            }]
        }

        PlainPayload::Seize {
            from_address,
            outputs,
            reason,
            ..
        } => {
            let payload_val = serde_json::json!({ "reason": reason });
            let mut items = Vec::new();
            if from_address == addr {
                let total: rust_decimal::Decimal = outputs
                    .iter()
                    .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
                    .sum();
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "seized".to_string(),
                    direction: "out".to_string(),
                    amount: Some(total.to_string()),
                    asset_id: None,
                    counterparty: None,
                    ledger_id: None,
                    payload: payload_val.clone(),
                });
            }
            for o in outputs.iter().filter(|o| o.address == addr) {
                items.push(ActivityItem {
                    block_id: String::new(),
                    ts_ms: 0,
                    activity_type: "seize_received".to_string(),
                    direction: "in".to_string(),
                    amount: Some(o.amount.clone()),
                    asset_id: None,
                    counterparty: Some(from_address.clone()),
                    ledger_id: None,
                    payload: payload_val.clone(),
                });
            }
            items
        }

        PlainPayload::Reverse {
            outputs, reason, ..
        } => outputs
            .iter()
            .filter(|o| o.address == addr)
            .map(|o| ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "reverse_received".to_string(),
                direction: "in".to_string(),
                amount: Some(o.amount.clone()),
                asset_id: o.asset_id.clone(),
                counterparty: None,
                ledger_id: None,
                payload: serde_json::json!({ "reason": reason }),
            })
            .collect(),

        PlainPayload::TokenCreate(meta) if meta.creator == addr => {
            vec![ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "token_create".to_string(),
                direction: "info".to_string(),
                amount: None,
                asset_id: Some(meta.asset_id.clone()),
                counterparty: None,
                ledger_id: None,
                payload: serde_json::to_value(meta).unwrap_or_default(),
            }]
        }

        PlainPayload::BridgeLock {
            dest_address,
            amount,
            asset_id,
            ..
        } if dest_address == addr => {
            vec![ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "bridge_lock_in".to_string(),
                direction: "in".to_string(),
                amount: Some(amount.clone()),
                asset_id: asset_id.clone(),
                counterparty: None,
                ledger_id: None,
                payload: serde_json::to_value(plain).unwrap_or_default(),
            }]
        }

        PlainPayload::BridgeMint { outputs, .. } => outputs
            .iter()
            .filter(|o| o.address == addr)
            .map(|o| ActivityItem {
                block_id: String::new(),
                ts_ms: 0,
                activity_type: "bridge_mint".to_string(),
                direction: "in".to_string(),
                amount: Some(o.amount.clone()),
                asset_id: o.asset_id.clone(),
                counterparty: None,
                ledger_id: None,
                payload: serde_json::to_value(plain).unwrap_or_default(),
            })
            .collect(),

        _ => vec![],
    }
}

// ═══════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════

pub(crate) fn parse_type_filter(filter: &Option<String>) -> Vec<&str> {
    match filter {
        Some(s) if !s.is_empty() => s.split(',').map(|s| s.trim()).collect(),
        _ => vec![],
    }
}

/// Check if an address is solely a fee recipient in a TxUtxo.
///
/// Returns `true` when **all** outputs addressed to `addr` account for exactly the
/// transaction fee — i.e. the address only appears in the transaction as the fee
/// collector, not as a regular transfer recipient.
///
/// This is the case when wallet_factory / prepare_tx add an explicit fee output
/// (`amount == tx.fee`) to the admin/treasury wallet inside the TxUtxo.
pub(crate) fn is_fee_output_only(tx: &pms_types::Transaction, addr: &str) -> bool {
    let fee = match tx.fee.parse::<rust_decimal::Decimal>() {
        Ok(f) if f > rust_decimal::Decimal::ZERO => f,
        _ => return false,
    };

    let addr_total: rust_decimal::Decimal = tx
        .outputs
        .iter()
        .filter(|o| o.address == addr)
        .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
        .sum();

    // The address receives exactly the fee amount — it's a fee collector,
    // not a regular transfer recipient.
    addr_total == fee
}

/// Resolve the sender address from transaction inputs via UTXO cache.
pub(crate) async fn resolve_sender(
    tx: &pms_types::Transaction,
    adapter: &dyn pms_interface::NetDagAdapter,
) -> Option<String> {
    if let Some(first_input) = tx.inputs.first() {
        if let Some(utxo) = adapter.get_utxo(&first_input.out).await {
            return Some(utxo.address);
        }
    }
    None
}
