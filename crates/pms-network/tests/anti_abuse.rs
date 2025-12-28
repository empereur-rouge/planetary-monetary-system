use std::{time::Duration};
use std::collections::{HashMap, HashSet};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    time::timeout,
};
use std::sync::Arc;
use tokio::sync::Mutex;

use pms_interface::NetDagAdapter;
use pms_network::messages::NetMsg;
use pms_server::limits::{MAX_LINE_BYTES, MAX_PARSE_ERRORS};
use serde_json::json;
use pms_config::load_config;
use pms_server::Server;
use pms_storage::store::PutResult;
use pms_testkit::{ephemeral_addr, test_meta_and_wallet};
use pms_utils::do_handshake;
use pms_wallet::SignerBackend;
use pms_wallet::signing_wire::canonical_wireblock_message;
use pms_wire::{WireBlock, WireMeta};

/// Adapter bidon : rien en persistance, on répond juste aux appels.
#[derive(Default)]
pub struct DummyAdapter {
    mem: Mutex<HashMap<String, WireBlock>>,
}

impl DummyAdapter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { mem: Mutex::new(Default::default()) })
    }
}

#[async_trait::async_trait]
impl NetDagAdapter for DummyAdapter {
    async fn have_block(&self, id: &str) -> bool {
        self.mem.lock().await.contains_key(id)
    }

    async fn persist_block(&self, b: &WireBlock) -> anyhow::Result<PutResult> {
        let mut m = self.mem.lock().await;
        if m.contains_key(&b.id) {
            Ok(PutResult::AlreadyExists)
        } else {
            m.insert(b.id.clone(), b.clone());
            Ok(PutResult::Inserted)
        }
    }

    async fn broadcast_block(&self, _b: &WireBlock) -> anyhow::Result<()> { Ok(()) }

    async fn top_tips(&self, limit: usize) -> anyhow::Result<Vec<String>> {

        let m = self.mem.lock().await;
        let mut has_parent: HashSet<&str> = HashSet::new();
        for wb in m.values() {
            for p in &wb.parents {
                has_parent.insert(p);
            }
        }
        // tips = blocs qui ne sont parents d’aucun autre (grossier, mais suffisant pour le test)
        let mut tips: Vec<String> = m
            .values()
            .filter(|wb| !has_parent.contains(wb.id.as_str()))
            .map(|wb| wb.id.clone())
            .collect();
        if tips.is_empty() {
            // fallback: tout le monde
            tips.extend(m.keys().cloned());
        }
        tips.truncate(limit);
        Ok(tips)
    }

    async fn get_block(&self, id: &str) -> anyhow::Result<Option<WireBlock>> {
        Ok(self.mem.lock().await.get(id).cloned())
    }

    async fn recent_ids(&self, _limit: usize) -> anyhow::Result<Vec<String>> {
        todo!()
    }

    async fn get_blocks_by_ids(&self, _ids: &[String]) -> anyhow::Result<Vec<WireBlock>> {
        todo!()
    }
}


async fn start_server(addr: &str) -> Arc<Server> {
    let (meta, wallet) = test_meta_and_wallet();

    let srv = Server::new(DummyAdapter::new(), meta.network_id, meta.protocol_version, Arc::new(wallet));
    let s2 = srv.clone();
    let addr = addr.to_owned();
    tokio::spawn(async move {
        let _ = s2.listen(&addr).await;
    });
    // petit délai pour bind
    tokio::time::sleep(Duration::from_millis(50)).await;
    srv
}

async fn read_line_with_timeout(reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> Option<String> {
    let mut line = String::new();
    if timeout(Duration::from_millis(500), reader.read_line(&mut line)).await.ok()?.ok()? == 0 {
        return None;
    }
    Some(line)
}

#[tokio::test]
async fn ping_pong_still_works() -> anyhow::Result<()> {
    let addr = ephemeral_addr();
    let _srv = start_server(addr.as_str()).await;

    let stream = TcpStream::connect(addr).await?;
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);

    do_handshake(&mut reader, &mut w).await?;

    // envoie Ping
    let ping = serde_json::to_string(&NetMsg::Ping)? + "\n";
    w.write_all(ping.as_bytes()).await?;

    // attend Pong
    let line = read_line_with_timeout(&mut reader).await.expect("no pong");
    let msg: NetMsg = serde_json::from_str(&line)?;
    match msg {
        NetMsg::Pong => {}
        _ => panic!("expected Pong, got {:?}", msg),
    }
    Ok(())
}

#[tokio::test]
async fn oversize_message_is_dropped_connection() -> anyhow::Result<()> {
    let addr = ephemeral_addr();
    let _srv = start_server(addr.as_str()).await;

    let stream = TcpStream::connect(addr).await?;
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);

    do_handshake(&mut reader, &mut w).await?;

    // construit une ligne > MAX_LINE_BYTES
    let big = "X".repeat(MAX_LINE_BYTES + 16);
    w.write_all(big.as_bytes()).await?;
    w.write_all(b"\n").await?;

    // Expect: serveur coupe -> read retourne None / 0
    let got = read_line_with_timeout(&mut reader).await;
    assert!(got.is_none(), "connection should close on oversize");
    Ok(())
}

