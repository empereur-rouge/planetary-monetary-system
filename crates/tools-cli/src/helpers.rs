use std::io;
use std::io::Read;
use std::path::Path;
use std::sync::{Arc};
use owo_colors::OwoColorize;
use tokio::sync::Mutex;
use anyhow::Result;
use pms_config::Settings;
use pms_wallet::{decode_address, SignerBackend, Wallet};
use crate::repl::CliState;

pub async fn current_wallet_or_err(state: &Arc<Mutex<CliState>>) -> anyhow::Result<Wallet> {
    let st = state.lock().await;
    st.current_wallet()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Aucun wallet sélectionné"))
}

pub fn in_docker() -> bool { Path::new("/.dockerenv").exists() }
pub fn default_export_path(filename: &str) -> String {
    if in_docker() && Path::new("/exports").exists() {
        format!("/exports/{filename}")
    } else {
        filename.to_string()
    }
}
pub fn read_stdin_all() -> Option<String> {
    if atty::isnt(atty::Stream::Stdin) {
        let mut s = String::new();
        io::stdin().read_to_string(&mut s).ok()?;
        Some(s)
    } else { None }
}

pub fn wait_enter() {
    println!("{}", "↩︎ Appuie sur Entrée pour revenir au menu".bright_black());
    let _ = io::stdin().read(&mut [0u8]).ok(); // lit 1 byte (Entrée)
}

/// Retourne la pub X25519 hex à partir d’une adresse Bech32m.
pub fn x25519_from_addr(addr: &str) -> Result<String> {
    let (_, xpk_hex) = decode_address(addr)
        .map_err(|e| anyhow::anyhow!("adresse invalide: {e}"))?;
    Ok(xpk_hex)
}

pub fn make_http_client(s: &Settings) -> anyhow::Result<reqwest::Client> {
    let mut b = reqwest::Client::builder();

    // Si dev/testnet -> on accepte les certs invalides (self-signed)
    if s.network.mode.is_non_prod() {
        b = b.danger_accept_invalid_certs(true);
    } else {
        b = b.danger_accept_invalid_certs(false);
    }

    Ok(b.build()?)
}