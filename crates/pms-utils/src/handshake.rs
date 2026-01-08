use pms_network::messages::NetMsg;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp;

// Handshake client simple:
// 1) lit Hello du serveur
// 2) envoie Hello
// 3) lit HelloAck(ok=true)
pub async fn do_handshake(
    r: &mut BufReader<tcp::OwnedReadHalf>,
    w: &mut tcp::OwnedWriteHalf,
) -> anyhow::Result<()> {
    let mut line = String::new();

    // 1) read server Hello
    line.clear();
    let n = r.read_line(&mut line).await?;
    anyhow::ensure!(n > 0, "eof before hello");
    let msg: NetMsg = serde_json::from_str(&line)?;
    match msg {
        NetMsg::Hello { .. } => {}
        other => anyhow::bail!("expected Hello, got {:?}", other),
    }

    // 2) send our Hello
    let hello = NetMsg::Hello {
        proto: 1,
        node_id: "test-client".into(),
        nonce: 0,
        ping_ms: 1000,
    };
    let s = serde_json::to_string(&hello)? + "\n";
    w.write_all(s.as_bytes()).await?;
    w.flush().await?;

    // 3) read HelloAck
    line.clear();
    let n = r.read_line(&mut line).await?;
    anyhow::ensure!(n > 0, "eof before hello-ack");
    let ack: NetMsg = serde_json::from_str(&line)?;
    match ack {
        NetMsg::HelloAck { ok: true, .. } => Ok(()),
        other => anyhow::bail!("expected HelloAck(ok), got {:?}", other),
    }
}
