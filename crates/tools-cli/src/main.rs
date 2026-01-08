// NOTE: Ces allow sont temporaires pour le pré-déploiement.
// Les warnings peuvent être refactorisés après le déploiement.
#![allow(clippy::collapsible_if)]
#![allow(clippy::needless_borrows_for_generic_args)]
#![allow(clippy::needless_borrow)]
#![allow(clippy::borrow_deref_ref)]
#![allow(clippy::unnecessary_to_owned)]
#![allow(unused_imports)]
#![allow(unused_variables)]
extern crate core;

use anyhow::Result;

mod block_submission;
mod helpers;
mod history_actions;
mod keygen;
mod repl;
mod utils;
mod wallet_actions;
mod wallet_mnemonic;

#[tokio::main]
async fn main() -> Result<()> {
    // Simple argument parsing to support scripting
    let args: Vec<String> = std::env::args().collect();

    if args.len() > 1 && args[1] == "gen-coordinator" {
        if args.len() != 4 {
            eprintln!("Usage: tools-cli gen-coordinator <key_file> <json_file>");
            std::process::exit(1);
        }
        let key_path = &args[2];
        let json_path = &args[3];
        keygen::generate_and_save(key_path, json_path)?;
        return Ok(());
    }

    repl::run().await
}
