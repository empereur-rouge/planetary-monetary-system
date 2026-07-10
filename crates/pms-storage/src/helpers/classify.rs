use crate::activity_item::StoredActivityItem;
use pms_types_payload::PlainPayload;
use std::collections::HashMap;

/// Extract all addresses involved in a `PlainPayload`.
///
/// Replicates `pms_wallet::history::collect_involved_addresses` to avoid a
/// circular dependency (pms-wallet depends on pms-storage).
pub fn extract_involved_addresses(plain: &PlainPayload) -> Vec<String> {
    let mut addrs = Vec::new();
    match plain {
        PlainPayload::Mint { outputs }
        | PlainPayload::CustodialMint { outputs, .. } => {
            addrs.extend(outputs.iter().map(|o| o.address.clone()));
        }
        PlainPayload::TxUtxo(tx) => {
            addrs.extend(tx.outputs.iter().map(|o| o.address.clone()));
        }
        // MarketSettle: seller, buyer AND every output recipient (incl. the
        // royalty beneficiary) so the sale surfaces in each party's activity.
        PlainPayload::MarketSettle { tx, seller, buyer, .. } => {
            addrs.push(seller.clone());
            addrs.push(buyer.clone());
            addrs.extend(tx.outputs.iter().map(|o| o.address.clone()));
        }
        PlainPayload::TokenBurn { owner, .. } => addrs.push(owner.clone()),
        PlainPayload::Reward {
            fee_outputs,
            reward_outputs,
            ..
        } => {
            addrs.extend(fee_outputs.iter().map(|o| o.address.clone()));
            addrs.extend(reward_outputs.iter().map(|o| o.address.clone()));
        }
        PlainPayload::Nft(action) => match action {
            pms_types_nft::NftAction::Mint { creator, .. } => addrs.push(creator.clone()),
            pms_types_nft::NftAction::Transfer { from, to, .. } => {
                addrs.push(from.clone());
                addrs.push(to.clone());
            }
            pms_types_nft::NftAction::Use { user, .. } => addrs.push(user.clone()),
            pms_types_nft::NftAction::Burn { burner, .. } => addrs.push(burner.clone()),
            pms_types_nft::NftAction::BatchBurn { burner, .. } => addrs.push(burner.clone()),
        },
        PlainPayload::TokenCreate(meta) => addrs.push(meta.creator.clone()),
        PlainPayload::RoyaltyUpdate { royalty_beneficiary, .. } => {
            if let Some(b) = royalty_beneficiary {
                addrs.push(b.clone());
            }
        }
        PlainPayload::BridgeLock { dest_address, .. } => addrs.push(dest_address.clone()),
        PlainPayload::BridgeMint { outputs, .. } => {
            addrs.extend(outputs.iter().map(|o| o.address.clone()));
        }
        PlainPayload::Freeze { address, .. } | PlainPayload::Unfreeze { address, .. } => {
            addrs.push(address.clone());
        }
        PlainPayload::Seize {
            from_address,
            outputs,
            ..
        } => {
            addrs.push(from_address.clone());
            addrs.extend(outputs.iter().map(|o| o.address.clone()));
        }
        PlainPayload::Reverse { outputs, .. } => {
            addrs.extend(outputs.iter().map(|o| o.address.clone()));
        }
        _other => {
            tracing::trace!("extract_addresses_from_payload: skipped variant (no extractable addresses)");
        }
    }
    addrs.dedup();
    addrs
}

/// Activity categories for the `addr_type_activity` CF.
/// Each variant maps to a 1-byte discriminant used as part of the key.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityCategory {
    Mint = 1,
    Transfer = 2,
    Fee = 3,
    Reward = 4,
    Nft = 5,
    TokenCreate = 6,
    Bridge = 7,
    Compliance = 8,
    Reverse = 9,
    Burn = 10,
}

