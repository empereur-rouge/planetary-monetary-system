// tests/history_core.rs
use pms_server::api_fn::history::PageResp;
use pms_types_payload::{AAD, EncryptedPayload, PayloadEnvelope};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
// Adaptez ces use selon vos crates
use pms_wire::WireBlock;

// --------- Petit trait de store et impl in-memory ---------
trait Store {
    fn recent_ids_by_time(
        &self,
        after_ts: Option<i64>,
        after_id: Option<String>,
        limit: usize,
    ) -> (Vec<String>, Option<(i64, String, bool)>);

    fn get_blocks_by_ids(&self, ids: &[String]) -> Vec<WireBlock>;
}

#[derive(Clone)]
struct FakeStore {
    // index temporel (du +récent au +ancien)
    // et accès par id -> WireBlock
    idx: Vec<(String, i64)>,
    by_id: HashMap<String, WireBlock>,
}

impl FakeStore {
    fn new() -> Self {
        Self {
            idx: vec![],
            by_id: HashMap::new(),
        }
    }

    fn push(&mut self, id: &str, ts: i64, wb: WireBlock) {
        self.idx.push((id.to_string(), ts));
        self.by_id.insert(id.to_string(), wb);
        // on garde l'ordre ts desc puis id desc pour imiter le vrai
        self.idx
            .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.cmp(&a.0)));
    }
}

impl Store for FakeStore {
    fn recent_ids_by_time(
        &self,
        after_ts: Option<i64>,
        after_id: Option<String>,
        limit: usize,
    ) -> (Vec<String>, Option<(i64, String, bool)>) {
        let mut start = 0usize;
        if let (Some(ts), aid) = (after_ts, after_id.clone()) {
            // positionner “strictement après” (ts desc / id desc)
            for (i, (id, its)) in self.idx.iter().enumerate() {
                if *its < ts {
                    break;
                }
                if *its > ts {
                    start = i + 1;
                    continue;
                }
                // égalité ts : on coupe après l'id passé
                if let Some(ref a) = aid {
                    if id == a {
                        start = i + 1;
                        break;
                    }
                }
                start = i + 1;
            }
        } else if let Some(aid) = after_id {
            if let Some(pos) = self.idx.iter().position(|(id, _)| *id == aid) {
                start = pos + 1;
            }
        }

        let end = (start + limit).min(self.idx.len());
        let slice = &self.idx[start..end];
        let ids = slice.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();

        let next = slice.last().map(|(last_id, last_ts)| {
            let has_more = end < self.idx.len();
            (*last_ts, last_id.clone(), has_more)
        });

        (ids, next)
    }

    fn get_blocks_by_ids(&self, ids: &[String]) -> Vec<WireBlock> {
        ids.iter()
            .filter_map(|id| self.by_id.get(id).cloned())
            .collect()
    }
}

// --------- Fonction cœur (pure) à tester ---------
fn get_encrypted_history_core<S: Store>(
    store: &S,
    after_ts: Option<i64>,
    after_id: Option<String>,
    limit: usize,
) -> PageResp<WireBlock> {
    let (ids, next_cursor) = store.recent_ids_by_time(after_ts, after_id, limit);
    let mut blocks = store.get_blocks_by_ids(&ids);

    // ne garder que les Encrypted
    blocks.retain(|wb| {
        wb.payload_json
            .as_ref()
            .and_then(|s| serde_json::from_str::<PayloadEnvelope>(s).ok())
            .map(|env| matches!(env, PayloadEnvelope::Encrypted(_)))
            .unwrap_or(false)
    });

    let (next_after_ts, next_after_id, has_more) = next_cursor
        .map(|(ts, id, more)| (Some(ts), Some(id), more))
        .unwrap_or((None, None, false));

    PageResp {
        items: blocks,
        next_after_ts,
        next_after_id,
        has_more,
    }
}

// --------- Un test ultra simple ---------
#[test]
fn history_core_filters_and_paginates() {
    // 1) Prépare un store mémoire avec 3 blocs
    let mut st = FakeStore::new();

    // Pour remplir les champs obligatoires
    fn dummy_block(id: &str, payload_json: Option<String>, nonce: u64) -> WireBlock {
        WireBlock {
            id: id.into(),
            parents: vec![],
            payload_json,
            nonce,

            // Champs obligatoires introduits récemment
            network_id: "testnet".into(),
            protocol_version: 1,
            signer_pk_hex: String::new(),
            signature_hex: String::new(),
            metadata: None,
        }
    }

    // a) Encrypted minimal (pas besoin de crypto réelle)
    let enc1 = EncryptedPayload {
        scheme: "x25519+aes256gcm".into(),
        key_version: 1,
        aad: AAD {
            len_hint: 0,
            binding: None,
        },
        commitment: "c".into(),
        ciphertext_b64: "AA==".into(),
        recipients: vec![],
        nonce_b64: "AA==".into(),
    };
    let wb_enc1 = dummy_block(
        "enc-1",
        Some(serde_json::to_string(&PayloadEnvelope::Encrypted(enc1)).unwrap()),
        1,
    );
    st.push("enc-1", 2000, wb_enc1);

    // b) Non-encrypté => doit être filtré
    let wb_clear = dummy_block(
        "clear-1",
        Some("\"Genesis\"".to_string()), // JSON non-encrypted
        2,
    );
    st.push("clear-1", 1500, wb_clear);

    // c) Encrypted 2 (plus ancien)
    let enc2 = EncryptedPayload {
        scheme: "x25519+aes256gcm".into(),
        key_version: 1,
        aad: AAD {
            len_hint: 0,
            binding: None,
        },
        commitment: "d".into(),
        ciphertext_b64: "AA==".into(),
        recipients: vec![],
        nonce_b64: "AA==".into(),
    };
    let wb_enc2 = dummy_block(
        "enc-2",
        Some(serde_json::to_string(&PayloadEnvelope::Encrypted(enc2)).unwrap()),
        3,
    );
    st.push("enc-2", 1000, wb_enc2);

    // 2) Appel de la fonction cœur (limit = 10)
    let page = get_encrypted_history_core(&st, None, None, 10);

    // 3) Assert : seuls les Encrypted reviennent, pas le bloc Genesis
    let ids: Vec<_> = page.items.iter().map(|w| w.id.as_str()).collect();
    assert_eq!(ids, vec!["enc-1", "enc-2"]);

    // 4) Test pagination : after = (2000, "enc-1") => on doit voir seulement enc-2
    let page2 = get_encrypted_history_core(&st, Some(2000), Some("enc-1".into()), 10);
    let ids2: Vec<_> = page2.items.iter().map(|w| w.id.as_str()).collect();
    assert_eq!(ids2, vec!["enc-2"]);
}
