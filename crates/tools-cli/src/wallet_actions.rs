use crate::helpers::wait_enter;
use crate::repl::CliState;
use anyhow::Result;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Input, Select};
use owo_colors::OwoColorize;
use pms_config::load_config;
use pms_storage::rocks_store::store::RocksStore;
use pms_wallet::{SignerBackend, Wallet};
use qrcode::QrCode;
use qrcode::render::unicode;
use std::sync::Arc;
use tokio::sync::Mutex;

pub fn print_qr(addr: &str) {
    match QrCode::new(addr.as_bytes()) {
        Ok(code) => {
            let img = code.render::<unicode::Dense1x2>().quiet_zone(false).build();
            println!("\n{}", "QR code:".bright_blue().bold());
            println!("{img}");
        }
        Err(e) => eprintln!("{} {e}", "❌ QR code échec:".red().bold()),
    }
}

// 1) Créer un wallet
pub async fn action_create_wallet(state: &Arc<Mutex<CliState>>, hrp: &str) -> Result<()> {
    let name: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Nom (facultatif)")
        .allow_empty(true)
        .interact_text()?;

    let w = Wallet::generate();
    {
        let mut st = state.lock().await;
        if st.current.is_none() {
            st.current = Some(0);
        }
        st.wallets.push(w);
    }

    // snapshot pour éviter les emprunts prolongés
    let (addr, pubhex) = {
        let st = state.lock().await;
        let w = st.wallets.last().unwrap();
        (w.get_address(hrp), w.encoded_public_key())
    };

    println!("{}", "✅ Wallet généré".green().bold());
    println!("Adresse    : {}", addr.cyan());
    println!(
        "Fingerprint: {}",
        addr.chars().take(14).collect::<String>().bright_black()
    );
    println!("Public     : {}", pubhex.yellow());
    if !name.is_empty() {
        println!("Alias      : {}", name);
    }

    // QR code ASCII
    print_qr(&addr);

    wait_enter();
    Ok(())
}

// 2) Lister wallets
pub async fn action_list_wallets(state: &Arc<Mutex<CliState>>, hrp: &str) -> anyhow::Result<()> {
    let st = state.lock().await;
    if st.wallets.is_empty() {
        println!("{}", "Aucun wallet".bright_black());
        return Ok(());
    }
    for (i, w) in st.wallets.iter().enumerate() {
        let line = format!("{}. {}", i, w.get_address(hrp));
        if Some(i) == st.current {
            println!("{}", line.cyan().bold());
        } else {
            println!("{line}");
        }
    }
    Ok(())
}

// 3) Sélectionner wallet
pub async fn action_select_wallet(state: &Arc<Mutex<CliState>>, hrp: &str) -> anyhow::Result<()> {
    let items: Vec<String>;
    {
        let st = state.lock().await;
        if st.wallets.is_empty() {
            println!("{}", "Aucun wallet à sélectionner".red());
            return Ok(());
        }
        items = st
            .wallets
            .iter()
            .enumerate()
            .map(|(i, w)| format!("{}. {}", i, w.get_address(hrp)))
            .collect();
    }

    let idx = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Sélectionne un wallet")
        .items(&items)
        .default(0)
        .interact()?;

    {
        let mut st = state.lock().await;
        st.current = Some(idx);
        println!(
            "✅ Wallet courant: {}",
            st.wallets[idx].encoded_public_key().green()
        );
    }
    Ok(())
}

// 4) Afficher le courant
pub async fn action_show_current(state: &Arc<Mutex<CliState>>, hrp: &str) -> anyhow::Result<()> {
    // snapshot pour éviter les emprunts prolongés
    let (addr, pubhex) = {
        let st = state.lock().await;
        match st.current.and_then(|i| st.wallets.get(i)) {
            Some(w) => (w.get_address(hrp), w.encoded_public_key()),
            None => {
                println!("{}", "Aucun wallet sélectionné".red());
                return Ok(());
            }
        }
    };

    println!("{}", "Wallet courant".bright_blue().bold());
    println!("Adresse    : {}", addr.cyan());
    println!(
        "Fingerprint: {}",
        addr.chars().take(14).collect::<String>().bright_black()
    );
    println!("Public     : {}", pubhex.yellow());

    // QR code ASCII
    print_qr(&addr);

    wait_enter();
    Ok(())
}

pub async fn action_wallet_balance(
    state: &Arc<Mutex<CliState>>,
    store: &Arc<RocksStore>,
) -> Result<()> {
    let (wallet, hrp) = {
        let st = state.lock().await;
        let w = st
            .current_wallet()
            .ok_or_else(|| anyhow::anyhow!("Aucun wallet sélectionné"))?;
        let settings = load_config()?;
        (w.clone(), settings.address.hrp.clone())
    };

    let bal = wallet.balance(store, &hrp, 1000).await?;
    println!(
        "💰 Balance du wallet {} = {} tokens",
        wallet.short_address(&hrp),
        bal
    );

    wait_enter();
    Ok(())
}
