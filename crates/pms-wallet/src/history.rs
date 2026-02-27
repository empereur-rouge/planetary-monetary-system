use anyhow::{Result, anyhow};
use pms_storage::DagStorage;
use pms_storage::rocks_store::store::RocksStore;
use pms_types_payload::{EncryptedPayload, EncryptedRewardOutput, PayloadEnvelope, PlainPayload};
use pms_types_transaction::TxOutput;
use pms_wire::WireBlock;

#[derive(Debug, Clone)]
pub struct Decrypted {
    pub block_id: String,
    pub plain: PlainPayload,
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub id: String,
    pub ts_ms: i64,
    pub plain: PlainPayload,
}

// Helper pour tenter de déchiffrer un EncryptedReward
// Si on trouve des outputs pour nous, on reconstruit un PlainPayload::Reward
pub fn try_decrypt_encrypted_reward(
    encrypted_outputs: &[EncryptedRewardOutput],
    burned: &str,
    tx_block_id: &str,
    x25519_sk_hex: &str,
    addr: &str,
) -> Option<PlainPayload> {
    let mut my_outputs = Vec::new();

    for e_out in encrypted_outputs {
        if let Ok(pt) = e_out.encrypted.decrypt_with(x25519_sk_hex) {
            // On suppose que c'est un TxOutput sérialisé
            if let Ok(out) = serde_json::from_slice::<TxOutput>(&pt) {
                if out.address == addr {
                    my_outputs.push(out);
                }
            }
        }
    }

    if my_outputs.is_empty() {
        None
    } else {
        // On présente ça comme un Reward "clair" pour l'affichage
        Some(PlainPayload::Reward {
            fee_outputs: vec![],
            reward_outputs: my_outputs,
            burned: burned.to_string(),
            tx_block_id: tx_block_id.to_string(),
        })
    }
}

/// Récupère les `limit` derniers blocs depuis le store, et tente de les
/// déchiffrer avec la clé privée X25519 (hex) du wallet.
/// Ne retourne que ceux qui se déchiffrent avec succès.
pub async fn scan_decrypt_recent(
    store: &RocksStore,
    x25519_sk_hex: &str,
    limit: usize,
) -> Result<Vec<Decrypted>> {
    let ids = store.recent_ids(limit).await?;
    let blocks: Vec<WireBlock> = store.get_blocks_by_ids(&ids).await?; // <- direct

    let mut out = Vec::new();
    for wb in blocks {
        let Some(pjson) = wb.payload_json.as_ref() else {
            continue;
        };
        let Ok(env) = serde_json::from_str::<PayloadEnvelope>(pjson) else {
            continue;
        };
        if let PayloadEnvelope::Encrypted(enc) = env {
            if let Ok(plain) = try_decrypt_plain(&enc, x25519_sk_hex) {
                out.push(Decrypted {
                    block_id: wb.id,
                    plain,
                });
            }
        }
    }
    Ok(out)
}

/// Helper: déchiffre un EncryptedPayload → PlainPayload avec la SK X25519 (hex).
fn try_decrypt_plain(enc: &EncryptedPayload, sk_hex: &str) -> Result<PlainPayload> {
    enc.decrypt_as_payload(sk_hex).map_err(|e| anyhow!(e))
}

/// Collecte toutes les adresses impliquées dans un PlainPayload.
/// Utilisé par l'EventBus pour pré-calculer les adresses sans DB lookup côté SSE.
pub fn collect_involved_addresses(plain: &PlainPayload) -> Vec<String> {
    let mut addrs = Vec::new();
    match plain {
        PlainPayload::Mint { outputs } => {
            addrs.extend(outputs.iter().map(|o| o.address.clone()));
        }
        PlainPayload::TxUtxo(tx) => {
            addrs.extend(tx.outputs.iter().map(|o| o.address.clone()));
        }
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
        PlainPayload::EncryptedReward { .. } => {} // chiffré, pas d'adresses extractibles
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
        _ => {} // Genesis, Milestone, ConfigUpdate
    }
    addrs.dedup();
    addrs
}

pub fn involves_address(plain: &PlainPayload, addr: &str) -> bool {
    match plain {
        PlainPayload::Mint { outputs } => outputs.iter().any(|o| o.address == addr),
        PlainPayload::TxUtxo(tx) => tx.outputs.iter().any(|o| o.address == addr),
        PlainPayload::Reward {
            fee_outputs,
            reward_outputs,
            ..
        } => {
            fee_outputs.iter().any(|o| o.address == addr)
                || reward_outputs.iter().any(|o| o.address == addr)
        }
        PlainPayload::Nft(action) => nft_involves_address(action, addr),
        PlainPayload::TokenCreate(meta) => meta.creator == addr,
        // EncryptedReward: outputs chiffrés, impossible de checker sans clé.
        // Géré séparément dans history_page_for_address / scan_decrypt_recent_for_address.
        PlainPayload::EncryptedReward { .. } => false,
        PlainPayload::BridgeLock { dest_address, .. } => dest_address == addr,
        PlainPayload::BridgeMint { outputs, .. } => outputs.iter().any(|o| o.address == addr),
        PlainPayload::Freeze { address, .. } | PlainPayload::Unfreeze { address, .. } => {
            address == addr
        }
        PlainPayload::Seize {
            from_address,
            outputs,
            ..
        } => from_address == addr || outputs.iter().any(|o| o.address == addr),
        PlainPayload::Reverse { outputs, .. } => outputs.iter().any(|o| o.address == addr),
        // Genesis, Milestone, ConfigUpdate: pas liés à une adresse wallet
        _ => false,
    }
}

