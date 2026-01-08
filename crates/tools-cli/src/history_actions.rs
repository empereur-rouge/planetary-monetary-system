use dialoguer::{Input, Select, theme::ColorfulTheme};
use owo_colors::OwoColorize;
use pms_config::{Settings, load_config};
use std::sync::Arc;
#[derive(serde::Deserialize)]
pub struct PageResp<T> {
    pub items: Vec<T>,
    pub next_after_ts: Option<i64>,
    pub next_after_id: Option<String>,
    pub has_more: bool,
}

// We'll use our local definition instead of the imported one for the HTTP case
use crate::helpers::{make_http_client, wait_enter};
use crate::repl::CliState;
use pms_storage::DagStorage;
use pms_storage::rocks_store::store::RocksStore;
use pms_types::{PayloadEnvelope, PlainPayload};
use pms_types_block::Block;
use pms_utils::print_block_full;
use pms_wallet::address_candidates;
use pms_wallet::history::involves_any_address;
use pms_wire::WireBlock;

enum FetchMode {
    Local,
    Http,
}

fn choose_mode(prompt: &str) -> FetchMode {
    let items = ["Local (direct Redis)", "HTTP GET (serveur)"];
    let i = Select::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .items(&items)
        .default(0)
        .interact()
        .unwrap();
    if i == 0 {
        FetchMode::Local
    } else {
        FetchMode::Http
    }
}

fn api_base_from_settings(s: &Settings) -> String {
    s.client
        .as_ref()
        .map(|c| c.api_addr.clone())
        .unwrap_or_else(|| "http://localhost:8080".to_string())
}

pub fn print_wire_page(page: &PageResp<WireBlock>) {
    println!("\n{}", "============ PAGE ============".bold().blue());
    println!(
        "{} {}",
        "items:".bright_black(),
        page.items.len().to_string().bold()
    );
    println!(
        "{} {}",
        "has_more:".bright_black(),
        if page.has_more {
            "true".green().to_string()
        } else {
            "false".red().to_string()
        }
    );
    if let Some(ts) = page.next_after_ts {
        println!(
            "{} {}",
            "next_after_ts:".bright_black(),
            ts.to_string().yellow()
        );
    } else {
        println!(
            "{} {}",
            "next_after_ts:".bright_black(),
            "none".bright_black()
        );
    }
    if let Some(id) = page.next_after_id.as_ref() {
        println!("{} {}", "next_after_id:".bright_black(), id.cyan());
    } else {
        println!(
            "{} {}",
            "next_after_id:".bright_black(),
            "none".bright_black()
        );
    }

    println!(
        "{}",
        "---------------- ITEMS ----------------".bright_black()
    );
    for wb in &page.items {
        let b: Block = wb.clone().into();
        print_block_full(&b);
    }
    println!(
        "{}",
        "======================================\n".bright_black()
    );
}

pub async fn action_stream_blocks(store: &Arc<RocksStore>) -> anyhow::Result<()> {
    let mode = choose_mode("Source des blocs ?");
    let limit: usize = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Limit")
        .default(100)
        .interact_text()?;

    match mode {
        FetchMode::Local => {
            let ids = store.recent_ids(limit).await?;
            let blocks: Vec<WireBlock> = store.get_blocks_by_ids(&ids).await?;

            for wb in blocks {
                let b: Block = wb.into();
                print_block_full(&b);
            }
        }
        FetchMode::Http => {
            let settings = load_config()?;
            let base = api_base_from_settings(&settings);
            let url = format!("{}/blocks/stream", base.trim_end_matches('/'));
            let client = make_http_client(&settings)?;
            let txt = client
                .get(&url)
                .query(&[("limit", limit.to_string())])
                .send()
                .await?
                .error_for_status()?
                .text()
                .await?;

            // côté HTTP, on reçoit du JSON (WireBlock[])
            let blocks: Vec<WireBlock> = serde_json::from_str(&txt)?;
            for wb in blocks {
                let b: Block = wb.into();
                print_block_full(&b);
            }
        }
    }
    wait_enter();
    Ok(())
}

