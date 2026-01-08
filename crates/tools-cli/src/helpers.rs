use owo_colors::OwoColorize;
use pms_config::Settings;
use std::io;
use std::io::Read;
use std::path::Path;

pub fn in_docker() -> bool {
    Path::new("/.dockerenv").exists()
}
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
    } else {
        None
    }
}

pub fn wait_enter() {
    println!(
        "{}",
        "↩︎ Appuie sur Entrée pour revenir au menu".bright_black()
    );
    let _ = io::stdin().read(&mut [0u8]).ok(); // lit 1 byte (Entrée)
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
