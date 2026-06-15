use anyhow::Result;
use pms_config::{NetworkMode, Settings, load_config};
use pms_types::Transaction;
use pms_wire::WireBlock;
use reqwest::{Client, StatusCode};

#[derive(Debug, serde::Deserialize)]
pub struct SubmitResp {
    pub id: String,
}

/// Corps de requête de `POST /wallet/tx/send` : une transaction déjà signée par
/// l'utilisateur (unlocks remplis) + les clés X25519 des destinataires pour le
/// chiffrement du payload côté coordinateur.
#[derive(Debug, serde::Serialize)]
struct WalletSendTxBody<'a> {
    tx: &'a Transaction,
    recipients_xpk: &'a [String],
}

/// Réponse de `POST /wallet/tx/send`. Tous les champs sont optionnels car la
/// forme varie selon le statut (`{id,status}` en succès, `{status,reason}` ou
/// `{error}` en échec).
#[derive(Debug, serde::Deserialize)]
pub struct SendTxResp {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
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

/// Résout l'URL de base du nœud + le mode TLS depuis la config `[client]`
/// (`api_addr` / `allow_insecure_tls`). Partagé par [`submit_block_http`] et
/// [`send_tx_http`] pour éviter la duplication de cette résolution.
fn resolve_node_base_and_tls(settings: &Settings) -> (String, bool) {
    let raw_addr = settings
        .client
        .as_ref()
        .map(|c| c.api_addr.as_str())
        .unwrap_or("127.0.0.1:8080");

    let base = if raw_addr.contains("://") {
        raw_addr.to_string()
    } else {
        format!("https://{}", raw_addr)
    };

    let allow_insecure = settings
        .client
        .as_ref()
        .map(|c| c.allow_insecure_tls)
        .unwrap_or(true);

    (base, allow_insecure)
}

pub async fn submit_block_http(wb: &WireBlock) -> anyhow::Result<(StatusCode, Option<String>)> {
    let settings = load_config()?;
    let (base, allow_insecure) = resolve_node_base_and_tls(&settings);
    submit_block_http_to(&base, wb, allow_insecure).await
}

/// Soumet une transaction **déjà signée par l'utilisateur** au coordinateur via
/// `POST {url}/wallet/tx/send`.
///
/// C'est la voie **non-custodiale** : la clé privée de l'utilisateur ne quitte
/// jamais l'appelant — seule la `tx` signée (unlocks remplis) part sur le
/// réseau. Le coordinateur vérifie les signatures d'inputs de l'utilisateur
/// (autorisation de dépense, audit C-1/C-2), chiffre le payload pour les
/// `recipients_xpk`, puis emballe la tx dans un bloc **qu'il signe lui-même**.
///
/// À l'inverse de [`submit_block_http`] (qui soumet un bloc déjà signé à
/// `/submit/block`), cette voie ne demande PAS à l'utilisateur de signer un
/// bloc : un bloc signé par la clé de l'utilisateur serait rejeté par le
/// `single_writer_gate` du nœud (« signer is not in the active coordinator key
/// set ») dès que `enforce_single_writer` est actif (testnet/mainnet).
///
/// Retourne `(status_http, id_du_bloc)` — `id` est `Some` seulement si le
/// coordinateur a accepté et créé le bloc.
pub async fn send_tx_http_to(
    url: &str,
    tx: &Transaction,
    recipients_xpk: &[String],
    allow_insecure: bool,
) -> anyhow::Result<(StatusCode, Option<String>)> {
    let client = Client::builder()
        .danger_accept_invalid_certs(allow_insecure)
        .build()?;

    let body = WalletSendTxBody { tx, recipients_xpk };

    println!(
        "[CLI][HTTP][REQ] POST {}/wallet/tx/send inputs={} outputs={} recipients_xpk={}",
        url,
        tx.inputs.len(),
        tx.outputs.len(),
        recipients_xpk.len()
    );

    let resp = client
        .post(format!("{}/wallet/tx/send", url))
        .json(&body)
        .send()
        .await?;

    let code = resp.status();
    let parsed = resp.json::<SendTxResp>().await.ok();
    let id_opt = parsed.as_ref().and_then(|r| r.id.clone());

    match code {
        StatusCode::CREATED => {
            println!(
                "✅ Tx emballée par le coordinateur: 201 Created (id={})",
                id_opt.as_deref().unwrap_or("?")
            );
        }
        StatusCode::CONFLICT => {
            println!("ℹ️  Tx déjà présente: 409 Conflict");
        }
        StatusCode::UNAUTHORIZED => {
            println!("⛔ Autorisation de transaction invalide (401) — signature/ownership des inputs");
        }
        StatusCode::PAYMENT_REQUIRED => {
            println!("❌ Gas pool insuffisant (402)");
        }
        StatusCode::BAD_REQUEST => {
            let detail = parsed
                .as_ref()
                .and_then(|r| r.reason.clone().or_else(|| r.error.clone()))
                .unwrap_or_else(|| "bad request".to_string());
            println!("❌ Tx rejetée (400): {detail}");
        }
        _ => {
            println!("❌ Erreur serveur: {code}");
        }
    }

    Ok((code, id_opt))
}

/// Variante de [`send_tx_http_to`] qui résout l'URL du nœud et le mode TLS
/// depuis la config (`[client].api_addr` / `allow_insecure_tls`), à l'image de
/// [`submit_block_http`]. C'est l'entrée utilisée par le CLI headless.
pub async fn send_tx_http(
    tx: &Transaction,
    recipients_xpk: &[String],
) -> anyhow::Result<(StatusCode, Option<String>)> {
    let settings = load_config()?;
    let (base, allow_insecure) = resolve_node_base_and_tls(&settings);
    send_tx_http_to(&base, tx, recipients_xpk, allow_insecure).await
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