impl ActivityCategory {
    /// Map an API `filter_type` string to a category.
    pub fn from_filter_type(s: &str) -> Option<Self> {
        match s {
            "mint" => Some(Self::Mint),
            "transfer_in" | "transfer_out" | "transfer_self" => Some(Self::Transfer),
            "fee_received" => Some(Self::Fee),
            "reward" => Some(Self::Reward),
            "nft_mint" | "nft_transfer_in" | "nft_transfer_out" | "nft_burn" | "nft_use" => {
                Some(Self::Nft)
            }
            "token_create" => Some(Self::TokenCreate),
            "token_burn" => Some(Self::Burn),
            "bridge_lock_in" | "bridge_mint" => Some(Self::Bridge),
            "freeze" | "unfreeze" | "seized" | "seize_received" => Some(Self::Compliance),
            "reverse_received" => Some(Self::Reverse),
            _ => None,
        }
    }

    pub fn as_byte(self) -> u8 {
        self as u8
    }
}

/// Extract `(address, category)` pairs from a `PlainPayload`.
///
/// Unlike `extract_involved_addresses`, this distinguishes between fee and
/// reward addresses in `Reward` blocks, assigning each its own category.
pub fn extract_involved_with_category(plain: &PlainPayload) -> Vec<(String, ActivityCategory)> {
    let mut out = Vec::new();
    match plain {
        PlainPayload::Mint { outputs }
        | PlainPayload::CustodialMint { outputs, .. } => {
            for o in outputs {
                out.push((o.address.clone(), ActivityCategory::Mint));
            }
        }
        PlainPayload::TxUtxo(tx) => {
            for o in &tx.outputs {
                // Fee outputs (amount == tx.fee) go under Fee category so that
                // ?type=transfer_in never returns fee-collector entries.
                let cat = if is_fee_output_only(tx, &o.address) {
                    ActivityCategory::Fee
                } else {
                    ActivityCategory::Transfer
                };
                out.push((o.address.clone(), cat));
            }
        }
        PlainPayload::Reward {
            fee_outputs,
            reward_outputs,
            ..
        } => {
            for o in fee_outputs {
                out.push((o.address.clone(), ActivityCategory::Fee));
            }
            for o in reward_outputs {
                out.push((o.address.clone(), ActivityCategory::Reward));
            }
        }
        PlainPayload::Nft(action) => match action {
            pms_types_nft::NftAction::Mint { creator, .. } => {
                out.push((creator.clone(), ActivityCategory::Nft));
            }
            pms_types_nft::NftAction::Transfer { from, to, .. } => {
                out.push((from.clone(), ActivityCategory::Nft));
                out.push((to.clone(), ActivityCategory::Nft));
            }
            pms_types_nft::NftAction::Use { user, .. } => {
                out.push((user.clone(), ActivityCategory::Nft));
            }
            pms_types_nft::NftAction::Burn { burner, .. } => {
                out.push((burner.clone(), ActivityCategory::Nft));
            }
            pms_types_nft::NftAction::BatchBurn { burner, .. } => {
                out.push((burner.clone(), ActivityCategory::Nft));
            }
        },
        PlainPayload::TokenCreate(meta) => {
            out.push((meta.creator.clone(), ActivityCategory::TokenCreate));
        }
        PlainPayload::RoyaltyUpdate { royalty_beneficiary, .. } => {
            if let Some(b) = royalty_beneficiary {
                out.push((b.clone(), ActivityCategory::TokenCreate));
            }
        }
        PlainPayload::TokenBurn { owner, .. } => {
            out.push((owner.clone(), ActivityCategory::Burn));
        }
        // MarketSettle: seller/buyer + all recipients under Transfer (the sale is
        // a value movement for each). Deduped downstream by the index writer.
        PlainPayload::MarketSettle { tx, seller, buyer, .. } => {
            out.push((seller.clone(), ActivityCategory::Transfer));
            out.push((buyer.clone(), ActivityCategory::Transfer));
            for o in &tx.outputs {
                out.push((o.address.clone(), ActivityCategory::Transfer));
            }
        }
        PlainPayload::BridgeLock { dest_address, .. } => {
            out.push((dest_address.clone(), ActivityCategory::Bridge));
        }
        PlainPayload::BridgeMint { outputs, .. } => {
            for o in outputs {
                out.push((o.address.clone(), ActivityCategory::Bridge));
            }
        }
        PlainPayload::Freeze { address, .. } | PlainPayload::Unfreeze { address, .. } => {
            out.push((address.clone(), ActivityCategory::Compliance));
        }
        PlainPayload::Seize {
            from_address,
            outputs,
            ..
        } => {
            out.push((from_address.clone(), ActivityCategory::Compliance));
            for o in outputs {
                out.push((o.address.clone(), ActivityCategory::Compliance));
            }
        }
        PlainPayload::Reverse { outputs, .. } => {
            for o in outputs {
                out.push((o.address.clone(), ActivityCategory::Reverse));
            }
        }
        _other => {
            tracing::trace!("addr_activity_pairs: skipped variant (no per-address activity)");
        }
    }
    out
}

