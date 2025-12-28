use anyhow::{Result, anyhow};
use pms_storage::DagStorage;
use pms_storage::rocks_store::store::RocksStore;
use pms_types_payload::{EncryptedPayload, PayloadEnvelope, PlainPayload};
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

pub fn involves_address(plain: &PlainPayload, addr: &str) -> bool {
    match plain {
        PlainPayload::Mint { outputs } => outputs.iter().any(|o| o.address == addr),
        PlainPayload::TxUtxo(tx) => {
            // MVP: filtre par outputs uniquement (les inputs nécessitent un index UTXO)
            tx.outputs.iter().any(|o| o.address == addr)
        }
        _ => false,
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
        if let Some(PayloadEnvelope::Encrypted(enc)) = b
            .payload_json
            .as_ref()
            .and_then(|s| serde_json::from_str::<PayloadEnvelope>(s).ok())
        {
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
    }

    // trie + limite
    out.sort_by(|a, b| b.ts_ms.cmp(&a.ts_ms).then_with(|| b.id.cmp(&a.id)));
    if out.len() > limit {
        out.truncate(limit);
    }
    Ok(out)
}
