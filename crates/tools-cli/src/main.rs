extern crate core;

use anyhow::Result;

mod repl;
mod block_submission;
mod helpers;
mod wallet_mnemonic;
mod wallet_actions;
mod history_actions;
mod utils;

#[tokio::main]
async fn main() -> Result<()> {
    repl::run().await
}