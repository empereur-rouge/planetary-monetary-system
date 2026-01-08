use anyhow::Result;
use tempfile::tempdir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    time::{Duration, Instant, sleep},
};

use pms_network::messages::NetMsg;
use pms_server::limits::{MAX_LINE_BYTES, RATE_BURST, RATE_MSGS_PER_SEC};
use pms_testkit::{ephemeral_addr, spawn_node_generic_rocks};
use pms_utils::do_handshake;

async fn wait_for_listen(addr: &str, max_wait_ms: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_millis(max_wait_ms);
    loop {
        match TcpStream::connect(addr).await {
            Ok(_) => return Ok(()),
            Err(_) if Instant::now() < deadline => sleep(Duration::from_millis(20)).await,
            Err(e) => return Err(e.into()),
        }
    }
}

#[tokio::test]
async fn rate_limit_and_size_rocks() -> Result<()> {
    // --- Nœud RocksDB éphémère ---
    let dir = tempdir()?;
    let db_path = dir.path().join("rocks-rls");
    let db_path_str = db_path.to_string_lossy();

    let addr = ephemeral_addr();
    let api_addr = ephemeral_addr();
    let tip_limit = 256usize;

    // Démarre un nœud Rocks (P2P + API sur la même adresse pour le test)
    let (_store, _dag, _adapter, _srv, _jh) = spawn_node_generic_rocks(
        &db_path_str,
        "pms:test:rls",
        addr.as_str(),
        api_addr.as_str(),
        tip_limit,
        None,
    )
    .await?;

    // Attendre que le listener P2P soit prêt
    wait_for_listen(addr.as_str(), 1000).await?;

    // -------- Phase 1 : rate-limit (le serveur peut fermer la connexion) --------
    let s1 = TcpStream::connect(addr.clone()).await?;
    let (r1, mut w1) = s1.into_split();
    let mut br1 = BufReader::new(r1);

    do_handshake(&mut br1, &mut w1).await?;

    // Server sends GetTips after handshake, consume it
    let mut gettips_line = String::new();
    let _ = br1.read_line(&mut gettips_line).await;

    let to_send = (RATE_MSGS_PER_SEC as usize) + (RATE_BURST as usize) + 20;
    for _ in 0..to_send {
        let line = serde_json::to_string(&NetMsg::Ping)? + "\n";
        if let Err(_e) = w1.write_all(line.as_bytes()).await {
            // le serveur a pu couper; on arrête d’écrire
            break;
        }
    }
    let _ = w1.flush().await;

    // Lire quelques réponses sans paniquer si la connexion est déjà fermée
    let mut got_pong = 0usize;
    let mut line = String::new();
    let deadline = Instant::now() + Duration::from_millis(600);
    loop {
        tokio::select! {
            _ = sleep(Duration::from_millis(5)) => {},
            res = br1.read_line(&mut line) => {
                match res {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        if line.contains("\"Pong\"") { got_pong += 1; }
                        line.clear();
                    }
                    Err(_) => break,
                }
            }
            _ = tokio::time::sleep_until(deadline) => break,
        }
    }
    assert!(
        got_pong < to_send,
        "rate-limit inactif: got_pong={got_pong} to_send={to_send}"
    );

    // -------- Phase 2 : oversize (nouvelle connexion) --------
    let s2 = TcpStream::connect(addr).await?;
    let (r2, mut w2) = s2.into_split();
    let mut br2 = BufReader::new(r2);

    do_handshake(&mut br2, &mut w2).await?;

    // Server sends GetTips after handshake, consume it
    let mut gettips_line2 = String::new();
    let _ = br2.read_line(&mut gettips_line2).await;

    // envoie un message volontairement trop gros + invalide
    let big = "X".repeat(MAX_LINE_BYTES + 16);
    let big_line = format!("\"{}\"\n", big);
    let _ = w2.write_all(big_line.as_bytes()).await;
    let _ = w2.flush().await;

    // Give server time to process and close connection
    sleep(Duration::from_millis(200)).await;

    // (A) écriture après oversize — peut échouer si la socket est déjà fermée
    let write_res = w2.write_all(b"{\"Ping\":null}\n").await;

    // (B) tente de lire une ligne (EOF/Err si fermé)
    let mut line2 = String::new();
    let read_res =
        tokio::time::timeout(Duration::from_millis(300), br2.read_line(&mut line2)).await;

    // Succès si : écriture en erreur OU lecture en EOF/erreur OU pas de réponse dans le délai
    let ok = match (write_res, read_res) {
        (Err(_), _) => true,        // fermé à l’écriture
        (_, Ok(Ok(0))) => true,     // EOF
        (_, Ok(Err(_))) => true,    // erreur lecture
        (_, Err(_timeout)) => true, // silencieux (blackhole) → ok pour ce test
        (_, Ok(Ok(_n))) => false,   // a répondu (pas attendu)
    };

    assert!(
        ok,
        "la connexion devrait être fermée ou silencieuse après oversize"
    );

    Ok(())
}