fn nft_involves_address(action: &pms_types_nft::NftAction, addr: &str) -> bool {
    match action {
        pms_types_nft::NftAction::Mint { creator, .. } => creator == addr,
        pms_types_nft::NftAction::Transfer { from, to, .. } => from == addr || to == addr,
        pms_types_nft::NftAction::Use { user, .. } => user == addr,
        pms_types_nft::NftAction::Burn { burner, .. } => burner == addr,
        pms_types_nft::NftAction::BatchBurn { burner, .. } => burner == addr,
    }
}

fn nft_involves_any(action: &pms_types_nft::NftAction, candidates: &[String]) -> bool {
    let has = |a: &str| candidates.iter().any(|c| a.eq_ignore_ascii_case(c));
    match action {
        pms_types_nft::NftAction::Mint { creator, .. } => has(creator),
        pms_types_nft::NftAction::Transfer { from, to, .. } => has(from) || has(to),
        pms_types_nft::NftAction::Use { user, .. } => has(user),
        pms_types_nft::NftAction::Burn { burner, .. } => has(burner),
        pms_types_nft::NftAction::BatchBurn { burner, .. } => has(burner),
    }
}

pub fn involves_any_address(plain: &PlainPayload, candidates: &[String]) -> bool {
    match plain {
        PlainPayload::Mint { outputs } => outputs
            .iter()
            .any(|o| candidates.iter().any(|c| o.address.eq_ignore_ascii_case(c))),
        PlainPayload::TxUtxo(tx) => tx
            .outputs
            .iter()
            .any(|o| candidates.iter().any(|c| o.address.eq_ignore_ascii_case(c))),
        PlainPayload::Reward {
            fee_outputs,
            reward_outputs,
            ..
        } => {
            fee_outputs
                .iter()
                .any(|o| candidates.iter().any(|c| o.address.eq_ignore_ascii_case(c)))
                || reward_outputs
                    .iter()
                    .any(|o| candidates.iter().any(|c| o.address.eq_ignore_ascii_case(c)))
        }
        PlainPayload::Nft(action) => nft_involves_any(action, candidates),
        PlainPayload::TokenCreate(meta) => {
            candidates
                .iter()
                .any(|c| meta.creator.eq_ignore_ascii_case(c))
        }
        PlainPayload::EncryptedReward { .. } => false,
        PlainPayload::BridgeLock { dest_address, .. } => {
            candidates.iter().any(|c| dest_address.eq_ignore_ascii_case(c))
        }
        PlainPayload::BridgeMint { outputs, .. } => outputs
            .iter()
            .any(|o| candidates.iter().any(|c| o.address.eq_ignore_ascii_case(c))),
        PlainPayload::Freeze { address, .. } | PlainPayload::Unfreeze { address, .. } => {
            candidates
                .iter()
                .any(|c| address.eq_ignore_ascii_case(c))
        }
        PlainPayload::Seize {
            from_address,
            outputs,
            ..
        } => {
            candidates
                .iter()
                .any(|c| from_address.eq_ignore_ascii_case(c))
                || outputs
                    .iter()
                    .any(|o| candidates.iter().any(|c| o.address.eq_ignore_ascii_case(c)))
        }
        PlainPayload::Reverse { outputs, .. } => outputs
            .iter()
            .any(|o| candidates.iter().any(|c| o.address.eq_ignore_ascii_case(c))),
        _ => false,
    }
}