pub async fn action_encrypted_history(store: &Arc<RocksStore>) -> anyhow::Result<()> {
    let mode = choose_mode("Source de l'history ?");

    let after_ts_s: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("after_ts (ms, vide = none)")
        .default(String::new())
        .interact_text()?;
    let after_ts = if after_ts_s.trim().is_empty() {
        None
    } else {
        Some(after_ts_s.parse::<i64>()?)
    };

    let after_id: String = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("after_id (vide = none)")
        .default(String::new())
        .interact_text()?;
    let after_id = if after_id.trim().is_empty() {
        None
    } else {
        Some(after_id)
    };

    let limit: usize = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("limit")
        .default(200)
        .interact_text()?;

    match mode {
        FetchMode::Local => {
            let (ids, next_cursor) = store
                .recent_ids_by_time(after_ts, after_id.clone(), limit)
                .await?;
            let mut items: Vec<WireBlock> = store.get_blocks_by_ids(&ids).await?;

            // ne garder que les blocs Encrypted
            items.retain(|wb| {
                wb.payload_json
                    .as_ref()
                    .and_then(|s| serde_json::from_str::<PayloadEnvelope>(s).ok())
                    .map(|env| matches!(env, PayloadEnvelope::Encrypted(_)))
                    .unwrap_or(false)
            });

            let (next_after_ts, next_after_id, has_more) = next_cursor
                .map(|(ts, id, more)| (Some(ts), Some(id), more))
                .unwrap_or((None, None, false));

            let page = PageResp {
                items,
                next_after_ts,
                next_after_id,
                has_more,
            };

            print_wire_page(&page);
        }
        FetchMode::Http => {
            let settings = load_config()?;
            let base = api_base_from_settings(&settings);
            let url = format!("{}/v1/history", base.trim_end_matches('/'));

            #[derive(serde::Serialize)]
            struct Q<'a> {
                #[serde(skip_serializing_if = "Option::is_none")]
                after_ts: Option<i64>,
                #[serde(skip_serializing_if = "Option::is_none")]
                after_id: Option<&'a str>,
                limit: usize,
            }

            let client = make_http_client(&settings)?;
            let page: PageResp<WireBlock> = client
                .get(&url)
                .query(&Q {
                    after_ts,
                    after_id: after_id.as_deref(),
                    limit,
                })
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            print_wire_page(&page);
        }
    }

    wait_enter();
    Ok(())
}

pub async fn action_wallet_history(
    state: &Arc<tokio::sync::Mutex<CliState>>,
    store: &Arc<RocksStore>,
) -> anyhow::Result<()> {
    // 1) Wallet courant → on fabrique les candidats d’adresse
    let (candidates, sk_hex, shown_addr) = {
        let st = state.lock().await;
        let wallet = st.current_wallet();
        let w = wallet
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Aucun wallet sélectionné"))?;

        let settings = load_config()?; // pour hrp
        let hrp = settings.address.hrp.clone();

        // génère toutes les formes possibles
        let candidates = address_candidates(&hrp, &w.public_key_hex, &w.x25519_pub_hex);
        let sk_hex = w.x25519_sk_hex(); // Option<String>
        // pour l’en-tête d’affichage, on prend la forme bech32 si présente, sinon la 1ère
        let shown_addr = candidates
            .iter()
            .find(|s| s.starts_with(&hrp)) // bech32m commence par HRP
            .cloned()
            .unwrap_or_else(|| candidates.first().cloned().unwrap_or_default());

        (candidates, sk_hex, shown_addr)
    };

    let limit: usize = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("limit par page")
        .default(200)
        .interact_text()?;

    let mut after_ts: Option<i64> = None;
    let mut after_id: Option<String> = None;

    loop {
        let (ids, next) = store
            .recent_ids_by_time(after_ts, after_id.clone(), limit)
            .await?;
        if ids.is_empty() {
            println!("(aucun bloc)");
            break;
        }

        let blocks: Vec<WireBlock> = store.get_blocks_by_ids(&ids).await?;
        // Map (id -> ts) pour tri stable; utilise ta fonction zmscore_by_time (HashMap)
        let id_ts = store.ts_for_ids(&ids).await?;

        let mut hits: Vec<(i64, String, PlainPayload)> = Vec::new();
        let mut dec_ok = 0usize;

        for b in blocks {
            let ts = *id_ts.get(&b.id).unwrap_or(&0);
            if let Some(s) = &b.payload_json {
                if let Ok(PayloadEnvelope::Encrypted(enc)) =
                    serde_json::from_str::<PayloadEnvelope>(s)
                {
                    if let Some(sk) = sk_hex.as_ref() {
                        if let Ok(plain) = enc.decrypt_as_payload(sk) {
                            dec_ok += 1;
                            if involves_any_address(&plain, &candidates) {
                                hits.push((ts, b.id.clone(), plain));
                            }
                        }
                    }
                }
            }
        }

        // Tri (ts desc, id desc)
        hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));

        println!("\n=== Transactions pour {} ===", shown_addr.cyan());
        println!("(candidats: {})", candidates.len());
        if hits.is_empty() {
            println!("(0 match). Déchiffrés OK: {} / page={}.", dec_ok, ids.len());
        } else {
            for (ts, id, p) in &hits {
                println!(
                    "{}",
                    "----------------------------------------".bright_black()
                );
                println!("ts: {}  id: {}", ts, id.cyan());
                if let Ok(js) = serde_json::to_string_pretty(p) {
                    println!("{js}");
                } else {
                    println!("{:#?}", p);
                }
            }
            println!(
                "{}",
                "----------------------------------------".bright_black()
            );
        }

        if let Some((ts, id, has_more)) = next {
            after_ts = Some(ts);
            after_id = Some(id);
            if !has_more {
                break;
            }
            let cont: String = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("Continuer ? (o/N)")
                .default(String::new())
                .interact_text()?;
            if cont.trim().to_lowercase() != "o" {
                break;
            }
        } else {
            break;
        }
    }

    wait_enter();
    Ok(())
}