#[tokio::test]
async fn too_many_parse_errors_kicks_peer() -> anyhow::Result<()> {
    let addr = ephemeral_addr();
    let _srv = start_server(addr.as_str()).await;

    let stream = TcpStream::connect(addr).await?;
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);

    do_handshake(&mut reader, &mut w).await?;

    // envoie MAX_PARSE_ERRORS + 1 lignes invalides
    for _ in 0..(MAX_PARSE_ERRORS + 1) {
        w.write_all(b"{not-json}\n").await?;
    }

    // Après dépassement, la connexion doit fermer rapidement
    let got = read_line_with_timeout(&mut reader).await;
    assert!(got.is_none(), "connection should close after too many parse errors");
    Ok(())
}

#[tokio::test]
async fn rate_limit_drops_or_closes_under_burst() -> anyhow::Result<()> {
    let addr = ephemeral_addr();
    let _srv = start_server(addr.as_str()).await;

    let stream = TcpStream::connect(addr).await?;
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);

    do_handshake(&mut reader, &mut w).await?;

    // spam de ping (burst) : selon bucket, certains pongs ne reviendront pas
    let n = 200usize;
    for _ in 0..n {
        let s = serde_json::to_string(&NetMsg::Ping)? + "\n";
        let _ = w.write_all(s.as_bytes()).await;
    }

    // lis au plus n réponses pendant 1s
    let mut pongs = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        if tokio::time::Instant::now() >= deadline { break; }
        if let Some(line) = read_line_with_timeout(&mut reader).await {
            if let Ok(NetMsg::Pong) = serde_json::from_str::<NetMsg>(&line) {
                pongs += 1;
            }
        } else {
            break; // connexion peut être fermée par le serveur (trop bavard)
        }
    }

    // On ne s’attend PAS à récupérer 200 pongs (limité par bucket)
    assert!(pongs < n, "rate-limit should cap responses (got {} of {})", pongs, n);
    Ok(())
}

#[tokio::test]
async fn block_is_gossiped_to_other_peers() -> anyhow::Result<()> {
    use pms_config::load_config;
    use pms_wallet::Wallet;

    let addr = ephemeral_addr();
    let srv = start_server(addr.as_str()).await;

    // Peer A
    let a = TcpStream::connect(addr.clone()).await?;
    let (ar, mut aw) = a.into_split();
    let mut ar = BufReader::new(ar);

    do_handshake(&mut ar, &mut aw).await?;

    // Peer B (receveur)
    let b = TcpStream::connect(addr).await?;
    let (br, mut bw) = b.into_split();
    let mut br = BufReader::new(br);

    do_handshake(&mut br, &mut bw).await?;

    // =========================
    // 1) Préparer un bloc signé
    // =========================

    // a) Charger la config réseau pour avoir network_id / protocol_version cohérents
    let settings = load_config()?;
    let meta = WireMeta::from(&settings);

    // b) Wallet de test pour signer
    let wallet = Wallet::from_seed(&[1u8; 32], None)
        .expect("Wallet::from_seed ne doit pas fail en test");

    // c) Construire un WireBlock "unsigned" minimal
    //    NB: l'id "blk1" peut être arbitraire ici, on ne recalcule pas côté serveur.
    let mut wb = WireBlock {
        id: "blk1".to_string(),
        parents: vec!["p1".into(), "p2".into()],
        payload_json: None,
        nonce: 1,
        network_id: meta.network_id.clone(),
        protocol_version: meta.protocol_version as u16,
        signer_pk_hex: wallet.encoded_public_key(),
        signature_hex: String::new(), // on remplit après signature
    };

    // d) Message canonique + signature ECDSA
    let msg = canonical_wireblock_message(&wb);
    wb.signature_hex = wallet
        .sign(&msg)
        .map_err(|e| anyhow::anyhow!("sign error: {e:?}"))?;

    // e) Emballer en NetMsg::Block et sérialiser en JSONL
    let net_block = NetMsg::Block {
        id: wb.id.clone(),
        parents: wb.parents.clone(),
        payload_json: wb.payload_json.clone(),
        nonce: wb.nonce,
        network_id: wb.network_id.clone(),
        protocol_version: wb.protocol_version,
        signer_pk_hex: wb.signer_pk_hex.clone(),
        signature_hex: wb.signature_hex.clone(),
    };

    let line = serde_json::to_string(&net_block)? + "\n";
    aw.write_all(line.as_bytes()).await?;

    // ====================================
    // 2) B doit récupérer ce bloc via gossip
    // ====================================

    let mut got_blk = false;
    let mut line = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);

    loop {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        line.clear();
        if br.read_line(&mut line).await.unwrap_or(0) == 0 {
            break;
        }

        if let Ok(msg) = serde_json::from_str::<NetMsg>(&line) {
            match msg {
                // Ancien chemin: le serveur re-gossip directement un Block complet
                NetMsg::Block { id, .. } if id == "blk1" => {
                    got_blk = true;
                    break;
                }
                // Nouveau chemin: annonce via Inv, puis B demande, puis reçoit Blocks
                NetMsg::Inv { ids } if ids.iter().any(|s| s == "blk1") => {
                    let req = NetMsg::GetBlock { id: "blk1".to_string() };
                    let s = serde_json::to_string(&req)? + "\n";
                    bw.write_all(s.as_bytes()).await?;
                    bw.flush().await?;
                }
                NetMsg::Blocks { blocks } => {
                    if blocks.iter().any(|wb| wb.id == "blk1") {
                        got_blk = true;
                        break;
                    }
                }
                _ => {}
            }
        }
    }

    assert!(
        got_blk,
        "expected to obtain blk1 via Block or Inv->GetBlock->Blocks"
    );

    // silence warnings
    let _ = srv;
    let _ = bw;
    let _ = ar;

    Ok(())
}