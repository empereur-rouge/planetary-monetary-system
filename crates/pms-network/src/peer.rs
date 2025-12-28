//! Gestion d’une connexion TCP sortante (client).
//! Protocole : chaque message = une ligne JSON.

use tokio::{io::{AsyncBufReadExt, AsyncWriteExt, BufReader}, net::TcpStream};
use crate::messages::NetMsg;

pub struct Peer {
    write: tokio::sync::Mutex<tokio::io::WriteHalf<TcpStream>>,
}

impl Peer {
    /// Ouvre la connexion, démarre une tâche de lecture, retourne :
    /// - handle d’écriture (Peer)
    /// - canal des messages *reçus*
    pub async fn _connect(addr: &str) -> anyhow::Result<(Self, tokio::sync::mpsc::Receiver<NetMsg>)> {
        let stream = TcpStream::connect(addr).await?;
        let (r, w) = tokio::io::split(stream);
        let mut reader = BufReader::new(r);

        let (tx_in, rx_in) = tokio::sync::mpsc::channel(1024);
        // Tâche lecture en fond
        tokio::spawn(async move {
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break, // fermé
                    Ok(_) => {
                        if let Ok(msg) = serde_json::from_str::<NetMsg>(&line) {
                            let _ = tx_in.send(msg).await;
                        }
                    }
                }
            }
        });

        Ok((Self { write: tokio::sync::Mutex::new(w) }, rx_in))
    }

    /// Envoie un message (écrit une ligne JSON)
    pub async fn _send(&self, msg: &NetMsg) -> anyhow::Result<()> {
        let s = serde_json::to_string(msg)?;
        let mut w = self.write.lock().await;
        w.write_all(s.as_bytes()).await?;
        w.write_all(b"\n").await?;
        Ok(())
    }
}