/// Scanne les derniers blocs depuis Redis, tente de déchiffrer avec la sk X25519,
/// et garde ceux qui impliquent `addr`.
pub async fn scan_decrypt_recent_for_address(
    store: &RocksStore,
    x25519_sk_hex: &str,
    addr: &str,
    limit: usize,
) -> Result<Vec<Decrypted>> {
    let ids = store.recent_ids(limit).await?;
    let blocks = store.get_blocks_by_ids(&ids).await?;

    let mut out = Vec::new();
    for wb in blocks {
        let Some(env) = wb
            .payload_json
            .as_ref()
            .and_then(|s| serde_json::from_str::<PayloadEnvelope>(s).ok())
        else {
            continue;
        };

        if let PayloadEnvelope::Encrypted(enc) = env {
            // déchiffre → PlainPayload
            if let Ok(plain) = enc.decrypt_as_payload(x25519_sk_hex) {
                if involves_address(&plain, addr) {
                    out.push(Decrypted {
                        block_id: wb.id,
                        plain,
                    });
                }
            }
        } else if let PayloadEnvelope::Plain(plain) = env {
            match plain {
                PlainPayload::EncryptedReward {
                    encrypted_outputs,
                    burned,
                    tx_block_id,
                } => {
                    if let Some(decrypted_reward) = try_decrypt_encrypted_reward(
                        &encrypted_outputs,
                        &burned,
                        &tx_block_id,
                        x25519_sk_hex,
                        addr,
                    ) {
                        out.push(Decrypted {
                            block_id: wb.id,
                            plain: decrypted_reward,
                        });
                    }
                }
                _ => {}
            }
        }
    }
    Ok(out)
}

/// Liste paginée (plus récents -> plus anciens) en filtrant par adresse.
pub async fn history_page_for_address(
    store: &RocksStore,
    recipient_sk_hex: &str,
    addr: &str,
    after: Option<(i64, String)>, // (ts_ms, id)
    limit: usize,
) -> Result<Vec<HistoryEntry>> {
    if limit == 0 {
        return Ok(vec![]);
    }
    // Récupère les ids avec pagination
    let (ids, _cursor) = store
        .recent_ids_by_time(
            after.clone().map(|(ts, _)| ts),
            after.map(|(_, id)| id),
            limit * 3,
        )
        .await?;

    if ids.is_empty() {
        return Ok(vec![]);
    }

    // récupère les blocs
    let blocks = store.get_blocks_by_ids(&ids).await?;

    // récupère les timestamps
    let id_ts = store.ts_for_ids(&ids).await?;

    // déchiffre + filtre
    let mut out = Vec::new();
    for b in blocks {
        let ts = *id_ts.get(&b.id).unwrap_or(&0);
        let Some(env) = b
            .payload_json
            .as_ref()
            .and_then(|s| serde_json::from_str::<PayloadEnvelope>(s).ok())
        else {
            continue;
        };

        match env {
            PayloadEnvelope::Encrypted(enc) => {
                if let Ok(plain) = enc.decrypt_as_payload(recipient_sk_hex) {
                    if involves_address(&plain, addr) {
                        out.push(HistoryEntry {
                            id: b.id.clone(),
                            ts_ms: ts,
                            plain,
                        });
                    }
                }
            }
            PayloadEnvelope::Plain(PlainPayload::EncryptedReward {
                encrypted_outputs,
                burned,
                tx_block_id,
            }) => {
                if let Some(decrypted_reward) = try_decrypt_encrypted_reward(
                    &encrypted_outputs,
                    &burned,
                    &tx_block_id,
                    recipient_sk_hex,
                    addr,
                ) {
                    out.push(HistoryEntry {
                        id: b.id.clone(),
                        ts_ms: ts,
                        plain: decrypted_reward,
                    });
                }
            }
            _ => {}
        }
    }

    // trie + limite
    out.sort_by(|a, b| b.ts_ms.cmp(&a.ts_ms).then_with(|| b.id.cmp(&a.id)));
    if out.len() > limit {
        out.truncate(limit);
    }
    Ok(out)
}

/// Scans recent blocks for **Plain** payloads (Mint, TxUtxo) involving an address.
/// This does NOT require decryption - it's for transparent/public transactions.
pub async fn history_plain_for_address(
    store: &RocksStore,
    addr: &str,
    limit: usize,
) -> Result<Vec<HistoryEntry>> {
    if limit == 0 {
        return Ok(vec![]);
    }

    // Get recent block IDs
    let (ids, _cursor) = store.recent_ids_by_time(None, None, limit * 3).await?;

    if ids.is_empty() {
        return Ok(vec![]);
    }

    // Retrieve blocks
    let blocks = store.get_blocks_by_ids(&ids).await?;

    // Get timestamps
    let id_ts = store.ts_for_ids(&ids).await?;

    // Filter blocks with Plain payloads involving the address
    let mut out = Vec::new();
    for b in blocks {
        let ts = *id_ts.get(&b.id).unwrap_or(&0);

        if let Some(env) = b
            .payload_json
            .as_ref()
            .and_then(|s| serde_json::from_str::<PayloadEnvelope>(s).ok())
        {
            match env {
                PayloadEnvelope::Plain(plain) => {
                    if involves_address(&plain, addr) {
                        out.push(HistoryEntry {
                            id: b.id.clone(),
                            ts_ms: ts,
                            plain,
                        });
                    }
                }
                _ => {} // Skip Encrypted payloads
            }
        }
    }

    // Sort by timestamp (newest first) and limit
    out.sort_by(|a, b| b.ts_ms.cmp(&a.ts_ms).then_with(|| b.id.cmp(&a.id)));
    if out.len() > limit {
        out.truncate(limit);
    }
    Ok(out)
}
