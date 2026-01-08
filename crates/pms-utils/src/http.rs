use anyhow::Result;
use pms_config::{NetworkMode, Settings, load_config};
use pms_wire::WireBlock;
use reqwest::{Client, StatusCode};

#[derive(Debug, serde::Deserialize)]
pub struct SubmitResp {
    pub id: String,
}

pub async fn submit_block_http_to(
    url: &str,
    wb: &WireBlock,
    allow_insecure: bool,
) -> anyhow::Result<(StatusCode, Option<String>)> {
    // Client HTTP
    let client = Client::builder()
        .danger_accept_invalid_certs(allow_insecure)
        .build()?;

    println!(
        "[CLI][HTTP][REQ] POST {}/submit/block id={} parents={:?}",
        url, wb.id, wb.parents
    );

    let resp = client
        .post(format!("{}/submit/block", url))
        .json(wb)
        .send()
        .await?;

    let code = resp.status();

    let id_opt = if code.is_success() {
        resp.json::<SubmitResp>().await.ok().map(|r| r.id)
    } else {
        None
    };

    // 4) Log humain
    match code {
        StatusCode::CREATED => {
            println!("✅ Soumis au nœud: 201 Created");
        }
        StatusCode::ACCEPTED => {
            println!("⚠️  Soumis au nœud: 202 Accepted (traitement async)");
        }
        StatusCode::CONFLICT => {
            println!("ℹ️  Déjà présent: 409 Conflict");
        }
        StatusCode::FORBIDDEN => {
            println!("⛔ Refusé (route prod désactivée, signature invalide ou réseau incorrect)");
        }
        StatusCode::BAD_REQUEST => {
            println!("❌ Mauvais format (400 Bad Request)");
        }
        _ => {
            println!("❌ Erreur serveur: {}", code);
        }
    }

    Ok((code, id_opt))
}

pub async fn submit_block_http(wb: &WireBlock) -> anyhow::Result<(StatusCode, Option<String>)> {
    let settings = load_config()?;

    let base = settings
        .client
        .as_ref()
        .map(|c| c.api_addr.as_str())
        .unwrap_or("https://127.0.0.1:8080");

    let allow_insecure = settings
        .client
        .as_ref()
        .map(|c| c.allow_insecure_tls)
        .unwrap_or(true);

    submit_block_http_to(base, wb, allow_insecure).await
}

/// Construit un client HTTP(s) adapté au mode réseau.
/// - dev/testnet  → accepte les certificats auto-signés
/// - mainnet      → strict (pas de danger_accept)
pub fn build_http_client(settings: &Settings) -> Result<Client> {
    let mut builder = reqwest::Client::builder();

    match settings.network.mode {
        NetworkMode::Dev | NetworkMode::Testnet => {
            eprintln!(
                "⚠️  TLS: certificats auto-signés acceptés (mode = {:?})",
                settings.network.mode
            );
            builder = builder.danger_accept_invalid_certs(true);
        }
        NetworkMode::Mainnet => {
            eprintln!("🔒 TLS: vérification stricte des certificats (mode = mainnet)");
        }
    }

    Ok(builder.build()?)
}