// ═══════════════════════════════════════════════════════════════════════════════
// Pre-computed activity items (written at block persist time, read at query time)
// ═══════════════════════════════════════════════════════════════════════════════

/// Check if an address is solely a fee recipient in a TxUtxo.
///
/// Returns `true` when **all** outputs addressed to `addr` account for exactly the
/// transaction fee -- i.e. the address only appears in the transaction as the fee
/// collector, not as a regular transfer recipient.
pub fn is_fee_output_only(tx: &pms_types::Transaction, addr: &str) -> bool {
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

    addr_total == fee
}

/// Classify a `PlainPayload` into `StoredActivityItem`s for a given address.
///
/// This is the storage-layer equivalent of `classify_activity()` in pms-server.
/// It is **synchronous**: the caller must pre-resolve the sender address for TxUtxo
/// and pass it as `sender_addr`.
pub fn classify_for_storage(
    plain: &PlainPayload,
    addr: &str,
    sender_addr: Option<&str>,
) -> Vec<StoredActivityItem> {
    match plain {
        PlainPayload::Mint { outputs }
        | PlainPayload::CustodialMint { outputs, .. } => outputs
            .iter()
            .filter(|o| o.address == addr)
            .map(|o| StoredActivityItem {
                activity_type: "mint".into(),
                direction: "in".into(),
                amount: Some(o.amount.clone()),
                asset_id: o.asset_id.clone(),
                counterparty: None,
                payload: serde_json::to_value(plain).unwrap_or_default(),
            })
            .collect(),

        PlainPayload::TxUtxo(tx) => {
            let is_sender = sender_addr == Some(addr);
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
                items.push(StoredActivityItem {
                    activity_type: "transfer_self".into(),
                    direction: "info".into(),
                    amount: Some(net.to_string()),
                    asset_id: tx.outputs.first().and_then(|o| o.asset_id.clone()),
                    counterparty: None,
                    payload: payload_val,
                });
            } else if is_sender {
                // Transfer out (with or without change back to sender)
                let recipient = tx
                    .outputs
                    .iter()
                    .find(|o| o.address != addr)
                    .map(|o| o.address.clone());
                let sent: rust_decimal::Decimal = tx
                    .outputs
                    .iter()
                    .filter(|o| o.address != addr)
                    .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
                    .sum();
                items.push(StoredActivityItem {
                    activity_type: "transfer_out".into(),
                    direction: "out".into(),
                    amount: Some(sent.to_string()),
                    asset_id: tx.outputs.first().and_then(|o| o.asset_id.clone()),
                    counterparty: recipient,
                    payload: payload_val,
                });
            } else if is_receiver {
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
                items.push(StoredActivityItem {
                    activity_type: activity_type.into(),
                    direction: "in".into(),
                    amount: Some(received.to_string()),
                    asset_id: tx
                        .outputs
                        .iter()
                        .find(|o| o.address == addr)
                        .and_then(|o| o.asset_id.clone()),
                    counterparty: sender_addr.map(String::from),
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
                items.push(StoredActivityItem {
                    activity_type: "fee_received".into(),
                    direction: "in".into(),
                    amount: Some(o.amount.clone()),
                    asset_id: None,
                    counterparty: None,
                    payload: payload_val.clone(),
                });
            }
            for o in reward_outputs.iter().filter(|o| o.address == addr) {
                items.push(StoredActivityItem {
                    activity_type: "reward".into(),
                    direction: "in".into(),
                    amount: Some(o.amount.clone()),
                    asset_id: None,
                    counterparty: None,
                    payload: payload_val.clone(),
                });
            }
            items
        }

        PlainPayload::Nft(action) => {
            let payload_val = serde_json::to_value(action).unwrap_or_default();
            match action {
                pms_types_nft::NftAction::Mint { creator, .. } if creator == addr => {
                    vec![StoredActivityItem {
                        activity_type: "nft_mint".into(),
                        direction: "in".into(),
                        amount: None,
                        asset_id: None,
                        counterparty: None,
                        payload: payload_val,
                    }]
                }
                pms_types_nft::NftAction::Transfer { from, to, .. } => {
                    if to == addr {
                        vec![StoredActivityItem {
                            activity_type: "nft_transfer_in".into(),
                            direction: "in".into(),
                            amount: None,
                            asset_id: None,
                            counterparty: Some(from.clone()),
                            payload: payload_val,
                        }]
                    } else if from == addr {
                        vec![StoredActivityItem {
                            activity_type: "nft_transfer_out".into(),
                            direction: "out".into(),
                            amount: None,
                            asset_id: None,
                            counterparty: Some(to.clone()),
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
                    vec![StoredActivityItem {
                        activity_type: "nft_burn".into(),
                        direction: "out".into(),
                        amount: None,
                        asset_id: None,
                        counterparty: None,
                        payload: payload_val,
                    }]
                }
                pms_types_nft::NftAction::Use { user, .. } if user == addr => {
                    vec![StoredActivityItem {
                        activity_type: "nft_use".into(),
                        direction: "info".into(),
                        amount: None,
                        asset_id: None,
                        counterparty: None,
                        payload: payload_val,
                    }]
                }
                _ => vec![],
            }
        }

        PlainPayload::TokenCreate(meta) if meta.creator == addr => {
            vec![StoredActivityItem {
                activity_type: "token_create".into(),
                direction: "info".into(),
                amount: None,
                asset_id: Some(meta.asset_id.clone()),
                counterparty: None,
                payload: serde_json::to_value(meta).unwrap_or_default(),
            }]
        }

        // RoyaltyUpdate (protocole 2.7): the new beneficiary sees "royalty_updated".
        PlainPayload::RoyaltyUpdate {
            asset_id,
            royalty_beneficiary,
            ..
        } if royalty_beneficiary.as_deref() == Some(addr) => {
            vec![StoredActivityItem {
                activity_type: "royalty_updated".into(),
                direction: "info".into(),
                amount: None,
                asset_id: Some(asset_id.clone()),
                counterparty: None,
                payload: serde_json::to_value(plain).unwrap_or_default(),
            }]
        }

        // MarketSettle (protocole 2.7): render the sale from `addr`'s perspective.
        // Buyer → "market_buy" (paid `price`); seller → "market_sell" (received
        // net); royalty beneficiary → "royalty_received". Amounts read from the
        // wrapped tx so change/gas are excluded.
        PlainPayload::MarketSettle {
            tx,
            price_asset,
            price,
            seller,
            buyer,
            ..
        } => {
            let payload_val = serde_json::to_value(plain).unwrap_or_default();
            // Net received by `addr` in the payment asset (excludes item/change/gas).
            let recv_pay: rust_decimal::Decimal = tx
                .outputs
                .iter()
                .filter(|o| o.address == addr && o.asset_id.as_deref() == price_asset.as_deref())
                .filter_map(|o| o.amount.parse::<rust_decimal::Decimal>().ok())
                .sum();
            if addr == buyer {
                vec![StoredActivityItem {
                    activity_type: "market_buy".into(),
                    direction: "out".into(),
                    amount: Some(price.clone()),
                    asset_id: price_asset.clone(),
                    counterparty: Some(seller.clone()),
                    payload: payload_val,
                }]
            } else if addr == seller {
                vec![StoredActivityItem {
                    activity_type: "market_sell".into(),
                    direction: "in".into(),
                    amount: Some(recv_pay.to_string()),
                    asset_id: price_asset.clone(),
                    counterparty: Some(buyer.clone()),
                    payload: payload_val,
                }]
            } else if recv_pay > rust_decimal::Decimal::ZERO {
                vec![StoredActivityItem {
                    activity_type: "royalty_received".into(),
                    direction: "in".into(),
                    amount: Some(recv_pay.to_string()),
                    asset_id: price_asset.clone(),
                    counterparty: Some(seller.clone()),
                    payload: payload_val,
                }]
            } else {
                vec![]
            }
        }

        PlainPayload::TokenBurn {
            owner,
            amount,
            asset_id,
            ..
        } if owner == addr => {
            vec![StoredActivityItem {
                activity_type: "token_burn".into(),
                direction: "out".into(),
                amount: Some(amount.clone()),
                asset_id: asset_id.clone(),
                counterparty: None,
                payload: serde_json::to_value(plain).unwrap_or_default(),
            }]
        }

        PlainPayload::BridgeLock {
            dest_address,
            amount,
            asset_id,
            ..
        } if dest_address == addr => {
            vec![StoredActivityItem {
                activity_type: "bridge_lock_in".into(),
                direction: "in".into(),
                amount: Some(amount.clone()),
                asset_id: asset_id.clone(),
                counterparty: None,
                payload: serde_json::to_value(plain).unwrap_or_default(),
            }]
        }

        PlainPayload::BridgeMint { outputs, .. } => outputs
            .iter()
            .filter(|o| o.address == addr)
            .map(|o| StoredActivityItem {
                activity_type: "bridge_mint".into(),
                direction: "in".into(),
                amount: Some(o.amount.clone()),
                asset_id: o.asset_id.clone(),
                counterparty: None,
                payload: serde_json::to_value(plain).unwrap_or_default(),
            })
            .collect(),

        PlainPayload::Freeze {
            address, reason, ..
        } if address == addr => {
            vec![StoredActivityItem {
                activity_type: "freeze".into(),
                direction: "info".into(),
                amount: None,
                asset_id: None,
                counterparty: None,
                payload: serde_json::json!({ "reason": reason }),
            }]
        }

        PlainPayload::Unfreeze {
            address, reason, ..
        } if address == addr => {
            vec![StoredActivityItem {
                activity_type: "unfreeze".into(),
                direction: "info".into(),
                amount: None,
                asset_id: None,
                counterparty: None,
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
                items.push(StoredActivityItem {
                    activity_type: "seized".into(),
                    direction: "out".into(),
                    amount: Some(total.to_string()),
                    asset_id: None,
                    counterparty: None,
                    payload: payload_val.clone(),
                });
            }
            for o in outputs.iter().filter(|o| o.address == addr) {
                items.push(StoredActivityItem {
                    activity_type: "seize_received".into(),
                    direction: "in".into(),
                    amount: Some(o.amount.clone()),
                    asset_id: None,
                    counterparty: Some(from_address.clone()),
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
                .map(|o| StoredActivityItem {
                    activity_type: "reverse_received".into(),
                    direction: "in".into(),
                    amount: Some(o.amount.clone()),
                    asset_id: o.asset_id.clone(),
                    counterparty: None,
                    payload: payload_val.clone(),
                })
                .collect()
        }

        _ => vec![],
    }
}

/// Pre-compute activity items for ALL involved addresses in one pass.
///
/// Returns a map: `address -> Vec<StoredActivityItem>`.
/// The caller must pre-resolve the TxUtxo sender address and pass it in.
pub fn precompute_all_items(
    plain: &PlainPayload,
    involved_addrs: &[String],
    sender_addr: Option<&str>,
) -> HashMap<String, Vec<StoredActivityItem>> {
    let mut map = HashMap::new();
    for addr in involved_addrs {
        let items = classify_for_storage(plain, addr, sender_addr);
        if !items.is_empty() {
            map.insert(addr.clone(), items);
        }
    }
    map
}
